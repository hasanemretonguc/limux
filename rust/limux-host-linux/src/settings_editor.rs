use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use crate::app_config::{
    AiTabTitleBackend, AppConfig, ColorScheme, LinkOpenDestination, NotificationSound, UiScale,
    UiTheme, UnreadColor, WindowControlsSide, DEFAULT_UI_SCALE, MAX_TAB_ACTIVITY_SECONDS,
    MIN_TAB_ACTIVITY_SECONDS,
};
use crate::gui_hooks::{GuiHook, GuiHookEvent, GuiHooksConfig};
use crate::keybind_editor;
use crate::shortcut_config::{NormalizedShortcut, ResolvedShortcutConfig, ShortcutId};

pub const SETTINGS_CSS: &str = r#"
.limux-settings-window {
    background-color: @window_bg_color;
    color: @window_fg_color;
}
"#;

const MIN_UI_SCALE_PERCENT: f64 = 80.0;
const MAX_UI_SCALE_PERCENT: f64 = 200.0;
const UI_SCALE_STEP_PERCENT: f64 = 5.0;

#[derive(Clone, Copy, Debug)]
struct UiScaleDescriptor {
    selector: &'static str,
    property: &'static str,
    base_size: f32,
}

const UI_SCALE_DESCRIPTORS: &[UiScaleDescriptor] = &[
    UiScaleDescriptor {
        selector: ".limux-ws-name",
        property: "font-size",
        base_size: 15.0,
    },
    UiScaleDescriptor {
        selector: ".limux-ws-star-btn",
        property: "font-size",
        base_size: 22.0,
    },
    UiScaleDescriptor {
        selector: ".limux-notify-dot, .limux-notify-dot-hidden",
        property: "font-size",
        base_size: 10.0,
    },
    UiScaleDescriptor {
        selector: ".limux-notify-msg, .limux-notify-msg-unread",
        property: "font-size",
        base_size: 11.0,
    },
    UiScaleDescriptor {
        selector: ".limux-sidebar-title",
        property: "font-size",
        base_size: 11.0,
    },
    UiScaleDescriptor {
        selector: ".limux-ws-path",
        property: "font-size",
        base_size: 12.0,
    },
    UiScaleDescriptor {
        selector: ".limux-tab",
        property: "font-size",
        base_size: 12.0,
    },
    UiScaleDescriptor {
        selector: ".limux-pin-icon",
        property: "font-size",
        base_size: 9.0,
    },
    UiScaleDescriptor {
        selector: ".limux-tab-rename-entry",
        property: "font-size",
        base_size: 12.0,
    },
    UiScaleDescriptor {
        selector: ".limux-browser-url-entry, .limux-browser-search-entry",
        property: "font-size",
        base_size: 12.0,
    },
    UiScaleDescriptor {
        selector: ".limux-keybind-hint, .limux-keybind-default, .limux-keybind-error, .limux-keybind-row-hint",
        property: "font-size",
        base_size: 12.0,
    },
    UiScaleDescriptor {
        selector: ".limux-pane-action image",
        property: "-gtk-icon-size",
        base_size: 16.0,
    },
    UiScaleDescriptor {
        selector: ".limux-tab-close image",
        property: "-gtk-icon-size",
        base_size: 12.0,
    },
    UiScaleDescriptor {
        selector: ".limux-ai-usage-badge",
        property: "font-size",
        base_size: 11.0,
    },
    UiScaleDescriptor {
        selector: ".limux-project-badge, .limux-project-tag",
        property: "font-size",
        base_size: 10.0,
    },
    UiScaleDescriptor {
        selector: ".limux-project-name",
        property: "font-size",
        base_size: 13.0,
    },
    UiScaleDescriptor {
        selector: ".limux-project-path",
        property: "font-size",
        base_size: 11.0,
    },
    UiScaleDescriptor {
        selector: ".limux-indicator-pill",
        property: "font-size",
        base_size: 12.0,
    },
    UiScaleDescriptor {
        selector: ".limux-sidebar-btn",
        property: "font-size",
        base_size: 16.0,
    },
];

pub fn sanitize_css_font_family(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | ';' | '{' | '}' | '\n' | '\r' | '\\'))
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn collect_system_font_families(widget: &impl IsA<gtk::Widget>) -> Vec<String> {
    let mut families: Vec<String> = widget
        .pango_context()
        .list_families()
        .into_iter()
        .map(|family| family.name().to_string())
        .filter(|name| !name.trim().is_empty())
        .collect();
    families.sort_by_key(|a| a.to_ascii_lowercase());
    families.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    families
}

fn searchable_font_dropdown(options: &[&str]) -> gtk::DropDown {
    let dropdown = gtk::DropDown::from_strings(options);
    dropdown.set_enable_search(true);
    let expr = gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        gtk::Expression::NONE,
        "string",
    );
    dropdown.set_expression(Some(&expr));
    dropdown.set_valign(gtk::Align::Center);
    dropdown
}

fn ghostty_config_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("ghostty/config"))
}

fn read_ghostty_font_family() -> Option<String> {
    let path = ghostty_config_path()?;
    let contents = std::fs::read_to_string(path).ok()?;
    let raw = crate::ghostty_config::read_ghostty_value(&contents, "font-family")?;
    let trimmed = raw.trim().trim_matches(['"', '\'']).trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn detect_omarchy_font_family() -> Option<String> {
    if let Ok(output) = std::process::Command::new("omarchy-font-current").output() {
        if output.status.success() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    read_ghostty_font_family()
}

fn write_ghostty_font_family(font_family: &str) -> Result<(), String> {
    let Some(clean) = sanitize_css_font_family(font_family) else {
        return Err("invalid font family".to_string());
    };
    let Some(path) = ghostty_config_path() else {
        return Err("config directory unavailable".to_string());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let replacement_line = format!("font-family = \"{clean}\"");
    let mut replaced = false;
    let mut lines: Vec<String> = existing
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("font-family")
                && trimmed["font-family".len()..].trim_start().starts_with('=')
            {
                replaced = true;
                replacement_line.clone()
            } else {
                line.to_string()
            }
        })
        .collect();
    if !replaced {
        lines.push(replacement_line);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    std::fs::write(&path, out).map_err(|err| err.to_string())
}

/// High-priority CSS for interface scale and custom UI font family so user
/// GTK overrides (e.g. `~/.config/gtk-4.0/gtk.css`) cannot block Limux's
/// in-app scale and font settings.
pub fn ui_scale_and_font_override_css(config: &AppConfig) -> String {
    let scale = config.appearance.ui_scale.get();
    let mut rules = Vec::new();
    if let Some(font) = config
        .appearance
        .ui_font_family
        .as_deref()
        .and_then(sanitize_css_font_family)
    {
        rules.push(format!(
            "window, window * {{ font-family: \"{font}\", sans-serif; }}"
        ));
    }
    if !config.appearance.ui_scale.is_default() {
        rules.push(format!(
            ".limux-settings-window, .limux-sidebar, .limux-pane-header, .limux-top-bar {{ font-size: {:.3}px; }}",
            12.0 * scale
        ));
    }
    for descriptor in UI_SCALE_DESCRIPTORS {
        rules.push(format!(
            "{} {{ {}: {:.3}px; }}",
            descriptor.selector,
            descriptor.property,
            descriptor.base_size * scale
        ));
    }
    rules.join("\n")
}

pub fn ui_scale_css(config: &AppConfig) -> String {
    let scale = config.appearance.ui_scale.get();
    let mut rules: Vec<String> = UI_SCALE_DESCRIPTORS
        .iter()
        .map(|descriptor| {
            format!(
                "{} {{ {}: {:.3}px; }}",
                descriptor.selector,
                descriptor.property,
                descriptor.base_size * scale
            )
        })
        .collect();
    let c = config.appearance.unread_color.css_color();
    rules.push(format!(
        ".limux-indicator-unread-dot, .limux-notify-dot, .limux-tab-unread-dot {{ color: {c}; }}\n\
         .limux-notify-msg-unread {{ color: alpha({c}, 0.9); }}\n\
         .limux-sidebar-row-unread {{ background-color: alpha({c}, 0.14); border-left: 3px solid {c}; }}\n\
         .limux-sidebar-list row:selected .limux-sidebar-row-box.limux-sidebar-row-unread {{ background: alpha({c}, 0.18); border-left: 3px solid {c}; }}"
    ));
    rules.join("\n")
}

type OnConfigChanged = dyn Fn(&AppConfig, &AppConfig);

pub struct SettingsEditorInput {
    pub config: Rc<RefCell<AppConfig>>,
    pub shortcuts: Rc<ResolvedShortcutConfig>,
    pub on_capture: Rc<
        dyn Fn(ShortcutId, Option<NormalizedShortcut>) -> Result<ResolvedShortcutConfig, String>,
    >,
    pub on_config_changed: Rc<OnConfigChanged>,
}

pub fn present_settings_dialog(parent: &impl IsA<gtk::Widget>, input: SettingsEditorInput) {
    let window = adw::Window::new();
    window.set_title(Some("Settings"));
    window.set_default_size(760, 680);
    window.set_modal(true);

    if let Some(parent_window) = parent
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
    {
        window.set_transient_for(Some(&parent_window));
        if let Some(app) = parent_window.application() {
            window.set_application(Some(&app));
        }
    }

    let content = build_settings_window_content(&window, input);
    window.set_content(Some(&content));
    window.present();
}

fn apply_config_change<F, G>(
    config: &Rc<RefCell<AppConfig>>,
    on_changed: &F,
    update: G,
) -> AppConfig
where
    F: Fn(&AppConfig, &AppConfig) + ?Sized,
    G: FnOnce(&mut AppConfig),
{
    let (previous, updated) = {
        let mut config_ref = config.borrow_mut();
        let previous = config_ref.clone();
        update(&mut config_ref);
        let updated = config_ref.clone();
        (previous, updated)
    };
    on_changed(&previous, &updated);
    config.borrow().clone()
}

fn build_settings_window_content(window: &adw::Window, input: SettingsEditorInput) -> gtk::Widget {
    let stack = adw::ViewStack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    let general_page = build_general_page(&input);
    let general_stack_page = stack.add_titled(&general_page, Some("general"), "General");
    general_stack_page.set_icon_name(Some("preferences-system-symbolic"));

    let hooks_page = build_hooks_page(&input);
    let hooks_stack_page = stack.add_titled(&hooks_page, Some("hooks"), "Hooks");
    hooks_stack_page.set_icon_name(Some("system-run-symbolic"));

    let notifications_page = build_notifications_page(&input);
    let notifications_stack_page =
        stack.add_titled(&notifications_page, Some("notifications"), "Notifications");
    notifications_stack_page.set_icon_name(Some("preferences-system-notifications-symbolic"));

    let keybinds_page = keybind_editor::build_keybind_editor(&input.shortcuts, input.on_capture);
    let keybinds_stack_page = stack.add_titled(&keybinds_page, Some("keybindings"), "Keybindings");
    keybinds_stack_page.set_icon_name(Some("input-keyboard-symbolic"));

    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();

    let close_button = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Close settings")
        .valign(gtk::Align::Center)
        .build();
    close_button.add_css_class("flat");

    {
        let window = window.clone();
        close_button.connect_clicked(move |_| {
            window.close();
        });
    }

    let header_bar = adw::HeaderBar::new();
    header_bar.set_show_start_title_buttons(false);
    header_bar.set_show_end_title_buttons(false);
    header_bar.set_title_widget(Some(&switcher));
    header_bar.pack_end(&close_button);

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.add_css_class("limux-settings-window");
    outer.append(&header_bar);
    outer.append(&stack);
    outer.upcast()
}

fn build_hooks_page(input: &SettingsEditorInput) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    page.set_title("Hooks");
    page.set_name(Some("hooks"));
    page.set_icon_name(Some("system-run-symbolic"));
    page.set_hexpand(true);
    page.set_vexpand(true);
    page.add(&build_gui_hooks_group(input));
    page.upcast()
}

fn build_gui_hooks_group(input: &SettingsEditorInput) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title("GUI hooks");
    group.set_description(Some(
        "Run a command when you create or delete a workspace from the GUI: \
         an \"On\" hook runs before Limux does it and can take it over, an \
         \"After\" hook runs once Limux has done it. A hook that takes the \
         action over does its own follow-up: requests made through the limux \
         CLI or socket never run hooks. The command runs in bash with \
         LIMUX_HOOK, LIMUX_SOCKET, LIMUX_WORKSPACE_NAME and, when known, \
         LIMUX_WORKSPACE_ID, LIMUX_WORKSPACE_FOLDER, LIMUX_GROUP_ID and \
         LIMUX_WORKSPACE_COMMAND. Closing a workspace's last tab also runs the \
         delete hooks, but cannot be skipped.",
    ));
    for event in GuiHookEvent::ALL {
        group.add(&build_gui_hook_row(input, event));
    }
    group
}

fn gui_hook_subtitle(hook: &GuiHook) -> String {
    match (hook.command.trim(), hook.enabled) {
        ("", _) => "Not set".to_string(),
        (command, true) => command.to_string(),
        (command, false) => format!("Disabled: {command}"),
    }
}

/// The widgets of one hook row, so a change made over the socket can be
/// shown in an open Settings window.
struct GuiHookRowWidgets {
    event: GuiHookEvent,
    expander: gtk::glib::WeakRef<adw::ExpanderRow>,
    enabled: gtk::glib::WeakRef<gtk::Switch>,
    command: gtk::glib::WeakRef<gtk::Entry>,
    show_terminal: gtk::glib::WeakRef<gtk::Switch>,
    /// `None` for an after-* hook, which cannot skip Limux's action.
    suppress_default: Option<gtk::glib::WeakRef<gtk::Switch>>,
}

impl GuiHookRowWidgets {
    fn show(&self, hook: &GuiHook) -> Option<()> {
        self.expander
            .upgrade()?
            .set_subtitle(&gui_hook_subtitle(hook));
        self.enabled.upgrade()?.set_active(hook.enabled);
        let command = self.command.upgrade()?;
        // Rewriting equal text would move the cursor while the user types.
        if command.text().trim() != hook.command {
            command.set_text(&hook.command);
        }
        self.show_terminal.upgrade()?.set_active(hook.show_terminal);
        if let Some(suppress_default) = &self.suppress_default {
            suppress_default
                .upgrade()?
                .set_active(hook.suppress_default);
        }
        Some(())
    }
}

thread_local! {
    static GUI_HOOK_ROWS: RefCell<Vec<GuiHookRowWidgets>> = const { RefCell::new(Vec::new()) };
    static REFRESHING_GUI_HOOK_ROWS: Cell<bool> = const { Cell::new(false) };
}

struct GuiHookRefreshGuard(bool);

impl GuiHookRefreshGuard {
    fn new() -> Self {
        Self(REFRESHING_GUI_HOOK_ROWS.with(|refreshing| refreshing.replace(true)))
    }
}

impl Drop for GuiHookRefreshGuard {
    fn drop(&mut self) {
        REFRESHING_GUI_HOOK_ROWS.with(|refreshing| refreshing.set(self.0));
    }
}

/// Show `hooks` in any open Settings window. The row handlers see that the
/// config already holds these values, so nothing is saved again. GTK emits
/// entry changes synchronously while setting text; suppress those callbacks
/// until every row reflects the effective config.
pub fn refresh_gui_hook_rows(hooks: &GuiHooksConfig) {
    let _guard = GuiHookRefreshGuard::new();
    GUI_HOOK_ROWS.with(|rows| {
        let mut rows = rows.borrow_mut();
        rows.retain(|row| row.expander.upgrade().is_some());
        for row in rows.iter() {
            row.show(hooks.get(row.event));
        }
    });
}

fn hook_switch_row(title: &str, subtitle: &str, active: bool) -> (adw::ActionRow, gtk::Switch) {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    // Wrap the whole explanation rather than cut it off.
    row.set_subtitle_lines(0);
    let switch = gtk::Switch::new();
    switch.set_active(active);
    switch.set_valign(gtk::Align::Center);
    row.add_suffix(&switch);
    row.set_activatable_widget(Some(&switch));
    (row, switch)
}

/// Save a change to `event`'s hook, unless the hook already has it.
fn connect_gui_hook_field<T: PartialEq + 'static>(
    input: &SettingsEditorInput,
    event: GuiHookEvent,
    field: fn(&mut GuiHook) -> &mut T,
) -> impl Fn(T) + 'static {
    let config = input.config.clone();
    let on_changed = input.on_config_changed.clone();
    move |value: T| {
        if REFRESHING_GUI_HOOK_ROWS.with(Cell::get) {
            return;
        }
        if *field(config.borrow_mut().hooks.get_mut(event)) == value {
            return;
        }
        let effective = apply_config_change(&config, &*on_changed, move |c| {
            *field(c.hooks.get_mut(event)) = value;
        });
        refresh_gui_hook_rows(&effective.hooks);
    }
}

fn build_gui_hook_row(input: &SettingsEditorInput, event: GuiHookEvent) -> adw::ExpanderRow {
    let hook = input.config.borrow().hooks.get(event).clone();
    let expander = adw::ExpanderRow::builder()
        .title(event.title())
        .subtitle(gui_hook_subtitle(&hook))
        .build();

    let (enabled_row, enabled_switch) = hook_switch_row(
        "Enabled",
        "Turn the hook off without losing its command",
        hook.enabled,
    );
    expander.add_row(&enabled_row);

    let command_row = adw::ActionRow::builder().title("Command").build();
    let command_entry = gtk::Entry::builder()
        .text(hook.command.as_str())
        .placeholder_text("e.g. ~/bin/workspace-hook.sh --force")
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    command_row.add_suffix(&command_entry);
    expander.add_row(&command_row);

    let (terminal_row, terminal_switch) = hook_switch_row(
        "Show in a terminal",
        "Run the command in a pop-up terminal you can type into",
        hook.show_terminal,
    );
    expander.add_row(&terminal_row);

    let suppress_switch = event.can_skip_default().then(|| {
        let (suppress_row, suppress_switch) = hook_switch_row(
            "Skip the default action",
            "Let the command create or delete the workspace itself, e.g. with the limux CLI. \
             Limux's folder prompt is skipped too, and the matching After hook does not run.",
            hook.suppress_default,
        );
        expander.add_row(&suppress_row);
        suppress_switch
    });

    let set_enabled = connect_gui_hook_field(input, event, |hook| &mut hook.enabled);
    enabled_switch.connect_active_notify(move |switch| set_enabled(switch.is_active()));
    let set_command = connect_gui_hook_field(input, event, |hook| &mut hook.command);
    command_entry.connect_text_notify(move |entry| set_command(entry.text().trim().to_string()));
    let set_show_terminal = connect_gui_hook_field(input, event, |hook| &mut hook.show_terminal);
    terminal_switch.connect_active_notify(move |switch| set_show_terminal(switch.is_active()));
    if let Some(suppress_switch) = &suppress_switch {
        let set_suppress_default =
            connect_gui_hook_field(input, event, |hook| &mut hook.suppress_default);
        suppress_switch
            .connect_active_notify(move |switch| set_suppress_default(switch.is_active()));
    }

    GUI_HOOK_ROWS.with(|rows| {
        rows.borrow_mut().push(GuiHookRowWidgets {
            event,
            expander: expander.downgrade(),
            enabled: enabled_switch.downgrade(),
            command: command_entry.downgrade(),
            show_terminal: terminal_switch.downgrade(),
            suppress_default: suppress_switch.as_ref().map(|switch| switch.downgrade()),
        });
    });
    expander
}

fn build_general_page(input: &SettingsEditorInput) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    page.set_title("General");
    page.set_name(Some("general"));
    page.set_icon_name(Some("preferences-system-symbolic"));
    page.set_hexpand(true);
    page.set_vexpand(true);

    let group = adw::PreferencesGroup::new();

    let theme_row = adw::ActionRow::builder()
        .title("Interface theme")
        .subtitle(
            "Choose the Limux UI style (Omarchy follows your active Omarchy theme colors automatically)",
        )
        .build();
    theme_row.set_title_lines(1);
    theme_row.set_subtitle_lines(2);
    let theme_dropdown = gtk::DropDown::from_strings(UiTheme::labels());
    theme_dropdown.set_selected(input.config.borrow().appearance.ui_theme.dropdown_index());
    theme_dropdown.set_valign(gtk::Align::Center);
    theme_row.add_suffix(&theme_dropdown);
    theme_row.set_activatable_widget(Some(&theme_dropdown));
    group.add(&theme_row);

    let color_row = adw::ActionRow::builder()
        .title("GTK color scheme")
        .subtitle("Choose whether the GTK interface follows system, dark, or light")
        .build();
    color_row.set_title_lines(1);
    color_row.set_subtitle_lines(2);
    let color_dropdown = gtk::DropDown::from_strings(&["System", "Dark", "Light"]);
    let initial_scheme = input.config.borrow().appearance.color_scheme;
    color_dropdown.set_selected(match initial_scheme {
        ColorScheme::System => 0,
        ColorScheme::Dark => 1,
        ColorScheme::Light => 2,
    });
    color_dropdown.set_valign(gtk::Align::Center);
    color_row.add_suffix(&color_dropdown);
    color_row.set_activatable_widget(Some(&color_dropdown));
    group.add(&color_row);

    let ghostty_row = adw::ActionRow::builder()
        .title("Ghostty color scheme")
        .subtitle("Choose whether terminal surfaces follow system, dark, or light")
        .build();
    ghostty_row.set_title_lines(1);
    ghostty_row.set_subtitle_lines(2);
    let ghostty_dropdown = gtk::DropDown::from_strings(&["System", "Dark", "Light"]);
    let initial_ghostty_scheme = input.config.borrow().appearance.ghostty_color_scheme;
    ghostty_dropdown.set_selected(match initial_ghostty_scheme {
        ColorScheme::System => 0,
        ColorScheme::Dark => 1,
        ColorScheme::Light => 2,
    });
    ghostty_dropdown.set_valign(gtk::Align::Center);
    ghostty_row.add_suffix(&ghostty_dropdown);
    ghostty_row.set_activatable_widget(Some(&ghostty_dropdown));
    group.add(&ghostty_row);

    let unread_color_row = adw::ActionRow::builder()
        .title("Unread indicator color")
        .subtitle(
            "Accent color for unread workspace rows, borders, and tab dots (distinct from blue selection)",
        )
        .build();
    unread_color_row.set_title_lines(1);
    unread_color_row.set_subtitle_lines(2);
    let unread_color_dropdown = gtk::DropDown::from_strings(UnreadColor::labels());
    unread_color_dropdown.set_selected(
        input
            .config
            .borrow()
            .appearance
            .unread_color
            .dropdown_index(),
    );
    unread_color_dropdown.set_valign(gtk::Align::Center);
    unread_color_row.add_suffix(&unread_color_dropdown);
    unread_color_row.set_activatable_widget(Some(&unread_color_dropdown));
    group.add(&unread_color_row);

    let hover_row = adw::ActionRow::builder()
        .title("Hover terminal focus")
        .subtitle("Focus terminal panes when the mouse pointer enters them")
        .build();
    hover_row.set_title_lines(1);
    hover_row.set_subtitle_lines(2);
    let hover_switch = gtk::Switch::new();
    hover_switch.set_active(input.config.borrow().focus.hover_terminal_focus);
    hover_switch.set_valign(gtk::Align::Center);
    hover_row.add_suffix(&hover_switch);
    hover_row.set_activatable_widget(Some(&hover_switch));
    group.add(&hover_row);

    let workspace_path_row = adw::ActionRow::builder()
        .title("Show workspace path")
        .subtitle("Display workspace folder below its name")
        .build();
    workspace_path_row.set_title_lines(1);
    workspace_path_row.set_subtitle_lines(2);
    let workspace_path_switch = gtk::Switch::new();
    workspace_path_switch.set_active(input.config.borrow().appearance.show_workspace_path);
    workspace_path_switch.set_valign(gtk::Align::Center);
    workspace_path_row.add_suffix(&workspace_path_switch);
    workspace_path_row.set_activatable_widget(Some(&workspace_path_switch));
    group.add(&workspace_path_row);

    let keep_workspace_open_row = adw::ActionRow::builder()
        .title("Keep workspace open")
        .subtitle("Keep the workspace available after its final terminal exits")
        .build();
    keep_workspace_open_row.set_title_lines(1);
    keep_workspace_open_row.set_subtitle_lines(2);
    let keep_workspace_open_switch = gtk::Switch::new();
    keep_workspace_open_switch.set_active(
        input
            .config
            .borrow()
            .workspace
            .keep_open_after_last_terminal_closes,
    );
    keep_workspace_open_switch.set_valign(gtk::Align::Center);
    keep_workspace_open_row.add_suffix(&keep_workspace_open_switch);
    keep_workspace_open_row.set_activatable_widget(Some(&keep_workspace_open_switch));
    group.add(&keep_workspace_open_row);

    let prompt_workspace_folder_row = adw::ActionRow::builder()
        .title("Ask for folder on new workspace")
        .subtitle("Prompt for a folder address when creating a new workspace")
        .build();
    prompt_workspace_folder_row.set_title_lines(1);
    prompt_workspace_folder_row.set_subtitle_lines(2);
    let prompt_workspace_folder_switch = gtk::Switch::new();
    prompt_workspace_folder_switch
        .set_active(input.config.borrow().workspace.prompt_for_folder_on_create);
    prompt_workspace_folder_switch.set_valign(gtk::Align::Center);
    prompt_workspace_folder_row.add_suffix(&prompt_workspace_folder_switch);
    prompt_workspace_folder_row.set_activatable_widget(Some(&prompt_workspace_folder_switch));
    group.add(&prompt_workspace_folder_row);

    let auto_copy_row = adw::ActionRow::builder()
        .title("Copy selection automatically")
        .subtitle("Copy selected terminal text to the regular clipboard")
        .build();
    auto_copy_row.set_title_lines(1);
    auto_copy_row.set_subtitle_lines(2);
    let auto_copy_switch = gtk::Switch::new();
    auto_copy_switch.set_active(input.config.borrow().clipboard.copy_selection_to_clipboard);
    auto_copy_switch.set_valign(gtk::Align::Center);
    auto_copy_row.add_suffix(&auto_copy_switch);
    auto_copy_row.set_activatable_widget(Some(&auto_copy_switch));
    group.add(&auto_copy_row);

    let link_destination_row = adw::ActionRow::builder()
        .title("Open links in")
        .subtitle("Choose whether terminal links open outside Limux or in a browser tab")
        .build();
    link_destination_row.set_title_lines(1);
    link_destination_row.set_subtitle_lines(2);
    let link_destination_dropdown =
        gtk::DropDown::from_strings(&["Default browser", "Browser tab"]);
    link_destination_dropdown.set_selected(
        input
            .config
            .borrow()
            .links
            .open_destination
            .effective(cfg!(feature = "webkit"))
            .dropdown_index(),
    );
    link_destination_dropdown.set_sensitive(cfg!(feature = "webkit"));
    link_destination_dropdown.set_valign(gtk::Align::Center);
    link_destination_row.add_suffix(&link_destination_dropdown);
    link_destination_row.set_activatable_widget(Some(&link_destination_dropdown));
    group.add(&link_destination_row);

    let scale_row = adw::ActionRow::builder()
        .title("Interface scale")
        .subtitle("Scale Limux sidebar, pane header, and settings text")
        .build();
    scale_row.set_title_lines(1);
    scale_row.set_subtitle_lines(2);
    let scale = input.config.borrow().appearance.ui_scale.get();
    let scale_adjustment = gtk::Adjustment::new(
        f64::from(scale) * 100.0,
        MIN_UI_SCALE_PERCENT,
        MAX_UI_SCALE_PERCENT,
        UI_SCALE_STEP_PERCENT,
        UI_SCALE_STEP_PERCENT * 2.0,
        0.0,
    );
    let scale_spin = gtk::SpinButton::builder()
        .adjustment(&scale_adjustment)
        .digits(0)
        .numeric(true)
        .valign(gtk::Align::Center)
        .width_chars(4)
        .build();
    let scale_reset_button = gtk::Button::builder()
        .label("Default")
        .tooltip_text("Reset interface scale")
        .valign(gtk::Align::Center)
        .build();
    scale_row.add_suffix(&scale_spin);
    scale_row.add_suffix(&scale_reset_button);
    scale_row.set_activatable_widget(Some(&scale_spin));
    group.add(&scale_row);

    let system_fonts = collect_system_font_families(&page);
    let mut ui_font_options: Vec<String> = vec!["System default".to_string()];
    ui_font_options.extend(system_fonts.iter().cloned());
    let ui_font_refs: Vec<&str> = ui_font_options.iter().map(String::as_str).collect();

    let ui_font_row = adw::ActionRow::builder()
        .title("Interface font")
        .subtitle("Choose a system font for the Limux UI")
        .build();
    ui_font_row.set_title_lines(1);
    ui_font_row.set_subtitle_lines(2);
    let ui_font_dropdown = searchable_font_dropdown(&ui_font_refs);
    let current_ui_font = input.config.borrow().appearance.ui_font_family.clone();
    let initial_ui_font_idx = current_ui_font
        .as_deref()
        .and_then(|f| {
            ui_font_options
                .iter()
                .position(|opt| opt.eq_ignore_ascii_case(f))
        })
        .unwrap_or(0) as u32;
    ui_font_dropdown.set_selected(initial_ui_font_idx);
    ui_font_row.add_suffix(&ui_font_dropdown);
    ui_font_row.set_activatable_widget(Some(&ui_font_dropdown));
    group.add(&ui_font_row);

    let mut term_font_options: Vec<String> = vec!["Omarchy / Ghostty default".to_string()];
    term_font_options.extend(system_fonts.iter().cloned());
    let term_font_refs: Vec<&str> = term_font_options.iter().map(String::as_str).collect();

    let term_font_row = adw::ActionRow::builder()
        .title("Terminal font")
        .subtitle("System font for Ghostty terminals (updates ~/.config/ghostty/config)")
        .build();
    term_font_row.set_title_lines(1);
    term_font_row.set_subtitle_lines(2);
    let term_font_dropdown = searchable_font_dropdown(&term_font_refs);
    let current_term_font = read_ghostty_font_family();
    let initial_term_font_idx = current_term_font
        .as_deref()
        .and_then(|f| {
            term_font_options
                .iter()
                .position(|opt| opt.eq_ignore_ascii_case(f))
        })
        .unwrap_or(0) as u32;
    term_font_dropdown.set_selected(initial_term_font_idx);
    term_font_row.add_suffix(&term_font_dropdown);
    term_font_row.set_activatable_widget(Some(&term_font_dropdown));
    group.add(&term_font_row);

    let term_font_size_row = adw::ActionRow::builder()
        .title("Terminal font size")
        .subtitle("Font size in points for all terminal surfaces")
        .build();
    term_font_size_row.set_title_lines(1);
    term_font_size_row.set_subtitle_lines(2);
    let initial_term_font = input
        .config
        .borrow()
        .font_size
        .unwrap_or_else(crate::terminal::default_font_size);
    let term_font_adjustment =
        gtk::Adjustment::new(f64::from(initial_term_font), 6.0, 48.0, 0.5, 2.0, 0.0);
    let term_font_spin = gtk::SpinButton::builder()
        .adjustment(&term_font_adjustment)
        .digits(1)
        .numeric(true)
        .valign(gtk::Align::Center)
        .width_chars(4)
        .build();
    let term_font_reset_button = gtk::Button::builder()
        .label("Default")
        .tooltip_text("Reset terminal font size to Ghostty default")
        .valign(gtk::Align::Center)
        .build();
    term_font_size_row.add_suffix(&term_font_spin);
    term_font_size_row.add_suffix(&term_font_reset_button);
    term_font_size_row.set_activatable_widget(Some(&term_font_spin));
    group.add(&term_font_size_row);

    let top_bar_row = adw::ActionRow::builder()
        .title("Top bar")
        .subtitle("Show a top bar with workspace indicators. When off, the controls move into the sidebar header. If the sidebar is also hidden, its toggle remains on the leading pane.")
        .build();
    top_bar_row.set_title_lines(1);
    top_bar_row.set_subtitle_lines(4);
    let top_bar_switch = gtk::Switch::new();
    top_bar_switch.set_active(input.config.borrow().interface.show_top_bar);
    top_bar_switch.set_valign(gtk::Align::Center);
    top_bar_row.add_suffix(&top_bar_switch);
    top_bar_row.set_activatable_widget(Some(&top_bar_switch));
    group.add(&top_bar_row);

    let indicators_row = adw::ActionRow::builder()
        .title("Workspace indicators on the top bar")
        .subtitle("Show a clickable pill for each workspace in the top bar")
        .build();
    indicators_row.set_title_lines(1);
    indicators_row.set_subtitle_lines(2);
    let indicators_switch = gtk::Switch::new();
    indicators_switch.set_active(input.config.borrow().interface.show_workspace_indicators);
    indicators_switch.set_valign(gtk::Align::Center);
    indicators_row.add_suffix(&indicators_switch);
    indicators_row.set_activatable_widget(Some(&indicators_switch));
    group.add(&indicators_row);

    let tab_activity_row = adw::ActionRow::builder()
        .title("Tab activity indicators")
        .subtitle("Show a spinner when background tabs receive terminal output")
        .build();
    tab_activity_row.set_title_lines(1);
    tab_activity_row.set_subtitle_lines(2);
    let tab_activity_switch = gtk::Switch::new();
    let show_tab_activity = input.config.borrow().interface.show_tab_activity;
    tab_activity_switch.set_active(show_tab_activity);
    tab_activity_switch.set_valign(gtk::Align::Center);
    tab_activity_row.add_suffix(&tab_activity_switch);
    tab_activity_row.set_activatable_widget(Some(&tab_activity_switch));
    group.add(&tab_activity_row);

    let tab_activity_timeout_row = adw::ActionRow::builder()
        .title("Tab activity timeout")
        .subtitle("Keep the spinner visible for this many seconds after terminal output")
        .build();
    tab_activity_timeout_row.set_title_lines(1);
    tab_activity_timeout_row.set_subtitle_lines(2);
    let tab_activity_adjustment = gtk::Adjustment::new(
        f64::from(input.config.borrow().interface.tab_activity_seconds),
        f64::from(MIN_TAB_ACTIVITY_SECONDS),
        f64::from(MAX_TAB_ACTIVITY_SECONDS),
        1.0,
        5.0,
        0.0,
    );
    let tab_activity_timeout_spin = gtk::SpinButton::builder()
        .adjustment(&tab_activity_adjustment)
        .digits(0)
        .numeric(true)
        .valign(gtk::Align::Center)
        .width_chars(3)
        .build();
    tab_activity_timeout_row.add_suffix(&tab_activity_timeout_spin);
    tab_activity_timeout_row.set_activatable_widget(Some(&tab_activity_timeout_spin));
    tab_activity_timeout_row.set_sensitive(show_tab_activity);
    group.add(&tab_activity_timeout_row);

    let controls_row = adw::ActionRow::builder()
        .title("Window controls side")
        .subtitle("Place close, minimize, and maximize on the left or right of the top bar (or of the sidebar header when the top bar is off)")
        .build();
    controls_row.set_title_lines(1);
    controls_row.set_subtitle_lines(3);
    let controls_dropdown = gtk::DropDown::from_strings(&["Left", "Right"]);
    let initial_side = input.config.borrow().interface.window_controls_side;
    controls_dropdown.set_selected(match initial_side {
        WindowControlsSide::Left => 0,
        WindowControlsSide::Right => 1,
    });
    controls_dropdown.set_valign(gtk::Align::Center);
    controls_row.add_suffix(&controls_dropdown);
    controls_row.set_activatable_widget(Some(&controls_dropdown));
    group.add(&controls_row);

    let ai_usage_row = adw::ActionRow::builder()
        .title("AI usage indicator")
        .subtitle("Show active AI tool token and cost usage in the top-right")
        .build();
    ai_usage_row.set_title_lines(1);
    ai_usage_row.set_subtitle_lines(2);
    let ai_usage_switch = gtk::Switch::new();
    ai_usage_switch.set_active(input.config.borrow().interface.show_ai_usage);
    ai_usage_switch.set_valign(gtk::Align::Center);
    ai_usage_row.add_suffix(&ai_usage_switch);
    ai_usage_row.set_activatable_widget(Some(&ai_usage_switch));
    group.add(&ai_usage_row);

    let ai_titles_row = adw::ActionRow::builder()
        .title("AI tab titles")
        .subtitle(
            "Automatically update terminal tab titles from live status via Gemini or Claude Haiku CLI",
        )
        .build();
    ai_titles_row.set_title_lines(1);
    ai_titles_row.set_subtitle_lines(2);
    let ai_titles_dropdown = gtk::DropDown::from_strings(AiTabTitleBackend::labels());
    ai_titles_dropdown.set_selected(
        input
            .config
            .borrow()
            .interface
            .ai_tab_titles
            .dropdown_index(),
    );
    ai_titles_dropdown.set_valign(gtk::Align::Center);
    ai_titles_row.add_suffix(&ai_titles_dropdown);
    ai_titles_row.set_activatable_widget(Some(&ai_titles_dropdown));
    group.add(&ai_titles_row);

    page.add(&group);

    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let ui_font_options = Rc::new(ui_font_options);
        ui_font_dropdown.connect_selected_notify(move |dropdown| {
            let idx = dropdown.selected() as usize;
            let next = if idx == 0 {
                None
            } else {
                ui_font_options.get(idx).cloned()
            };
            if next == config.borrow().appearance.ui_font_family {
                return;
            }
            apply_config_change(&config, &*on_changed, move |c| {
                c.appearance.ui_font_family = next;
            });
        });
    }
    {
        let term_font_options = Rc::new(term_font_options);
        term_font_dropdown.connect_selected_notify(move |dropdown| {
            let idx = dropdown.selected() as usize;
            let chosen = if idx == 0 {
                detect_omarchy_font_family()
            } else {
                term_font_options.get(idx).cloned()
            };
            if let Some(font) = chosen {
                let _ = write_ghostty_font_family(&font);
            }
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let syncing = Rc::new(Cell::new(false));
        let syncing_for_spin = syncing.clone();
        term_font_spin.connect_value_changed(move |spin| {
            if syncing_for_spin.get() {
                return;
            }
            let size = (spin.value() as f32).clamp(6.0, 48.0);
            if config
                .borrow()
                .font_size
                .is_some_and(|existing| (existing - size).abs() < 0.05)
            {
                return;
            }
            apply_config_change(&config, &*on_changed, move |c| {
                c.font_size = Some(size);
            });
        });
        let config_for_reset = input.config.clone();
        let on_changed_for_reset = input.on_config_changed.clone();
        let term_font_spin_for_reset = term_font_spin.clone();
        term_font_reset_button.connect_clicked(move |_| {
            syncing.set(true);
            term_font_spin_for_reset.set_value(f64::from(crate::terminal::default_font_size()));
            syncing.set(false);
            apply_config_change(&config_for_reset, &*on_changed_for_reset, |c| {
                c.font_size = None;
            });
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        ai_usage_switch.connect_active_notify(move |switch| {
            let show_ai_usage = switch.is_active();
            apply_config_change(&config, &*on_changed, move |c| {
                c.interface.show_ai_usage = show_ai_usage;
            });
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let syncing = Rc::new(Cell::new(false));
        ai_titles_dropdown.connect_selected_notify(move |dropdown| {
            if syncing.get() {
                return;
            }
            let backend = AiTabTitleBackend::from_dropdown_index(dropdown.selected());
            if backend == config.borrow().interface.ai_tab_titles {
                return;
            }
            let effective = apply_config_change(&config, &*on_changed, move |c| {
                c.interface.ai_tab_titles = backend;
            })
            .interface
            .ai_tab_titles;
            if dropdown.selected() != effective.dropdown_index() {
                syncing.set(true);
                dropdown.set_selected(effective.dropdown_index());
                syncing.set(false);
            }
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let syncing = Rc::new(Cell::new(false));
        theme_dropdown.connect_selected_notify(move |dropdown| {
            if syncing.get() {
                return;
            }
            let ui_theme = UiTheme::from_dropdown_index(dropdown.selected());
            if ui_theme == config.borrow().appearance.ui_theme {
                return;
            }
            let effective = apply_config_change(&config, &*on_changed, move |c| {
                c.appearance.ui_theme = ui_theme;
            })
            .appearance
            .ui_theme;
            if dropdown.selected() != effective.dropdown_index() {
                syncing.set(true);
                dropdown.set_selected(effective.dropdown_index());
                syncing.set(false);
            }
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        color_dropdown.connect_selected_notify(move |dropdown| {
            let scheme = match dropdown.selected() {
                1 => ColorScheme::Dark,
                2 => ColorScheme::Light,
                _ => ColorScheme::System,
            };
            apply_config_change(&config, &*on_changed, move |c| {
                c.appearance.color_scheme = scheme;
            });
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        ghostty_dropdown.connect_selected_notify(move |dropdown| {
            let scheme = match dropdown.selected() {
                1 => ColorScheme::Dark,
                2 => ColorScheme::Light,
                _ => ColorScheme::System,
            };
            apply_config_change(&config, &*on_changed, move |c| {
                c.appearance.ghostty_color_scheme = scheme;
            });
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let syncing = Rc::new(Cell::new(false));
        unread_color_dropdown.connect_selected_notify(move |dropdown| {
            if syncing.get() {
                return;
            }
            let unread_color = UnreadColor::from_dropdown_index(dropdown.selected());
            if unread_color == config.borrow().appearance.unread_color {
                return;
            }
            let effective = apply_config_change(&config, &*on_changed, move |c| {
                c.appearance.unread_color = unread_color;
            })
            .appearance
            .unread_color;
            if dropdown.selected() != effective.dropdown_index() {
                syncing.set(true);
                dropdown.set_selected(effective.dropdown_index());
                syncing.set(false);
            }
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        hover_switch.connect_active_notify(move |switch| {
            let hover_terminal_focus = switch.is_active();
            apply_config_change(&config, &*on_changed, move |c| {
                c.focus.hover_terminal_focus = hover_terminal_focus;
            });
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let syncing = Rc::new(Cell::new(false));
        workspace_path_switch.connect_active_notify(move |switch| {
            if syncing.get() {
                return;
            }
            let show_workspace_path = switch.is_active();
            let effective = apply_config_change(&config, &*on_changed, move |c| {
                c.appearance.show_workspace_path = show_workspace_path;
            })
            .appearance
            .show_workspace_path;
            if switch.is_active() != effective {
                syncing.set(true);
                switch.set_active(effective);
                syncing.set(false);
            }
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        keep_workspace_open_switch.connect_active_notify(move |switch| {
            let keep_open = switch.is_active();
            apply_config_change(&config, &*on_changed, move |c| {
                c.workspace.keep_open_after_last_terminal_closes = keep_open;
            });
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        prompt_workspace_folder_switch.connect_active_notify(move |switch| {
            let prompt_for_folder = switch.is_active();
            apply_config_change(&config, &*on_changed, move |c| {
                c.workspace.prompt_for_folder_on_create = prompt_for_folder;
            });
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        auto_copy_switch.connect_active_notify(move |switch| {
            let copy_selection_to_clipboard = switch.is_active();
            apply_config_change(&config, &*on_changed, move |c| {
                c.clipboard.copy_selection_to_clipboard = copy_selection_to_clipboard;
            });
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let syncing = Rc::new(Cell::new(false));
        top_bar_switch.connect_active_notify(move |switch| {
            if syncing.get() {
                return;
            }
            let show_top_bar = switch.is_active();
            if show_top_bar == config.borrow().interface.show_top_bar {
                return;
            }
            let effective = apply_config_change(&config, &*on_changed, move |c| {
                c.interface.show_top_bar = show_top_bar;
            })
            .interface
            .show_top_bar;
            if switch.is_active() != effective {
                syncing.set(true);
                switch.set_active(effective);
                syncing.set(false);
            }
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let syncing = Rc::new(Cell::new(false));
        link_destination_dropdown.connect_selected_notify(move |dropdown| {
            if syncing.get() {
                return;
            }
            let destination = LinkOpenDestination::from_dropdown_index(dropdown.selected());
            let effective = apply_config_change(&config, &*on_changed, move |c| {
                c.links.open_destination = destination;
            })
            .links
            .open_destination
            .dropdown_index();
            if dropdown.selected() != effective {
                syncing.set(true);
                dropdown.set_selected(effective);
                syncing.set(false);
            }
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let updating_spin = Rc::new(Cell::new(false));
        let updating_spin_for_reset = updating_spin.clone();
        scale_spin.connect_value_changed(move |spin| {
            if updating_spin.get() {
                return;
            }
            let scale = UiScale::new((spin.value() / 100.0) as f32).unwrap_or_default();
            apply_config_change(&config, &*on_changed, move |c| {
                c.appearance.ui_scale = scale;
            });
        });

        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let scale_spin = scale_spin.clone();
        scale_reset_button.connect_clicked(move |_| {
            apply_config_change(&config, &*on_changed, move |c| {
                c.appearance.ui_scale = UiScale::default();
            });
            updating_spin_for_reset.set(true);
            scale_spin.set_value(f64::from(DEFAULT_UI_SCALE) * 100.0);
            updating_spin_for_reset.set(false);
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let syncing = Rc::new(Cell::new(false));
        indicators_switch.connect_active_notify(move |switch| {
            if syncing.get() {
                return;
            }
            let show_workspace_indicators = switch.is_active();
            if show_workspace_indicators == config.borrow().interface.show_workspace_indicators {
                return;
            }
            let effective = apply_config_change(&config, &*on_changed, move |c| {
                c.interface.show_workspace_indicators = show_workspace_indicators;
            })
            .interface
            .show_workspace_indicators;
            if switch.is_active() != effective {
                syncing.set(true);
                switch.set_active(effective);
                syncing.set(false);
            }
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let timeout_row = tab_activity_timeout_row.clone();
        let syncing = Rc::new(Cell::new(false));
        tab_activity_switch.connect_active_notify(move |switch| {
            if syncing.get() {
                return;
            }
            let show_tab_activity = switch.is_active();
            if show_tab_activity == config.borrow().interface.show_tab_activity {
                timeout_row.set_sensitive(show_tab_activity);
                return;
            }
            let effective = apply_config_change(&config, &*on_changed, move |c| {
                c.interface.show_tab_activity = show_tab_activity;
            })
            .interface
            .show_tab_activity;
            timeout_row.set_sensitive(effective);
            if switch.is_active() != effective {
                syncing.set(true);
                switch.set_active(effective);
                syncing.set(false);
            }
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let syncing = Rc::new(Cell::new(false));
        tab_activity_timeout_spin.connect_value_changed(move |spin| {
            if syncing.get() {
                return;
            }
            let seconds = spin.value_as_int().clamp(
                MIN_TAB_ACTIVITY_SECONDS as i32,
                MAX_TAB_ACTIVITY_SECONDS as i32,
            ) as u32;
            if seconds == config.borrow().interface.tab_activity_seconds {
                return;
            }
            let effective = apply_config_change(&config, &*on_changed, move |c| {
                c.interface.tab_activity_seconds = seconds;
            })
            .interface
            .tab_activity_seconds;
            if spin.value_as_int() != effective as i32 {
                syncing.set(true);
                spin.set_value(f64::from(effective));
                syncing.set(false);
            }
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let syncing = Rc::new(Cell::new(false));
        controls_dropdown.connect_selected_notify(move |dropdown| {
            if syncing.get() {
                return;
            }
            let side = match dropdown.selected() {
                0 => WindowControlsSide::Left,
                _ => WindowControlsSide::Right,
            };
            if side == config.borrow().interface.window_controls_side {
                return;
            }
            let effective = apply_config_change(&config, &*on_changed, move |c| {
                c.interface.window_controls_side = side;
            })
            .interface
            .window_controls_side;
            let selected = match effective {
                WindowControlsSide::Left => 0,
                WindowControlsSide::Right => 1,
            };
            if dropdown.selected() != selected {
                syncing.set(true);
                dropdown.set_selected(selected);
                syncing.set(false);
            }
        });
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&page)
        .build();
    scroller.set_hexpand(true);
    scroller.set_vexpand(true);

    scroller.upcast()
}

fn build_notifications_page(input: &SettingsEditorInput) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    page.set_title("Notifications");
    page.set_name(Some("notifications"));
    page.set_icon_name(Some("preferences-system-notifications-symbolic"));
    page.set_hexpand(true);
    page.set_vexpand(true);

    let group = adw::PreferencesGroup::new();

    let enabled_row = adw::ActionRow::builder()
        .title("Desktop notifications")
        .subtitle("Show desktop alerts when background workspaces need attention")
        .build();
    enabled_row.set_title_lines(1);
    enabled_row.set_subtitle_lines(2);
    let notifications = input.config.borrow().notifications;
    let enabled_switch = gtk::Switch::new();
    enabled_switch.set_active(notifications.enabled);
    enabled_switch.set_valign(gtk::Align::Center);
    enabled_row.add_suffix(&enabled_switch);
    enabled_row.set_activatable_widget(Some(&enabled_switch));
    group.add(&enabled_row);

    let sound_row = adw::ActionRow::builder()
        .title("Notification sound")
        .subtitle("Choose sound hint sent with desktop alerts. Support depends on your desktop notification service")
        .build();
    sound_row.set_title_lines(1);
    sound_row.set_subtitle_lines(3);
    sound_row.set_sensitive(notifications.enabled);
    let sound_dropdown = gtk::DropDown::from_strings(NotificationSound::labels());
    sound_dropdown.set_selected(notifications.sound.dropdown_index());
    sound_dropdown.set_valign(gtk::Align::Center);
    sound_row.add_suffix(&sound_dropdown);
    sound_row.set_activatable_widget(Some(&sound_dropdown));
    group.add(&sound_row);

    page.add(&group);

    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        let sound_row = sound_row.clone();
        enabled_switch.connect_active_notify(move |switch| {
            let enabled = switch.is_active();
            sound_row.set_sensitive(enabled);
            apply_config_change(&config, &*on_changed, move |c| {
                c.notifications.enabled = enabled;
            });
        });
    }
    {
        let config = input.config.clone();
        let on_changed = input.on_config_changed.clone();
        sound_dropdown.connect_selected_notify(move |dropdown| {
            let sound = NotificationSound::from_dropdown_index(dropdown.selected());
            apply_config_change(&config, &*on_changed, move |c| {
                c.notifications.sound = sound;
            });
        });
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&page)
        .build();
    scroller.set_hexpand(true);
    scroller.set_vexpand(true);

    scroller.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::DEFAULT_TAB_ACTIVITY_SECONDS;

    fn find_row(widget: &gtk::Widget, title: &str) -> Option<adw::ActionRow> {
        if let Some(row) = widget.downcast_ref::<adw::ActionRow>() {
            if row.title() == title {
                return Some(row.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(row) = find_row(&widget, title) {
                return Some(row);
            }
            child = widget.next_sibling();
        }
        None
    }

    fn find_control(widget: &gtk::Widget, title: &str) -> Option<gtk::Widget> {
        find_row(widget, title)?.activatable_widget()
    }

    fn find_descendant<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Option<T> {
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(found) = widget
                .clone()
                .downcast::<T>()
                .ok()
                .or_else(|| find_descendant(&widget))
            {
                return Some(found);
            }
            child = widget.next_sibling();
        }
        None
    }

    #[test]
    #[ignore = "requires GTK; exercised by xvfb-smoke-test.sh"]
    fn interface_controls_restore_effective_values_after_save_failure() {
        adw::init().expect("initialize GTK");
        let config = Rc::new(RefCell::new(AppConfig::default()));
        let rejected_changes = Rc::new(Cell::new(0));
        let reject_changes = Rc::new(Cell::new(true));
        let page = build_general_page(&SettingsEditorInput {
            config: config.clone(),
            shortcuts: Rc::new(crate::shortcut_config::default_shortcuts()),
            on_capture: Rc::new(|_, _| Ok(crate::shortcut_config::default_shortcuts())),
            on_config_changed: Rc::new({
                let config = config.clone();
                let rejected_changes = rejected_changes.clone();
                let reject_changes = reject_changes.clone();
                move |previous, _| {
                    rejected_changes.set(rejected_changes.get() + 1);
                    if reject_changes.get() {
                        config.borrow_mut().clone_from(previous);
                    }
                }
            }),
        });
        for title in ["Top bar", "Workspace indicators on the top bar"] {
            let switch = find_control(&page, title)
                .expect("settings control")
                .downcast::<gtk::Switch>()
                .expect("switch");
            switch.set_active(false);
            assert!(switch.is_active(), "{title} retained a rejected setting");
        }
        let dropdown = find_control(&page, "Window controls side")
            .expect("settings control")
            .downcast::<gtk::DropDown>()
            .expect("dropdown");
        dropdown.set_selected(0);
        assert_eq!(dropdown.selected(), 1);
        let tab_activity_switch = find_control(&page, "Tab activity indicators")
            .expect("settings control")
            .downcast::<gtk::Switch>()
            .expect("switch");
        let timeout_spin = find_control(&page, "Tab activity timeout")
            .expect("settings control")
            .downcast::<gtk::SpinButton>()
            .expect("spin button");
        assert_eq!(timeout_spin.adjustment().lower(), 1.0);
        assert_eq!(timeout_spin.adjustment().upper(), 60.0);
        tab_activity_switch.set_active(false);
        assert!(
            tab_activity_switch.is_active(),
            "rejected activity toggle was retained"
        );
        assert_eq!(
            timeout_spin.value_as_int(),
            DEFAULT_TAB_ACTIVITY_SECONDS as i32
        );
        timeout_spin.set_value(12.0);
        assert_eq!(
            timeout_spin.value_as_int(),
            DEFAULT_TAB_ACTIVITY_SECONDS as i32
        );
        let unread_color = find_control(&page, "Unread indicator color")
            .expect("settings control")
            .downcast::<gtk::DropDown>()
            .expect("dropdown");
        unread_color.set_selected(UnreadColor::Rose.dropdown_index());
        assert_eq!(
            unread_color.selected(),
            UnreadColor::default().dropdown_index(),
            "rejected unread color was retained"
        );
        assert_eq!(config.borrow().interface, Default::default());
        assert_eq!(rejected_changes.get(), 6, "rollback must not save again");

        reject_changes.set(false);
        tab_activity_switch.set_active(false);
        assert!(!tab_activity_switch.is_active());
        assert!(!timeout_spin.is_sensitive());
        assert!(!config.borrow().interface.show_tab_activity);
    }

    #[test]
    #[ignore = "requires GTK; exercised by xvfb-smoke-test.sh"]
    fn hook_controls_restore_effective_values_after_save_failure() {
        fn find_expander(widget: &gtk::Widget, title: &str) -> Option<adw::ExpanderRow> {
            if let Some(row) = widget.downcast_ref::<adw::ExpanderRow>() {
                if row.title() == title {
                    return Some(row.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(row) = find_expander(&widget, title) {
                    return Some(row);
                }
                child = widget.next_sibling();
            }
            None
        }

        adw::init().expect("initialize GTK");
        let config = Rc::new(RefCell::new(AppConfig::default()));
        let event = GuiHookEvent::OnWorkspaceCreate;
        let original = GuiHook {
            enabled: false,
            command: "before-save".to_string(),
            show_terminal: true,
            suppress_default: false,
        };
        *config.borrow_mut().hooks.get_mut(event) = original.clone();
        let rejected_changes = Rc::new(Cell::new(0));
        let page = build_hooks_page(&SettingsEditorInput {
            config: config.clone(),
            shortcuts: Rc::new(crate::shortcut_config::default_shortcuts()),
            on_capture: Rc::new(|_, _| Ok(crate::shortcut_config::default_shortcuts())),
            on_config_changed: Rc::new({
                let config = config.clone();
                let rejected_changes = rejected_changes.clone();
                move |previous, _| {
                    rejected_changes.set(rejected_changes.get() + 1);
                    config.borrow_mut().clone_from(previous);
                }
            }),
        });
        let row = find_expander(&page, event.title()).expect("hook row");
        let row: &gtk::Widget = row.upcast_ref();
        let enabled = find_control(row, "Enabled")
            .expect("Enabled switch")
            .downcast::<gtk::Switch>()
            .expect("switch");
        let command = find_row(row, "Command")
            .and_then(|row| find_descendant::<gtk::Entry>(row.upcast_ref()))
            .expect("command entry");
        let show_terminal = find_control(row, "Show in a terminal")
            .expect("Show in a terminal switch")
            .downcast::<gtk::Switch>()
            .expect("switch");
        let suppress_default = find_control(row, "Skip the default action")
            .expect("Skip the default action switch")
            .downcast::<gtk::Switch>()
            .expect("switch");

        enabled.set_active(true);
        assert!(!enabled.is_active(), "rejected enabled change was retained");
        command.set_text("rejected-command");
        assert_eq!(command.text(), original.command);
        show_terminal.set_active(false);
        assert!(
            show_terminal.is_active(),
            "rejected terminal change was retained"
        );
        suppress_default.set_active(true);
        assert!(
            !suppress_default.is_active(),
            "rejected suppress-default change was retained"
        );
        assert_eq!(
            row.downcast_ref::<adw::ExpanderRow>().unwrap().subtitle(),
            gui_hook_subtitle(&original)
        );
        assert_eq!(config.borrow().hooks.get(event), &original);
        assert_eq!(
            rejected_changes.get(),
            4,
            "refreshing controls must not save the rejected values again"
        );
    }

    #[test]
    #[ignore = "requires GTK; exercised by xvfb-smoke-test.sh"]
    fn hook_rows_show_socket_changes_without_saving_them_again() {
        fn find_expander(widget: &gtk::Widget, title: &str) -> Option<adw::ExpanderRow> {
            if let Some(row) = widget.downcast_ref::<adw::ExpanderRow>() {
                if row.title() == title {
                    return Some(row.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(row) = find_expander(&widget, title) {
                    return Some(row);
                }
                child = widget.next_sibling();
            }
            None
        }

        adw::init().expect("initialize GTK");
        let config = Rc::new(RefCell::new(AppConfig::default()));
        for event in GuiHookEvent::ALL {
            config.borrow_mut().hooks.get_mut(event).command =
                format!("{} before-socket", event.as_str());
        }
        let saves = Rc::new(Cell::new(0));
        let page = build_hooks_page(&SettingsEditorInput {
            config: config.clone(),
            shortcuts: Rc::new(crate::shortcut_config::default_shortcuts()),
            on_capture: Rc::new(|_, _| Ok(crate::shortcut_config::default_shortcuts())),
            on_config_changed: Rc::new({
                let saves = saves.clone();
                move |_, _| saves.set(saves.get() + 1)
            }),
        });

        for event in GuiHookEvent::ALL {
            let row = find_expander(&page, event.title())
                .unwrap_or_else(|| panic!("{} row", event.title()));
            let row: &gtk::Widget = row.upcast_ref();
            let switch = |title| {
                find_control(row, title).and_then(|control| control.downcast::<gtk::Switch>().ok())
            };
            let enabled = switch("Enabled").expect("Enabled switch");
            let terminal = switch("Show in a terminal").expect("terminal switch");
            let suppress = switch("Skip the default action");
            // An after-* hook runs once Limux's action is done: nothing to skip.
            assert_eq!(suppress.is_some(), event.can_skip_default(), "{event:?}");
            let command = find_row(row, "Command")
                .and_then(|row| find_descendant::<gtk::Entry>(row.upcast_ref()))
                .expect("command entry");

            // What the socket handler does after saving a change.
            let saves_before = saves.get();
            let hooks = {
                let mut config = config.borrow_mut();
                let hook = config.hooks.get_mut(event);
                hook.command = format!("{} --force", event.as_str());
                hook.enabled = false;
                hook.show_terminal = true;
                hook.suppress_default = event.can_skip_default();
                config.hooks.clone()
            };
            refresh_gui_hook_rows(&hooks);
            assert_eq!(command.text(), format!("{} --force", event.as_str()));
            assert!(!enabled.is_active());
            assert!(terminal.is_active());
            if let Some(suppress) = &suppress {
                assert!(suppress.is_active());
            }
            assert_eq!(saves.get(), saves_before, "a refresh must not save again");
            assert_eq!(config.borrow().hooks, hooks);

            // The user's own changes still save, to this hook only.
            enabled.set_active(true);
            assert_eq!(saves.get(), saves_before + 1);
            assert!(config.borrow().hooks.get(event).enabled);
            let mut others = hooks.clone();
            others.get_mut(event).enabled = true;
            assert_eq!(config.borrow().hooks, others);
        }
    }

    #[test]
    #[ignore = "requires GTK; exercised by xvfb-smoke-test.sh"]
    fn hooks_is_the_second_settings_tab() {
        adw::init().expect("initialize GTK");
        let window = adw::Window::new();
        let content = build_settings_window_content(
            &window,
            SettingsEditorInput {
                config: Rc::new(RefCell::new(AppConfig::default())),
                shortcuts: Rc::new(crate::shortcut_config::default_shortcuts()),
                on_capture: Rc::new(|_, _| Ok(crate::shortcut_config::default_shortcuts())),
                on_config_changed: Rc::new(|_, _| {}),
            },
        );
        let stack = find_descendant::<adw::ViewStack>(&content).expect("settings stack");
        let pages = stack.pages();
        let names: Vec<String> = (0..pages.n_items())
            .filter_map(|index| pages.item(index))
            .filter_map(|page| page.downcast::<adw::ViewStackPage>().ok())
            .filter_map(|page| page.name().map(|name| name.to_string()))
            .collect();
        assert_eq!(names, ["general", "hooks", "notifications", "keybindings"]);
    }

    #[test]
    fn apply_config_change_returns_reentrant_config_state() {
        let config = Rc::new(RefCell::new(AppConfig::default()));

        let effective = apply_config_change(
            &config,
            &|previous, _updated| {
                config.borrow_mut().clone_from(previous);
            },
            |current| {
                current.focus.hover_terminal_focus = true;
            },
        );

        assert!(!effective.focus.hover_terminal_focus);
        assert!(!config.borrow().focus.hover_terminal_focus);
    }

    #[test]
    fn ui_scale_css_scales_sidebar_and_pane_chrome() {
        let mut config = AppConfig::default();
        config.appearance.ui_scale = UiScale::new(1.5).expect("valid scale");

        let css = ui_scale_css(&config);

        assert!(css.contains(".limux-ws-name { font-size: 22.500px; }"));
        assert!(css.contains(".limux-tab { font-size: 18.000px; }"));
        assert!(css.contains(".limux-pane-action image { -gtk-icon-size: 24.000px; }"));
    }
}
