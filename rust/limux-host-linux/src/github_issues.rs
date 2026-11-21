use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use serde_json::Value;
use shell_quote::QuoteRefExt;

const ISSUES_PANEL_WIDTH: i32 = 360;

pub const GITHUB_ISSUES_CSS: &str = r#"
.limux-issues-panel {
    background-color: @sidebar_bg_color;
    border-left: 1px solid alpha(@window_fg_color, 0.10);
}
.limux-issues-header {
    padding: 4px 8px;
    border-bottom: 1px solid alpha(@window_fg_color, 0.08);
    min-height: 30px;
}
.limux-issues-title {
    font-size: 12px;
    font-weight: 700;
    color: @window_fg_color;
}
.limux-issues-list {
    background: transparent;
}
.limux-issues-list row {
    padding: 0;
    background: transparent;
}
.limux-issue-card {
    padding: 8px 10px;
    margin: 2px 6px;
    border-radius: 3px;
    background: alpha(@window_fg_color, 0.04);
    border: 1px solid alpha(@window_fg_color, 0.08);
}
.limux-issue-card:hover {
    background: alpha(@window_fg_color, 0.07);
    border-color: alpha(@accent_bg_color, 0.40);
}
.limux-issue-num {
    font-size: 11px;
    font-weight: 700;
    color: @accent_color;
}
.limux-issue-num-closed {
    color: alpha(@window_fg_color, 0.45);
}
.limux-issue-title {
    font-size: 12px;
    font-weight: 600;
    color: @window_fg_color;
}
.limux-issue-meta {
    font-size: 10px;
    color: alpha(@window_fg_color, 0.55);
}
.limux-issue-label {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 2px;
    background: alpha(@accent_bg_color, 0.16);
    color: @window_fg_color;
}
.limux-issue-action-btn {
    font-size: 10px;
    font-weight: 600;
    padding: 2px 6px;
    min-height: 20px;
    border-radius: 2px;
    background: alpha(@window_fg_color, 0.07);
    border: 1px solid alpha(@window_fg_color, 0.12);
}
.limux-issue-action-btn:hover {
    background: alpha(@accent_bg_color, 0.22);
    border-color: @accent_bg_color;
}
.limux-issues-toggle-btn {
    font-size: 11px;
    font-weight: 600;
    padding: 2px 7px;
    margin: 2px 2px;
    min-height: 0;
    border-radius: 3px;
}
.limux-issue-detail-body {
    font-size: 12px;
    padding: 6px 8px;
    border-radius: 3px;
    background: alpha(@window_fg_color, 0.03);
    border: 1px solid alpha(@window_fg_color, 0.08);
}
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueAgent {
    GeminiAgy,
    Claude,
    Codex,
}

impl IssueAgent {
    pub fn label(self) -> &'static str {
        match self {
            Self::GeminiAgy => "Gemini",
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }

    pub fn executable(self) -> &'static str {
        match self {
            Self::GeminiAgy => "agy",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueComment {
    pub author: String,
    pub body: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubIssue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub body: String,
    pub url: String,
    pub labels: Vec<String>,
    pub comments: Vec<IssueComment>,
    pub updated_at: String,
}

impl GitHubIssue {
    pub fn is_open(&self) -> bool {
        self.state.eq_ignore_ascii_case("OPEN")
    }
}

/// Parse `owner/repo` from a GitHub remote URL (`git@github.com:o/r.git` or `https://github.com/o/r.git`).
pub fn parse_github_repo_from_remote(remote_url: &str) -> Option<String> {
    let trimmed = remote_url.trim().trim_end_matches('/');
    let rest = if let Some(after) = trimmed.strip_prefix("git@github.com:") {
        after
    } else {
        let idx = trimmed.find("github.com/")?;
        &trimmed[idx + "github.com/".len()..]
    };
    let clean = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = clean.split('/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

pub fn detect_github_repo(dir: &Path) -> Option<String> {
    if let Ok(output) = Command::new("git")
        .args(["-C", &dir.to_string_lossy(), "remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if output.status.success() {
            if let Ok(url) = String::from_utf8(output.stdout) {
                if let Some(repo) = parse_github_repo_from_remote(&url) {
                    return Some(repo);
                }
            }
        }
    }

    let output = Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub fn parse_gh_issues_json(raw: &str) -> Result<Vec<GitHubIssue>, String> {
    let items: Vec<Value> = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    let mut issues = Vec::with_capacity(items.len());
    for item in items {
        let Some(number) = item.get("number").and_then(Value::as_u64) else {
            continue;
        };
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let state = item
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("OPEN")
            .to_string();
        let body = item
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let url = item
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let updated_at = item
            .get("updatedAt")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let labels = item
            .get("labels")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| l.get("name").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let comments = item
            .get("comments")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|c| IssueComment {
                        author: c
                            .get("author")
                            .and_then(|a| a.get("login"))
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        body: c
                            .get("body")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        created_at: c
                            .get("createdAt")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        issues.push(GitHubIssue {
            number,
            title,
            state,
            body,
            url,
            labels,
            comments,
            updated_at,
        });
    }
    Ok(issues)
}

pub fn fetch_issues_for_dir(
    dir: &Path,
    state_filter: &str,
) -> Result<(String, Vec<GitHubIssue>), String> {
    let repo = detect_github_repo(dir)
        .ok_or_else(|| "No GitHub repository detected for this workspace.".to_string())?;
    let output = Command::new("gh")
        .args([
            "issue",
            "list",
            "--repo",
            &repo,
            "--state",
            state_filter,
            "--limit",
            "40",
            "--json",
            "number,title,state,body,url,labels,comments,updatedAt",
        ])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("Failed to run gh CLI: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh issue list failed: {}", stderr.trim()));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|err| err.to_string())?;
    let issues = parse_gh_issues_json(&stdout)?;
    Ok((repo, issues))
}

fn issues_state_dir(repo: &str) -> Option<PathBuf> {
    let base = dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .or_else(dirs::data_local_dir)?;
    let slug = repo.replace('/', "__");
    Some(base.join("limux").join("issues").join(slug))
}

pub fn issue_notes_path(repo: &str, number: u64) -> Option<PathBuf> {
    Some(issues_state_dir(repo)?.join(format!("issue-{number}-notes.md")))
}

pub fn issue_context_path(repo: &str, number: u64) -> Option<PathBuf> {
    Some(issues_state_dir(repo)?.join(format!("issue-{number}-context.md")))
}

pub fn load_issue_notes(repo: &str, number: u64) -> String {
    issue_notes_path(repo, number)
        .and_then(|p| fs::read_to_string(p).ok())
        .unwrap_or_default()
}

pub fn append_issue_note(
    repo: &str,
    number: u64,
    author: &str,
    note: &str,
) -> Result<PathBuf, String> {
    let path = issue_notes_path(repo, number).ok_or_else(|| "state dir unavailable".to_string())?;
    append_issue_note_to_path(&path, author, note)?;
    Ok(path)
}

fn append_issue_note_to_path(path: &Path, author: &str, note: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut existing = fs::read_to_string(path).unwrap_or_default();
    if !existing.is_empty() && !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push_str(&format!("\n### Note ({author} · {ts})\n{}\n", note.trim()));
    fs::write(path, existing).map_err(|err| err.to_string())
}

pub fn write_full_issue_context_file(
    repo: &str,
    issue: &GitHubIssue,
) -> Result<(PathBuf, PathBuf), String> {
    let context_path = issue_context_path(repo, issue.number)
        .ok_or_else(|| "state dir unavailable".to_string())?;
    let notes_path =
        issue_notes_path(repo, issue.number).ok_or_else(|| "state dir unavailable".to_string())?;
    if let Some(parent) = context_path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    if !notes_path.exists() {
        let _ = fs::write(
            &notes_path,
            format!("# Shared AI Notes for {repo}#{}\n", issue.number),
        );
    }
    let notes = fs::read_to_string(&notes_path).unwrap_or_default();
    let content = render_issue_context_markdown(repo, issue, &notes);
    fs::write(&context_path, content).map_err(|err| err.to_string())?;
    Ok((context_path, notes_path))
}

pub fn render_issue_context_markdown(
    repo: &str,
    issue: &GitHubIssue,
    shared_notes: &str,
) -> String {
    let mut md = format!(
        "# GitHub Issue {repo}#{}: {}\n\n- **State:** {}\n- **URL:** {}\n",
        issue.number, issue.title, issue.state, issue.url
    );
    if !issue.labels.is_empty() {
        md.push_str(&format!("- **Labels:** {}\n", issue.labels.join(", ")));
    }
    md.push_str("\n## Description\n\n");
    if issue.body.trim().is_empty() {
        md.push_str("_(No description)_\n");
    } else {
        md.push_str(issue.body.trim());
        md.push('\n');
    }
    if !issue.comments.is_empty() {
        md.push_str("\n## GitHub Comments\n");
        for comment in &issue.comments {
            md.push_str(&format!(
                "\n### @{} ({})\n{}\n",
                comment.author,
                comment.created_at,
                comment.body.trim()
            ));
        }
    }
    if !shared_notes.trim().is_empty() {
        md.push_str("\n## Shared Limux AI Notes\n\n");
        md.push_str(shared_notes.trim());
        md.push('\n');
    }
    md
}

pub fn build_issue_agent_command(
    agent: IssueAgent,
    repo: &str,
    issue: &GitHubIssue,
    context_file: &Path,
    notes_file: &Path,
) -> String {
    let prompt = format!(
        "You are assigned GitHub issue {repo}#{} (\"{}\"). \
         First read the full issue context at {} and shared AI notes at {}. \
         While working: record key findings/progress to {} AND post milestone updates on GitHub with `gh issue comment {} --repo {} --body \"...\"`. \
         Update labels if needed with `gh issue edit {} --repo {}`. \
         When finished and tests pass, close the issue with a clear summary using `gh issue close {} --repo {} --comment \"...\"`.",
        issue.number,
        issue.title,
        context_file.display(),
        notes_file.display(),
        notes_file.display(),
        issue.number,
        repo,
        issue.number,
        repo,
        issue.number,
        repo,
    );
    let quoted_prompt: String = prompt.quoted(shell_quote::Bash);
    match agent {
        IssueAgent::GeminiAgy => format!("{} -i {}", agent.executable(), quoted_prompt),
        IssueAgent::Claude => format!("{} {}", agent.executable(), quoted_prompt),
        IssueAgent::Codex => format!("{} {}", agent.executable(), quoted_prompt),
    }
}

pub fn post_github_comment(dir: &Path, repo: &str, number: u64, body: &str) -> Result<(), String> {
    let output = Command::new("gh")
        .args([
            "issue",
            "comment",
            &number.to_string(),
            "--repo",
            repo,
            "--body",
            body,
        ])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

pub fn set_issue_state(dir: &Path, repo: &str, number: u64, close: bool) -> Result<(), String> {
    let subcmd = if close { "close" } else { "reopen" };
    let output = Command::new("gh")
        .args(["issue", subcmd, &number.to_string(), "--repo", repo])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

pub fn create_github_issue(dir: &Path, repo: &str, title: &str, body: &str) -> Result<(), String> {
    let output = Command::new("gh")
        .args([
            "issue", "create", "--repo", repo, "--title", title, "--body", body,
        ])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

type OnLaunchAgent = Rc<dyn Fn(String, String, String)>;

#[derive(Clone)]
pub struct IssuesPanel {
    root: gtk::Box,
    inner: Rc<RefCell<IssuesPanelInner>>,
}

struct IssuesPanelInner {
    workspace_id: Option<String>,
    workspace_dir: Option<PathBuf>,
    repo_name: Option<String>,
    state_filter: String,
    search_query: String,
    issues: Vec<GitHubIssue>,
    selected_issue: Option<u64>,
    header_title: gtk::Label,
    status_label: gtk::Label,
    view_stack: gtk::Stack,
    list_box: gtk::ListBox,
    detail_box: gtk::Box,
    on_launch_agent: OnLaunchAgent,
}

impl IssuesPanel {
    pub fn new(on_launch_agent: OnLaunchAgent) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(ISSUES_PANEL_WIDTH)
            .hexpand(false)
            .vexpand(true)
            .visible(false)
            .build();
        root.add_css_class("limux-issues-panel");

        let header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .build();
        header.add_css_class("limux-issues-header");

        let header_title = gtk::Label::builder()
            .label("GitHub Issues")
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        header_title.add_css_class("limux-issues-title");

        let new_btn = gtk::Button::from_icon_name("list-add-symbolic");
        new_btn.add_css_class("flat");
        new_btn.add_css_class("limux-pane-action");
        new_btn.set_tooltip_text(Some("New GitHub Issue"));

        let refresh_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_btn.add_css_class("flat");
        refresh_btn.add_css_class("limux-pane-action");
        refresh_btn.set_tooltip_text(Some("Refresh issues"));

        let close_btn = gtk::Button::from_icon_name("window-close-symbolic");
        close_btn.add_css_class("flat");
        close_btn.add_css_class("limux-pane-action");
        close_btn.set_tooltip_text(Some("Hide Issues panel"));

        header.append(&header_title);
        header.append(&new_btn);
        header.append(&refresh_btn);
        header.append(&close_btn);
        root.append(&header);

        let filter_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .margin_top(6)
            .margin_bottom(4)
            .margin_start(6)
            .margin_end(6)
            .build();
        let search_entry = gtk::Entry::builder()
            .placeholder_text("Filter issues...")
            .hexpand(true)
            .build();
        let state_dropdown = gtk::DropDown::from_strings(&["Open", "Closed", "All"]);
        state_dropdown.set_selected(0);
        filter_row.append(&search_entry);
        filter_row.append(&state_dropdown);
        root.append(&filter_row);

        let status_label = gtk::Label::builder()
            .label("Select a GitHub workspace to load issues.")
            .xalign(0.0)
            .wrap(true)
            .margin_start(8)
            .margin_end(8)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        status_label.add_css_class("limux-issue-meta");
        root.append(&status_label);

        let view_stack = gtk::Stack::new();
        view_stack.set_hexpand(true);
        view_stack.set_vexpand(true);

        let list_box = gtk::ListBox::new();
        list_box.set_selection_mode(gtk::SelectionMode::None);
        list_box.add_css_class("limux-issues-list");
        let list_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&list_box)
            .build();
        view_stack.add_named(&list_scroll, Some("list"));

        let detail_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(6)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();
        let detail_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&detail_box)
            .build();
        view_stack.add_named(&detail_scroll, Some("detail"));

        // New issue creation form
        let new_issue_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(8)
            .margin_end(8)
            .build();
        let new_back_btn = gtk::Button::with_label("← Back");
        new_back_btn.add_css_class("flat");
        new_back_btn.set_halign(gtk::Align::Start);
        let new_title_entry = gtk::Entry::builder()
            .placeholder_text("Issue title...")
            .build();
        let new_body_view = gtk::TextView::new();
        new_body_view.set_wrap_mode(gtk::WrapMode::WordChar);
        new_body_view.set_vexpand(true);
        let new_body_scroll = gtk::ScrolledWindow::builder()
            .min_content_height(140)
            .vexpand(true)
            .child(&new_body_view)
            .build();
        let create_submit_btn = gtk::Button::with_label("Create GitHub Issue");
        create_submit_btn.add_css_class("suggested-action");
        new_issue_box.append(&new_back_btn);
        new_issue_box.append(&new_title_entry);
        new_issue_box.append(&new_body_scroll);
        new_issue_box.append(&create_submit_btn);
        view_stack.add_named(&new_issue_box, Some("new"));

        view_stack.set_visible_child_name("list");
        root.append(&view_stack);

        let inner = Rc::new(RefCell::new(IssuesPanelInner {
            workspace_id: None,
            workspace_dir: None,
            repo_name: None,
            state_filter: "open".to_string(),
            search_query: String::new(),
            issues: Vec::new(),
            selected_issue: None,
            header_title,
            status_label,
            view_stack: view_stack.clone(),
            list_box,
            detail_box,
            on_launch_agent,
        }));

        let panel = Self {
            root: root.clone(),
            inner: inner.clone(),
        };

        {
            let root = root.clone();
            close_btn.connect_clicked(move |_| {
                root.set_visible(false);
            });
        }
        {
            let panel = panel.clone();
            refresh_btn.connect_clicked(move |_| {
                panel.refresh();
            });
        }
        {
            let view_stack = view_stack.clone();
            new_btn.connect_clicked(move |_| {
                view_stack.set_visible_child_name("new");
            });
        }
        {
            let view_stack = view_stack.clone();
            new_back_btn.connect_clicked(move |_| {
                view_stack.set_visible_child_name("list");
            });
        }
        {
            let panel = panel.clone();
            search_entry.connect_changed(move |entry| {
                panel.inner.borrow_mut().search_query = entry.text().to_string();
                panel.render_list();
            });
        }
        {
            let panel = panel.clone();
            state_dropdown.connect_selected_notify(move |dd| {
                let filter = match dd.selected() {
                    1 => "closed",
                    2 => "all",
                    _ => "open",
                };
                panel.inner.borrow_mut().state_filter = filter.to_string();
                panel.refresh();
            });
        }
        {
            let panel = panel.clone();
            let title_entry = new_title_entry.clone();
            let body_view = new_body_view.clone();
            create_submit_btn.connect_clicked(move |btn| {
                let title = title_entry.text().trim().to_string();
                if title.is_empty() {
                    return;
                }
                let buf = body_view.buffer();
                let body = buf
                    .text(&buf.start_iter(), &buf.end_iter(), false)
                    .to_string();
                let (Some(dir), Some(repo)) = ({
                    let s = panel.inner.borrow();
                    (s.workspace_dir.clone(), s.repo_name.clone())
                }) else {
                    return;
                };
                btn.set_sensitive(false);
                let btn_weak = btn.downgrade();
                let panel_clone = panel.clone();
                let title_entry_clone = title_entry.clone();
                let body_view_clone = body_view.clone();
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let res = create_github_issue(&dir, &repo, &title, &body);
                    let _ = tx.send(res);
                });
                glib::timeout_add_local(Duration::from_millis(80), move || match rx.try_recv() {
                    Ok(res) => {
                        if let Some(b) = btn_weak.upgrade() {
                            b.set_sensitive(true);
                        }
                        match res {
                            Ok(()) => {
                                title_entry_clone.set_text("");
                                body_view_clone.buffer().set_text("");
                                panel_clone
                                    .inner
                                    .borrow()
                                    .view_stack
                                    .set_visible_child_name("list");
                                panel_clone.refresh();
                            }
                            Err(err) => {
                                panel_clone
                                    .inner
                                    .borrow()
                                    .status_label
                                    .set_label(&format!("Create failed: {err}"));
                            }
                        }
                        glib::ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
                });
            });
        }

        panel
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn is_visible(&self) -> bool {
        self.root.is_visible()
    }

    pub fn toggle(&self, workspace_id: Option<String>, folder_path: Option<String>) {
        let next = !self.root.is_visible();
        self.root.set_visible(next);
        if next {
            self.set_workspace(workspace_id, folder_path);
        }
    }

    pub fn set_workspace(&self, workspace_id: Option<String>, folder_path: Option<String>) {
        let next_dir = folder_path.map(PathBuf::from);
        let changed = {
            let mut s = self.inner.borrow_mut();
            let diff = s.workspace_id != workspace_id || s.workspace_dir != next_dir;
            s.workspace_id = workspace_id;
            s.workspace_dir = next_dir;
            if diff {
                s.selected_issue = None;
                s.view_stack.set_visible_child_name("list");
            }
            diff
        };
        if self.root.is_visible() && (changed || self.inner.borrow().repo_name.is_none()) {
            self.refresh();
        }
    }

    pub fn refresh(&self) {
        let (dir, state_filter) = {
            let s = self.inner.borrow();
            (s.workspace_dir.clone(), s.state_filter.clone())
        };
        let Some(dir) = dir.filter(|d| d.is_dir()) else {
            let mut s = self.inner.borrow_mut();
            s.repo_name = None;
            s.issues.clear();
            s.header_title.set_label("GitHub Issues");
            s.status_label.set_label("Workspace has no local folder.");
            drop(s);
            self.render_list();
            return;
        };

        self.inner
            .borrow()
            .status_label
            .set_label("Loading GitHub issues…");

        let panel = self.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let res = fetch_issues_for_dir(&dir, &state_filter);
            let _ = tx.send(res);
        });
        glib::timeout_add_local(Duration::from_millis(80), move || match rx.try_recv() {
            Ok(res) => {
                match res {
                    Ok((repo, issues)) => {
                        let count = issues.len();
                        let selected = {
                            let mut s = panel.inner.borrow_mut();
                            s.header_title.set_label(&format!("Issues · {repo}"));
                            s.status_label
                                .set_label(&format!("{count} issue(s) · {repo}"));
                            s.repo_name = Some(repo);
                            s.issues = issues;
                            s.selected_issue
                        };
                        panel.render_list();
                        if let Some(num) = selected {
                            panel.open_detail(num);
                        }
                    }
                    Err(err) => {
                        let mut s = panel.inner.borrow_mut();
                        s.repo_name = None;
                        s.issues.clear();
                        s.header_title.set_label("GitHub Issues");
                        s.status_label.set_label(&err);
                        drop(s);
                        panel.render_list();
                    }
                }
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
        });
    }

    fn render_list(&self) {
        let (list_box, issues, query) = {
            let s = self.inner.borrow();
            (
                s.list_box.clone(),
                s.issues.clone(),
                s.search_query.trim().to_ascii_lowercase(),
            )
        };
        while let Some(child) = list_box.first_child() {
            list_box.remove(&child);
        }

        for issue in issues {
            if !query.is_empty()
                && !issue.title.to_ascii_lowercase().contains(&query)
                && !issue.number.to_string().contains(&query)
                && !issue
                    .labels
                    .iter()
                    .any(|l| l.to_ascii_lowercase().contains(&query))
            {
                continue;
            }

            let card = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .build();
            card.add_css_class("limux-issue-card");

            let top = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(6)
                .build();
            let num_lbl = gtk::Label::new(Some(&format!("#{}", issue.number)));
            num_lbl.add_css_class("limux-issue-num");
            if !issue.is_open() {
                num_lbl.add_css_class("limux-issue-num-closed");
            }
            let title_lbl = gtk::Label::builder()
                .label(&issue.title)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            title_lbl.add_css_class("limux-issue-title");
            let comments_lbl = gtk::Label::new(Some(&format!("💬 {}", issue.comments.len())));
            comments_lbl.add_css_class("limux-issue-meta");
            top.append(&num_lbl);
            top.append(&title_lbl);
            top.append(&comments_lbl);
            card.append(&top);

            if !issue.labels.is_empty() {
                let labels_row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(4)
                    .build();
                for label in issue.labels.iter().take(4) {
                    let chip = gtk::Label::new(Some(label));
                    chip.add_css_class("limux-issue-label");
                    labels_row.append(&chip);
                }
                card.append(&labels_row);
            }

            let actions = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(4)
                .margin_top(2)
                .build();
            for agent in [IssueAgent::GeminiAgy, IssueAgent::Claude, IssueAgent::Codex] {
                let btn = gtk::Button::with_label(&format!("▶ {}", agent.label()));
                btn.add_css_class("limux-issue-action-btn");
                btn.set_tooltip_text(Some(&format!(
                    "Start {} on issue #{} with full context & notes",
                    agent.label(),
                    issue.number
                )));
                let panel = self.clone();
                let num = issue.number;
                btn.connect_clicked(move |_| {
                    panel.launch_agent_for_issue(num, agent);
                });
                actions.append(&btn);
            }
            let detail_btn = gtk::Button::with_label("Notes");
            detail_btn.add_css_class("limux-issue-action-btn");
            {
                let panel = self.clone();
                let num = issue.number;
                detail_btn.connect_clicked(move |_| {
                    panel.open_detail(num);
                });
            }
            actions.append(&detail_btn);
            card.append(&actions);

            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&card));
            list_box.append(&row);
        }
    }

    fn open_detail(&self, number: u64) {
        let (detail_box, view_stack, issue, repo) = {
            let mut s = self.inner.borrow_mut();
            s.selected_issue = Some(number);
            let Some(issue) = s.issues.iter().find(|i| i.number == number).cloned() else {
                return;
            };
            (
                s.detail_box.clone(),
                s.view_stack.clone(),
                issue,
                s.repo_name.clone().unwrap_or_default(),
            )
        };

        while let Some(child) = detail_box.first_child() {
            detail_box.remove(&child);
        }

        let nav_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        let back_btn = gtk::Button::with_label("← List");
        back_btn.add_css_class("flat");
        {
            let panel = self.clone();
            back_btn.connect_clicked(move |_| {
                let mut s = panel.inner.borrow_mut();
                s.selected_issue = None;
                s.view_stack.set_visible_child_name("list");
            });
        }
        let state_btn = gtk::Button::with_label(if issue.is_open() {
            "Close Issue"
        } else {
            "Reopen Issue"
        });
        state_btn.add_css_class("limux-issue-action-btn");
        state_btn.set_halign(gtk::Align::End);
        state_btn.set_hexpand(true);
        {
            let panel = self.clone();
            let close = issue.is_open();
            state_btn.connect_clicked(move |btn| {
                let (Some(dir), Some(repo)) = ({
                    let s = panel.inner.borrow();
                    (s.workspace_dir.clone(), s.repo_name.clone())
                }) else {
                    return;
                };
                btn.set_sensitive(false);
                let panel_clone = panel.clone();
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(set_issue_state(&dir, &repo, number, close));
                });
                glib::timeout_add_local(Duration::from_millis(80), move || match rx.try_recv() {
                    Ok(_) => {
                        panel_clone.refresh();
                        glib::ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
                });
            });
        }
        nav_row.append(&back_btn);
        nav_row.append(&state_btn);
        detail_box.append(&nav_row);

        let heading = gtk::Label::builder()
            .label(format!("#{} · {}", issue.number, issue.title))
            .xalign(0.0)
            .wrap(true)
            .build();
        heading.add_css_class("limux-issue-title");
        detail_box.append(&heading);

        let agent_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .build();
        for agent in [IssueAgent::GeminiAgy, IssueAgent::Claude, IssueAgent::Codex] {
            let btn = gtk::Button::with_label(&format!("▶ {}", agent.label()));
            btn.add_css_class("limux-issue-action-btn");
            let panel = self.clone();
            btn.connect_clicked(move |_| {
                panel.launch_agent_for_issue(number, agent);
            });
            agent_row.append(&btn);
        }
        detail_box.append(&agent_row);

        let body_text = if issue.body.trim().is_empty() {
            "(No description)".to_string()
        } else {
            issue.body.trim().to_string()
        };
        let body_lbl = gtk::Label::builder()
            .label(&body_text)
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        body_lbl.add_css_class("limux-issue-detail-body");
        detail_box.append(&body_lbl);

        if !issue.comments.is_empty() {
            let c_head = gtk::Label::builder()
                .label(format!("GitHub Comments ({})", issue.comments.len()))
                .xalign(0.0)
                .build();
            c_head.add_css_class("limux-issue-num");
            detail_box.append(&c_head);
            for comment in &issue.comments {
                let c_lbl = gtk::Label::builder()
                    .label(format!("@{}: {}", comment.author, comment.body.trim()))
                    .xalign(0.0)
                    .wrap(true)
                    .selectable(true)
                    .build();
                c_lbl.add_css_class("limux-issue-detail-body");
                detail_box.append(&c_lbl);
            }
        }

        let notes = load_issue_notes(&repo, number);
        let n_head = gtk::Label::builder()
            .label("Shared AI Notes")
            .xalign(0.0)
            .build();
        n_head.add_css_class("limux-issue-num");
        detail_box.append(&n_head);

        let notes_lbl = gtk::Label::builder()
            .label(if notes.trim().is_empty() {
                "No AI notes yet. Add a note below so all agents share context."
            } else {
                notes.trim()
            })
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        notes_lbl.add_css_class("limux-issue-detail-body");
        detail_box.append(&notes_lbl);

        let note_entry = gtk::Entry::builder()
            .placeholder_text("Add note for AI agents & issue...")
            .build();
        let post_gh_check = gtk::CheckButton::with_label("Also post as GitHub comment");
        post_gh_check.set_active(true);
        let save_note_btn = gtk::Button::with_label("Save Note");
        save_note_btn.add_css_class("suggested-action");
        {
            let panel = self.clone();
            let note_entry_c = note_entry.clone();
            let post_gh_c = post_gh_check.clone();
            save_note_btn.connect_clicked(move |btn| {
                let text = note_entry_c.text().trim().to_string();
                if text.is_empty() {
                    return;
                }
                let (Some(dir), Some(repo)) = ({
                    let s = panel.inner.borrow();
                    (s.workspace_dir.clone(), s.repo_name.clone())
                }) else {
                    return;
                };
                let _ = append_issue_note(&repo, number, "user", &text);
                note_entry_c.set_text("");
                if post_gh_c.is_active() {
                    btn.set_sensitive(false);
                    let panel_clone = panel.clone();
                    let (tx, rx) = mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(post_github_comment(&dir, &repo, number, &text));
                    });
                    glib::timeout_add_local(Duration::from_millis(80), move || {
                        match rx.try_recv() {
                            Ok(_) => {
                                panel_clone.refresh();
                                glib::ControlFlow::Break
                            }
                            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                            Err(mpsc::TryRecvError::Disconnected) => glib::ControlFlow::Break,
                        }
                    });
                } else {
                    panel.open_detail(number);
                }
            });
        }
        detail_box.append(&note_entry);
        detail_box.append(&post_gh_check);
        detail_box.append(&save_note_btn);

        view_stack.set_visible_child_name("detail");
    }

    fn launch_agent_for_issue(&self, number: u64, agent: IssueAgent) {
        let (ws_id, repo, issue, on_launch) = {
            let s = self.inner.borrow();
            let Some(ws_id) = s.workspace_id.clone() else {
                return;
            };
            let Some(repo) = s.repo_name.clone() else {
                return;
            };
            let Some(issue) = s.issues.iter().find(|i| i.number == number).cloned() else {
                return;
            };
            (ws_id, repo, issue, s.on_launch_agent.clone())
        };

        let Ok((context_file, notes_file)) = write_full_issue_context_file(&repo, &issue) else {
            return;
        };
        let command = build_issue_agent_command(agent, &repo, &issue, &context_file, &notes_file);
        let tab_title = format!("#{} {}", issue.number, agent.label());
        on_launch(ws_id, tab_title, command);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        append_issue_note_to_path, build_issue_agent_command, parse_gh_issues_json,
        parse_github_repo_from_remote, render_issue_context_markdown, GitHubIssue, IssueAgent,
        IssueComment,
    };
    use std::path::Path;

    #[test]
    fn parses_https_and_ssh_github_remotes() {
        assert_eq!(
            parse_github_repo_from_remote("https://github.com/hasanemretonguc/limux.git"),
            Some("hasanemretonguc/limux".to_string())
        );
        assert_eq!(
            parse_github_repo_from_remote("git@github.com:am-will/limux.git"),
            Some("am-will/limux".to_string())
        );
        assert_eq!(
            parse_github_repo_from_remote("https://gitlab.com/a/b"),
            None
        );
    }

    #[test]
    fn parses_gh_issue_list_json_and_renders_context() {
        let raw = r#"[{
            "number": 14,
            "title": "Fix auth refresh",
            "state": "OPEN",
            "body": "Token expires silently.",
            "url": "https://github.com/org/repo/issues/14",
            "updatedAt": "2026-11-22T00:00:00Z",
            "labels": [{"name": "bug"}],
            "comments": [{"author": {"login": "dev"}, "body": "Reproduced", "createdAt": "2026-11-22T01:00:00Z"}]
        }]"#;
        let issues = parse_gh_issues_json(raw).expect("parse issues");
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].number, 14);
        assert_eq!(issues[0].labels, vec!["bug"]);
        assert_eq!(issues[0].comments.len(), 1);

        let md = render_issue_context_markdown("org/repo", &issues[0], "Check refresh token");
        assert!(md.contains("# GitHub Issue org/repo#14: Fix auth refresh"));
        assert!(md.contains("Token expires silently."));
        assert!(md.contains("@dev"));
        assert!(md.contains("Check refresh token"));
    }

    #[test]
    fn builds_quoted_agent_command_with_lifecycle_instructions() {
        let issue = GitHubIssue {
            number: 7,
            title: "Fix 'quotes' & spaces".to_string(),
            state: "OPEN".to_string(),
            body: String::new(),
            url: String::new(),
            labels: Vec::new(),
            comments: Vec::<IssueComment>::new(),
            updated_at: String::new(),
        };
        let cmd = build_issue_agent_command(
            IssueAgent::Claude,
            "org/repo",
            &issue,
            Path::new("/tmp/ctx.md"),
            Path::new("/tmp/notes.md"),
        );
        assert!(cmd.starts_with("claude "));
        assert!(cmd.contains("gh issue close 7 --repo org/repo"));
        assert!(cmd.contains("/tmp/ctx.md"));
        assert!(cmd.contains("/tmp/notes.md"));
    }

    #[test]
    fn appends_shared_notes_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("issue-1-notes.md");
        append_issue_note_to_path(&path, "user", "Check edge case").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("Check edge case"));
    }
}
