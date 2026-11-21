use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::app_config::AiTabTitleBackend;

const MAX_CONTEXT_LINES: usize = 24;
const MAX_CONTEXT_CHARS: usize = 1600;
const MAX_TITLE_CHARS: usize = 32;
const CLI_TIMEOUT: Duration = Duration::from_secs(10);

/// Extract a compact tail snippet from terminal screen text for title summarization.
pub fn extract_screen_context(screen_text: &str) -> String {
    let lines: Vec<&str> = screen_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let start = lines.len().saturating_sub(MAX_CONTEXT_LINES);
    let mut joined = lines[start..].join("\n");
    if joined.len() > MAX_CONTEXT_CHARS {
        let mut cut = joined.len() - MAX_CONTEXT_CHARS;
        while cut < joined.len() && !joined.is_char_boundary(cut) {
            cut += 1;
        }
        joined = joined[cut..].to_string();
    }
    joined
}

/// Compute a stable hash of the terminal context so we only invoke the AI model
/// when the working state actually changes.
pub fn context_fingerprint(
    screen_context: &str,
    cwd: Option<&str>,
    agent_label: Option<&str>,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    screen_context.hash(&mut hasher);
    cwd.hash(&mut hasher);
    agent_label.hash(&mut hasher);
    hasher.finish()
}

/// Invoke the configured lightweight CLI (`gemini` or `claude --model haiku`)
/// to generate a concise 2-4 word status title for the terminal tab.
///
/// Must be called from a background thread (blocking subprocess with timeout).
pub fn generate_tab_title(
    backend: AiTabTitleBackend,
    screen_context: &str,
    cwd: Option<&str>,
    agent_label: Option<&str>,
) -> Option<String> {
    if backend == AiTabTitleBackend::Off || screen_context.trim().is_empty() {
        return None;
    }

    let prompt = build_title_prompt(screen_context, cwd, agent_label);
    let raw = match backend {
        AiTabTitleBackend::Off => return None,
        AiTabTitleBackend::Gemini => run_gemini_cli(&prompt)?,
        AiTabTitleBackend::ClaudeHaiku => run_claude_haiku_cli(&prompt)?,
        AiTabTitleBackend::Auto => {
            run_gemini_cli(&prompt).or_else(|| run_claude_haiku_cli(&prompt))?
        }
    };
    sanitize_generated_title(&raw)
}

fn build_title_prompt(
    screen_context: &str,
    cwd: Option<&str>,
    agent_label: Option<&str>,
) -> String {
    let folder = cwd
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let agent = agent_label.unwrap_or("shell");
    format!(
        "Generate a concise 2 to 4 word terminal tab title summarizing the current task or status. \
         Output ONLY the plain title text (no quotes, no markdown, no punctuation, max 30 chars).\n\
         Tool: {agent}\n\
         Folder: {folder}\n\
         Terminal output:\n{screen_context}"
    )
}

fn run_gemini_cli(prompt: &str) -> Option<String> {
    run_command_with_timeout("gemini", &["-p", prompt], CLI_TIMEOUT)
}

fn run_claude_haiku_cli(prompt: &str) -> Option<String> {
    run_command_with_timeout(
        "claude",
        &["--bare", "-p", "--model", "haiku", prompt],
        CLI_TIMEOUT,
    )
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
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                return None;
            }
        }
    }
}

/// Clean raw LLM output into a safe, concise single-line tab title.
pub fn sanitize_generated_title(raw: &str) -> Option<String> {
    let first_line = raw.lines().map(str::trim).find(|line| !line.is_empty())?;

    let stripped = first_line
        .trim_start_matches(['-', '*', '#', '>', ' '])
        .strip_prefix("Title:")
        .or_else(|| first_line.strip_prefix("title:"))
        .unwrap_or(first_line)
        .trim()
        .trim_matches(['"', '\'', '`', '.', ':', ' ']);

    let collapsed: String = stripped.split_whitespace().collect::<Vec<_>>().join(" ");

    if collapsed.is_empty() {
        return None;
    }

    let truncated: String = collapsed.chars().take(MAX_TITLE_CHARS).collect();
    let trimmed = truncated.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::{context_fingerprint, extract_screen_context, sanitize_generated_title};

    #[test]
    fn extracts_tail_of_non_empty_lines() {
        let screen = "\n\nline 1\n   \nline 2\nline 3\n";
        assert_eq!(extract_screen_context(screen), "line 1\nline 2\nline 3");
    }

    #[test]
    fn sanitizes_llm_output_into_concise_title() {
        assert_eq!(
            sanitize_generated_title("  \"Fix Auth Token\"  \nextra line"),
            Some("Fix Auth Token".to_string())
        );
        assert_eq!(
            sanitize_generated_title("Title: `Cargo Test Pass`."),
            Some("Cargo Test Pass".to_string())
        );
        assert_eq!(sanitize_generated_title("   \n  "), None);
        let long =
            sanitize_generated_title("This is a very long generated title that exceeds limit")
                .unwrap();
        assert!(long.chars().count() <= 32);
    }

    #[test]
    fn fingerprint_changes_with_context() {
        let a = context_fingerprint("cargo test", Some("/a"), Some("Claude"));
        let b = context_fingerprint("cargo build", Some("/a"), Some("Claude"));
        assert_ne!(a, b);
    }
}
