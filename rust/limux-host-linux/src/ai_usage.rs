use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use limux_control::agent_sessions::AgentKind;
use serde_json::Value;

const TODAY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
const QUOTA_CACHE_TTL: Duration = Duration::from_secs(60);
const QUOTA_CMD_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_CLAUDE_FILES_TO_SCAN: usize = 30;
const MAX_AGY_FILES_TO_SCAN: usize = 25;

pub const AI_USAGE_CSS: &str = r#"
.limux-ai-usage-badge {
    font-size: 11px;
    font-weight: 600;
    padding: 2px 8px;
    margin: 2px 4px;
    border-radius: 4px;
    background: alpha(@accent_bg_color, 0.14);
    color: @window_fg_color;
    border: 1px solid alpha(@accent_bg_color, 0.35);
}
.limux-ai-usage-badge-claude {
    background: alpha(#e37057, 0.14);
    border-color: alpha(#e37057, 0.40);
}
.limux-ai-usage-badge-agy {
    background: alpha(#69b2e8, 0.14);
    border-color: alpha(#69b2e8, 0.40);
}
.limux-ai-usage-badge-codex {
    background: alpha(#10b981, 0.14);
    border-color: alpha(#10b981, 0.40);
}
.limux-ai-usage-badge-gemini {
    background: alpha(#a78bfa, 0.14);
    border-color: alpha(#a78bfa, 0.40);
}
"#;

#[derive(Clone, Debug, PartialEq)]
pub struct QuotaWindow {
    pub used_percent: f64,
    pub resets_at: Option<String>,
}

impl QuotaWindow {
    pub fn remaining_percent(&self) -> f64 {
        (100.0 - self.used_percent).clamp(0.0, 100.0)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuotaSnapshot {
    pub five_hour: Option<QuotaWindow>,
    pub seven_day: Option<QuotaWindow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AiToolUsage {
    pub agent: AgentKind,
    pub model: Option<String>,
    pub five_hour: Option<QuotaWindow>,
    pub seven_day: Option<QuotaWindow>,
    pub session_input_tokens: u64,
    pub session_output_tokens: u64,
    pub session_cache_read_tokens: u64,
    pub session_cost_usd: Option<f64>,
    pub today_total_tokens: u64,
    pub today_cost_usd: Option<f64>,
}

impl AiToolUsage {
    pub fn session_total_tokens(&self) -> u64 {
        self.session_input_tokens
            .saturating_add(self.session_output_tokens)
            .saturating_add(self.session_cache_read_tokens)
    }

    pub fn css_variant_class(&self) -> &'static str {
        match self.agent {
            AgentKind::Claude => "limux-ai-usage-badge-claude",
            AgentKind::Agy => "limux-ai-usage-badge-agy",
            AgentKind::Codex => "limux-ai-usage-badge-codex",
            AgentKind::Gemini | AgentKind::OpenCode => "limux-ai-usage-badge-gemini",
        }
    }

    pub fn short_agent_name(&self) -> &'static str {
        match self.agent {
            AgentKind::Claude => "Claude",
            AgentKind::Agy => "Gemini",
            AgentKind::Codex => "Codex",
            AgentKind::Gemini => "Gemini",
            AgentKind::OpenCode => "OpenCode",
        }
    }

    pub fn format_badge(&self) -> String {
        let name = self.short_agent_name();
        match (&self.five_hour, &self.seven_day) {
            (Some(fh), Some(sd)) => format!(
                "{name} · 5h {:.0}% · 7d {:.0}%",
                fh.used_percent.clamp(0.0, 100.0),
                sd.used_percent.clamp(0.0, 100.0)
            ),
            (Some(fh), None) => format!("{name} · 5h {:.0}%", fh.used_percent.clamp(0.0, 100.0)),
            (None, Some(sd)) => format!("{name} · 7d {:.0}%", sd.used_percent.clamp(0.0, 100.0)),
            (None, None) => {
                let tok = self.session_total_tokens();
                match (tok > 0, self.session_cost_usd) {
                    (true, Some(cost)) => format!("{name} · {} · ${cost:.2}", format_tokens(tok)),
                    (true, None) => format!("{name} · {}", format_tokens(tok)),
                    (false, Some(cost)) => format!("{name} · ${cost:.2}"),
                    (false, None) if self.today_total_tokens > 0 => {
                        format!("{name} · today {}", format_tokens(self.today_total_tokens))
                    }
                    (false, None) => format!("{name} · active"),
                }
            }
        }
    }

    pub fn format_tooltip(&self) -> String {
        let tool_name = match self.agent {
            AgentKind::Agy => "Gemini (Antigravity)",
            other => other.label(),
        };
        let mut lines = vec![format!("Active AI Tool: {tool_name}")];
        if let Some(model) = self.model.as_deref().filter(|m| !m.is_empty()) {
            lines.push(format!("Model: {model}"));
        }
        if let Some(fh) = &self.five_hour {
            let mut line = format!(
                "5-hour usage: {:.0}% used ({:.0}% left)",
                fh.used_percent.clamp(0.0, 100.0),
                fh.remaining_percent()
            );
            if let Some(reset) = fh.resets_at.as_deref().filter(|s| !s.is_empty()) {
                line.push_str(&format!(" · resets {reset}"));
            }
            lines.push(line);
        }
        if let Some(sd) = &self.seven_day {
            let mut line = format!(
                "Weekly usage: {:.0}% used ({:.0}% left)",
                sd.used_percent.clamp(0.0, 100.0),
                sd.remaining_percent()
            );
            if let Some(reset) = sd.resets_at.as_deref().filter(|s| !s.is_empty()) {
                line.push_str(&format!(" · resets {reset}"));
            }
            lines.push(line);
        }
        let session_total = self.session_total_tokens();
        if session_total > 0 {
            lines.push(format!(
                "Session tokens: {} (in {} / out {} / cache {})",
                format_tokens(session_total),
                format_tokens(self.session_input_tokens),
                format_tokens(self.session_output_tokens),
                format_tokens(self.session_cache_read_tokens),
            ));
        }
        if let Some(cost) = self.session_cost_usd {
            lines.push(format!("Session cost: ${cost:.4}"));
        }
        if self.today_total_tokens > 0 || self.today_cost_usd.is_some() {
            let mut today_parts = Vec::new();
            if self.today_total_tokens > 0 {
                today_parts.push(format!("{} tok", format_tokens(self.today_total_tokens)));
            }
            if let Some(cost) = self.today_cost_usd {
                today_parts.push(format!("${cost:.2}"));
            }
            lines.push(format!("Last 24h: {}", today_parts.join(" · ")));
        }
        lines.join("\n")
    }
}

pub fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

#[derive(Clone, Debug)]
pub struct ActiveAgentContext {
    pub agent: AgentKind,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

/// Detect which AI tool is active on the focused surface/workspace using
/// Limux's hook session files first, falling back to tab title heuristics.
pub fn detect_active_agent(
    surface_id: Option<&str>,
    workspace_id: Option<&str>,
    tab_title: Option<&str>,
    cwd: Option<&str>,
) -> Option<ActiveAgentContext> {
    if let Some(state_dir) = limux_state_dir() {
        if let Some(found) = detect_active_agent_in_state_dir(&state_dir, surface_id, workspace_id)
        {
            return Some(found);
        }
    }

    let title = tab_title?.to_ascii_lowercase();
    let agent = if title.contains("claude") {
        AgentKind::Claude
    } else if title.contains("antigravity") || title == "agy" || title.starts_with("agy ") {
        AgentKind::Agy
    } else if title.contains("codex") {
        AgentKind::Codex
    } else if title.contains("gemini") {
        AgentKind::Gemini
    } else if title.contains("opencode") {
        AgentKind::OpenCode
    } else {
        return None;
    };

    Some(ActiveAgentContext {
        agent,
        session_id: None,
        cwd: cwd.map(str::to_owned),
    })
}

fn limux_state_dir() -> Option<PathBuf> {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .map(|d| d.join("limux"))
}

fn detect_active_agent_in_state_dir(
    state_dir: &Path,
    surface_id: Option<&str>,
    workspace_id: Option<&str>,
) -> Option<ActiveAgentContext> {
    let mut best_surface: Option<(f64, ActiveAgentContext)> = None;
    let mut best_workspace: Option<(f64, ActiveAgentContext)> = None;

    for agent in AgentKind::ALL {
        let path = state_dir.join(agent.hook_session_file_name());
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(root) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(sessions) = root.get("sessions").and_then(Value::as_object) else {
            continue;
        };

        for (session_key, record) in sessions {
            let updated_at = record
                .get("updated_at")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let rec_surface = record.get("surface_id").and_then(Value::as_str);
            let rec_workspace = record.get("workspace_id").and_then(Value::as_str);
            let rec_cwd = record.get("cwd").and_then(Value::as_str).map(str::to_owned);
            let session_id = record
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or(session_key)
                .to_string();

            if surface_id.is_some() && rec_surface == surface_id {
                if best_surface
                    .as_ref()
                    .is_none_or(|(prev, _)| updated_at > *prev)
                {
                    best_surface = Some((
                        updated_at,
                        ActiveAgentContext {
                            agent,
                            session_id: Some(session_id),
                            cwd: rec_cwd,
                        },
                    ));
                }
            } else if workspace_id.is_some()
                && rec_workspace == workspace_id
                && best_workspace
                    .as_ref()
                    .is_none_or(|(prev, _)| updated_at > *prev)
            {
                best_workspace = Some((
                    updated_at,
                    ActiveAgentContext {
                        agent,
                        session_id: Some(session_id),
                        cwd: rec_cwd,
                    },
                ));
            }
        }
    }

    best_surface.or(best_workspace).map(|(_, ctx)| ctx)
}

/// Collect usage statistics (including cached 5h/7d quota percentages) for the
/// detected active AI tool. Runs on a background thread.
pub fn collect_usage_for_context(ctx: &ActiveAgentContext) -> AiToolUsage {
    let home = dirs::home_dir();
    match ctx.agent {
        AgentKind::Claude => collect_claude_usage(home.as_deref(), ctx),
        AgentKind::Agy => collect_agy_usage(home.as_deref(), ctx),
        other => AiToolUsage {
            agent: other,
            model: None,
            five_hour: None,
            seven_day: None,
            session_input_tokens: 0,
            session_output_tokens: 0,
            session_cache_read_tokens: 0,
            session_cost_usd: None,
            today_total_tokens: 0,
            today_cost_usd: None,
        },
    }
}

fn cached_agy_quota() -> QuotaSnapshot {
    static CACHE: OnceLock<Mutex<Option<(Instant, QuotaSnapshot)>>> = OnceLock::new();
    let lock = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = lock.lock() {
        if let Some((fetched_at, snapshot)) = guard.as_ref() {
            if fetched_at.elapsed() < QUOTA_CACHE_TTL {
                return snapshot.clone();
            }
        }
    }
    let fresh = fetch_agy_quota_via_cli().unwrap_or_default();
    if let Ok(mut guard) = lock.lock() {
        *guard = Some((Instant::now(), fresh.clone()));
    }
    fresh
}

fn fetch_agy_quota_via_cli() -> Option<QuotaSnapshot> {
    let stdout = run_command_with_timeout("agy", &["-p", "/usage"], QUOTA_CMD_TIMEOUT)?;
    parse_agy_usage_output(&stdout)
}

/// Parse `agy -p /usage` tab-separated output into 5-hour and weekly usage
/// percentages (`used = 100 - remaining`).
pub fn parse_agy_usage_output(output: &str) -> Option<QuotaSnapshot> {
    let mut snapshot = QuotaSnapshot::default();

    for raw_line in output.lines() {
        let cols: Vec<&str> = raw_line.split('\t').map(str::trim).collect();
        if cols.len() < 3 {
            continue;
        }
        let group = cols[0].to_ascii_lowercase();
        let metric = cols[1].to_ascii_lowercase();
        let Some(pct) = cols[2]
            .strip_suffix('%')
            .and_then(|v| v.trim().parse::<f64>().ok())
        else {
            continue;
        };
        let resets_at = cols
            .get(3)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        // Prefer the "Gemini Models" bucket for Antigravity Gemini usage.
        if !group.contains("gemini models") {
            continue;
        }
        let used_percent = if metric.contains("remaining") {
            (100.0 - pct).clamp(0.0, 100.0)
        } else {
            pct.clamp(0.0, 100.0)
        };

        if metric.contains("five hour") || metric.contains("5 hour") || metric.contains("5h") {
            snapshot.five_hour = Some(QuotaWindow {
                used_percent,
                resets_at,
            });
        } else if metric.contains("weekly") || metric.contains("7 day") || metric.contains("7d") {
            snapshot.seven_day = Some(QuotaWindow {
                used_percent,
                resets_at,
            });
        }
    }

    (snapshot.five_hour.is_some() || snapshot.seven_day.is_some()).then_some(snapshot)
}

fn cached_claude_quota(home: Option<&Path>) -> QuotaSnapshot {
    // Allow an optional local statusline-written rate limits file to win immediately.
    if let Some(state_dir) = limux_state_dir() {
        let local_path = state_dir.join("claude-rate-limits.json");
        if let Ok(raw) = fs::read_to_string(&local_path) {
            if let Some(parsed) = parse_claude_oauth_usage_json(&raw) {
                return parsed;
            }
        }
    }

    static CACHE: OnceLock<Mutex<Option<(Instant, QuotaSnapshot)>>> = OnceLock::new();
    let lock = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = lock.lock() {
        if let Some((fetched_at, snapshot)) = guard.as_ref() {
            if fetched_at.elapsed() < QUOTA_CACHE_TTL {
                return snapshot.clone();
            }
        }
    }
    let fresh = fetch_claude_quota_via_oauth(home).unwrap_or_default();
    if let Ok(mut guard) = lock.lock() {
        *guard = Some((Instant::now(), fresh.clone()));
    }
    fresh
}

fn fetch_claude_quota_via_oauth(home: Option<&Path>) -> Option<QuotaSnapshot> {
    let creds_path = home?.join(".claude/.credentials.json");
    let raw = fs::read_to_string(creds_path).ok()?;
    let root: Value = serde_json::from_str(&raw).ok()?;
    let token = root
        .get("claudeAiOauth")
        .and_then(|o| o.get("accessToken"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())?;

    let auth_header = format!("Authorization: Bearer {token}");
    let stdout = run_command_with_timeout(
        "curl",
        &[
            "-sf",
            "--max-time",
            "5",
            "-H",
            &auth_header,
            "-H",
            "anthropic-beta: oauth-2025-04-20",
            "-H",
            "User-Agent: claude-code/2.1",
            "https://api.anthropic.com/api/oauth/usage",
        ],
        QUOTA_CMD_TIMEOUT,
    )?;
    parse_claude_oauth_usage_json(&stdout)
}

/// Parse Claude `/api/oauth/usage` or statusline `rate_limits` JSON into 5h/7d windows.
pub fn parse_claude_oauth_usage_json(raw: &str) -> Option<QuotaSnapshot> {
    let root: Value = serde_json::from_str(raw).ok()?;
    let container = root.get("rate_limits").unwrap_or(&root);

    let parse_window = |obj: &Value| -> Option<QuotaWindow> {
        let used = obj
            .get("utilization")
            .or_else(|| obj.get("used_percentage"))
            .and_then(Value::as_f64)?;
        let resets_at = obj
            .get("resets_at")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Some(QuotaWindow {
            used_percent: used.clamp(0.0, 100.0),
            resets_at,
        })
    };

    let five_hour = container.get("five_hour").and_then(parse_window);
    let seven_day = container.get("seven_day").and_then(parse_window);
    (five_hour.is_some() || seven_day.is_some()).then_some(QuotaSnapshot {
        five_hour,
        seven_day,
    })
}

fn run_command_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let output = child.wait_with_output().ok()?;
                return String::from_utf8(output.stdout).ok();
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(_) => {
                let _ = child.kill();
                return None;
            }
        }
    }
}

fn collect_claude_usage(home: Option<&Path>, ctx: &ActiveAgentContext) -> AiToolUsage {
    let quota = cached_claude_quota(home);
    let mut usage = AiToolUsage {
        agent: AgentKind::Claude,
        model: None,
        five_hour: quota.five_hour,
        seven_day: quota.seven_day,
        session_input_tokens: 0,
        session_output_tokens: 0,
        session_cache_read_tokens: 0,
        session_cost_usd: None,
        today_total_tokens: 0,
        today_cost_usd: None,
    };
    let Some(projects_dir) = home
        .map(|h| h.join(".claude/projects"))
        .filter(|p| p.is_dir())
    else {
        return usage;
    };

    let mut jsonl_files = collect_recent_files_by_ext(&projects_dir, "jsonl", 2);
    jsonl_files.sort_by_key(|a| std::cmp::Reverse(a.1));

    let session_file = ctx
        .session_id
        .as_deref()
        .and_then(|sid| {
            let expected = format!("{sid}.jsonl");
            jsonl_files
                .iter()
                .find(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some(&expected))
                .map(|(p, _)| p.clone())
        })
        .or_else(|| {
            ctx.cwd.as_deref().and_then(|cwd| {
                let encoded = encode_claude_project_dir(cwd);
                jsonl_files
                    .iter()
                    .find(|(p, _)| {
                        p.parent()
                            .and_then(|d| d.file_name())
                            .and_then(|n| n.to_str())
                            == Some(&encoded)
                    })
                    .map(|(p, _)| p.clone())
            })
        })
        .or_else(|| jsonl_files.first().map(|(p, _)| p.clone()));

    if let Some(path) = session_file {
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Some(parsed) = parse_claude_jsonl_usage(&contents) {
                usage.model = parsed.model;
                usage.session_input_tokens = parsed.input_tokens;
                usage.session_output_tokens = parsed.output_tokens;
                usage.session_cache_read_tokens = parsed.cache_read_tokens;
                usage.session_cost_usd = parsed.cost_usd;
            }
        }
    }

    let cutoff = SystemTime::now().checked_sub(TODAY_WINDOW);
    let mut today_tokens = 0u64;
    let mut today_cost = 0.0f64;
    let mut any_cost = false;
    for (path, mtime) in jsonl_files.into_iter().take(MAX_CLAUDE_FILES_TO_SCAN) {
        if cutoff.is_some_and(|c| mtime < c) {
            break;
        }
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Some(parsed) = parse_claude_jsonl_usage(&contents) {
                today_tokens = today_tokens
                    .saturating_add(parsed.input_tokens)
                    .saturating_add(parsed.output_tokens)
                    .saturating_add(parsed.cache_read_tokens);
                if let Some(cost) = parsed.cost_usd {
                    today_cost += cost;
                    any_cost = true;
                }
            }
        }
    }
    usage.today_total_tokens = today_tokens;
    usage.today_cost_usd = any_cost.then_some(today_cost);
    usage
}

fn encode_claude_project_dir(cwd: &str) -> String {
    cwd.replace('/', "-")
}

#[derive(Default)]
pub struct ParsedTokenUsage {
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: Option<f64>,
}

pub fn parse_claude_jsonl_usage(contents: &str) -> Option<ParsedTokenUsage> {
    let mut last_cost_state: Option<ParsedTokenUsage> = None;
    let mut fallback = ParsedTokenUsage::default();
    let mut saw_fallback = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        if !trimmed.contains("cost-state") && !trimmed.contains("\"usage\"") {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if val.get("type").and_then(Value::as_str) == Some("cost-state") {
            let mut state = ParsedTokenUsage {
                cost_usd: val.get("totalCostUSD").and_then(Value::as_f64),
                ..ParsedTokenUsage::default()
            };
            if let Some(models) = val.get("modelUsage").and_then(Value::as_object) {
                let mut top_model: Option<(u64, String)> = None;
                for (model_name, stats) in models {
                    let inp = stats
                        .get("inputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let out = stats
                        .get("outputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let cache = stats
                        .get("cacheReadInputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    state.input_tokens = state.input_tokens.saturating_add(inp);
                    state.output_tokens = state.output_tokens.saturating_add(out);
                    state.cache_read_tokens = state.cache_read_tokens.saturating_add(cache);
                    let total_for_model = inp + out + cache;
                    if top_model
                        .as_ref()
                        .is_none_or(|(prev, _)| total_for_model > *prev)
                    {
                        top_model = Some((total_for_model, model_name.clone()));
                    }
                }
                state.model = top_model.map(|(_, m)| m);
            }
            last_cost_state = Some(state);
        } else if let Some(msg_usage) = val
            .get("message")
            .and_then(|m| m.get("usage"))
            .or_else(|| val.get("usage"))
        {
            let inp = msg_usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let out = msg_usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let cache = msg_usage
                .get("cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if inp > 0 || out > 0 || cache > 0 {
                saw_fallback = true;
                fallback.input_tokens = fallback.input_tokens.saturating_add(inp);
                fallback.output_tokens = fallback.output_tokens.saturating_add(out);
                fallback.cache_read_tokens = fallback.cache_read_tokens.saturating_add(cache);
                if let Some(model) = val
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                {
                    fallback.model = Some(model.to_string());
                }
            }
        }
    }

    last_cost_state.or_else(|| saw_fallback.then_some(fallback))
}

fn collect_agy_usage(home: Option<&Path>, ctx: &ActiveAgentContext) -> AiToolUsage {
    let quota = cached_agy_quota();
    let mut usage = AiToolUsage {
        agent: AgentKind::Agy,
        model: None,
        five_hour: quota.five_hour,
        seven_day: quota.seven_day,
        session_input_tokens: 0,
        session_output_tokens: 0,
        session_cache_read_tokens: 0,
        session_cost_usd: None,
        today_total_tokens: 0,
        today_cost_usd: None,
    };
    let Some(brain_dir) = home
        .map(|h| h.join(".gemini/antigravity-cli/brain"))
        .filter(|p| p.is_dir())
    else {
        return usage;
    };

    let mut transcripts = Vec::new();
    if let Ok(entries) = fs::read_dir(&brain_dir) {
        for entry in entries.flatten() {
            let transcript = entry.path().join(".system_generated/logs/transcript.jsonl");
            if let Ok(meta) = fs::metadata(&transcript) {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                transcripts.push((
                    entry.file_name().to_string_lossy().to_string(),
                    transcript,
                    mtime,
                ));
            }
        }
    }
    transcripts.sort_by_key(|a| std::cmp::Reverse(a.2));

    let session_transcript = ctx
        .session_id
        .as_deref()
        .and_then(|sid| {
            transcripts
                .iter()
                .find(|(id, _, _)| id == sid)
                .map(|(_, path, _)| path.clone())
        })
        .or_else(|| transcripts.first().map(|(_, path, _)| path.clone()));

    if let Some(path) = session_transcript {
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Some(parsed) = parse_agy_transcript_usage(&contents) {
                usage.model = parsed.model;
                usage.session_input_tokens = parsed.input_tokens;
                usage.session_output_tokens = parsed.output_tokens;
                usage.session_cache_read_tokens = parsed.cache_read_tokens;
            }
        }
    }

    let cutoff = SystemTime::now().checked_sub(TODAY_WINDOW);
    let mut today_tokens = 0u64;
    for (_, path, mtime) in transcripts.into_iter().take(MAX_AGY_FILES_TO_SCAN) {
        if cutoff.is_some_and(|c| mtime < c) {
            break;
        }
        if let Ok(contents) = fs::read_to_string(&path) {
            if let Some(parsed) = parse_agy_transcript_usage(&contents) {
                today_tokens = today_tokens
                    .saturating_add(parsed.input_tokens)
                    .saturating_add(parsed.output_tokens)
                    .saturating_add(parsed.cache_read_tokens);
            }
        }
    }
    usage.today_total_tokens = today_tokens;
    usage
}

pub fn parse_agy_transcript_usage(contents: &str) -> Option<ParsedTokenUsage> {
    let mut total = ParsedTokenUsage::default();
    let mut saw_any = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') || !trimmed.contains("_tokens") {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let inp = val.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
        let out = val
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache = val
            .get("cache_read_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if inp > 0 || out > 0 || cache > 0 {
            saw_any = true;
            total.input_tokens = total.input_tokens.saturating_add(inp);
            total.output_tokens = total.output_tokens.saturating_add(out);
            total.cache_read_tokens = total.cache_read_tokens.saturating_add(cache);
            if let Some(model) = val.get("model").and_then(Value::as_str) {
                total.model = Some(model.to_string());
            }
        }
    }

    saw_any.then_some(total)
}

fn collect_recent_files_by_ext(
    root: &Path,
    ext: &str,
    max_depth: usize,
) -> Vec<(PathBuf, SystemTime)> {
    let mut out = Vec::new();
    collect_recent_files_recursive(root, ext, max_depth, &mut out);
    out
}

fn collect_recent_files_recursive(
    dir: &Path,
    ext: &str,
    depth_left: usize,
    out: &mut Vec<(PathBuf, SystemTime)>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth_left > 0 {
                collect_recent_files_recursive(&path, ext, depth_left - 1, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            out.push((path, mtime));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        detect_active_agent_in_state_dir, format_tokens, parse_agy_transcript_usage,
        parse_agy_usage_output, parse_claude_jsonl_usage, parse_claude_oauth_usage_json,
        AiToolUsage, QuotaWindow,
    };
    use limux_control::agent_sessions::AgentKind;
    use std::fs;

    #[test]
    fn parses_agy_usage_tsv_into_5h_and_weekly_used_percentages() {
        let sample = "\
Gemini Models\tWeekly Limit Remaining\t63%\t2026-11-28T09:35:29Z
Gemini Models\tFive Hour Limit Remaining\t46%\t2026-11-22T02:40:24Z
Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-11-28T22:28:59Z
";
        let quota = parse_agy_usage_output(sample).expect("should parse agy /usage");
        let fh = quota.five_hour.expect("5h quota");
        let sd = quota.seven_day.expect("7d quota");
        assert!((fh.used_percent - 54.0).abs() < 0.01);
        assert_eq!(fh.resets_at.as_deref(), Some("2026-11-22T02:40:24Z"));
        assert!((sd.used_percent - 37.0).abs() < 0.01);
        assert_eq!(sd.resets_at.as_deref(), Some("2026-11-28T09:35:29Z"));
    }

    #[test]
    fn parses_claude_oauth_usage_json() {
        let sample = r#"{
            "five_hour": { "utilization": 34.0, "resets_at": "2026-11-22T05:00:00Z" },
            "seven_day": { "utilization": 61.5, "resets_at": "2026-11-28T00:00:00Z" }
        }"#;
        let quota = parse_claude_oauth_usage_json(sample).expect("should parse claude quota");
        assert!((quota.five_hour.unwrap().used_percent - 34.0).abs() < 0.01);
        assert!((quota.seven_day.unwrap().used_percent - 61.5).abs() < 0.01);
    }

    #[test]
    fn parses_claude_cost_state_jsonl() {
        let sample = r#"
{"type":"session-owner","sessionId":"abc"}
{"type":"cost-state","sessionId":"abc","totalCostUSD":1.25,"modelUsage":{"claude-sonnet-5-5":{"inputTokens":1200,"outputTokens":800,"cacheReadInputTokens":50000,"costUSD":1.25}}}
"#;
        let parsed = parse_claude_jsonl_usage(sample).expect("should parse cost-state");
        assert_eq!(parsed.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(parsed.input_tokens, 1200);
        assert_eq!(parsed.output_tokens, 800);
        assert_eq!(parsed.cache_read_tokens, 50000);
        assert_eq!(parsed.cost_usd, Some(1.25));
    }

    #[test]
    fn parses_agy_transcript_jsonl() {
        let sample = r#"
{"step_index":1,"model":"gemini-4-neon-medium","input_tokens":1000,"cache_read_tokens":20000,"output_tokens":500}
{"step_index":2,"model":"gemini-4-neon-medium","input_tokens":500,"cache_read_tokens":10000,"output_tokens":250}
"#;
        let parsed = parse_agy_transcript_usage(sample).expect("should parse agy transcript");
        assert_eq!(parsed.model.as_deref(), Some("gemini-4-neon-medium"));
        assert_eq!(parsed.input_tokens, 1500);
        assert_eq!(parsed.output_tokens, 750);
        assert_eq!(parsed.cache_read_tokens, 30000);
    }

    #[test]
    fn formats_quota_badge_and_tooltip_and_token_fallback() {
        assert_eq!(format_tokens(950), "950");
        assert_eq!(format_tokens(42_150), "42.1k");
        assert_eq!(format_tokens(3_450_000), "3.5M");

        let gemini_quota = AiToolUsage {
            agent: AgentKind::Agy,
            model: Some("gemini-4-neon-medium".to_string()),
            five_hour: Some(QuotaWindow {
                used_percent: 54.0,
                resets_at: Some("2026-11-22T02:40:24Z".to_string()),
            }),
            seven_day: Some(QuotaWindow {
                used_percent: 37.0,
                resets_at: Some("2026-11-28T09:35:29Z".to_string()),
            }),
            session_input_tokens: 1_000,
            session_output_tokens: 500,
            session_cache_read_tokens: 40_500,
            session_cost_usd: None,
            today_total_tokens: 120_000,
            today_cost_usd: None,
        };
        assert_eq!(gemini_quota.format_badge(), "Gemini · 5h 54% · 7d 37%");
        let tooltip = gemini_quota.format_tooltip();
        assert!(tooltip.contains("Active AI Tool: Gemini (Antigravity)"));
        assert!(tooltip.contains("5-hour usage: 54% used (46% left)"));
        assert!(tooltip.contains("Weekly usage: 37% used (63% left)"));

        let claude_fallback = AiToolUsage {
            agent: AgentKind::Claude,
            model: Some("claude-sonnet-5-5".to_string()),
            five_hour: None,
            seven_day: None,
            session_input_tokens: 1_000,
            session_output_tokens: 500,
            session_cache_read_tokens: 40_500,
            session_cost_usd: Some(0.18),
            today_total_tokens: 120_000,
            today_cost_usd: Some(0.75),
        };
        assert_eq!(claude_fallback.format_badge(), "Claude · 42.0k · $0.18");
    }

    #[test]
    fn detects_active_agent_from_limux_hook_session_files() {
        let dir = tempfile::tempdir().unwrap();
        let agy_file = dir.path().join("agy-hook-sessions.json");
        fs::write(
            &agy_file,
            r#"{
                "version": 1,
                "sessions": {
                    "sess-1": {
                        "session_id": "sess-1",
                        "workspace_id": "ws-1",
                        "surface_id": "2:tab-1",
                        "cwd": "/tmp/proj",
                        "updated_at": 500.0
                    }
                }
            }"#,
        )
        .unwrap();

        let detected = detect_active_agent_in_state_dir(dir.path(), Some("2:tab-1"), Some("ws-1"))
            .expect("should detect agy");
        assert_eq!(detected.agent, AgentKind::Agy);
        assert_eq!(detected.session_id.as_deref(), Some("sess-1"));
        assert_eq!(detected.cwd.as_deref(), Some("/tmp/proj"));
    }
}
