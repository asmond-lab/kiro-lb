//! Optionally shorten the long tool descriptions Claude Code ships. Shortening
//! never removes a tool or touches a schema: a tool the model cannot see is a
//! tool it invents results for.

use parking_lot::Mutex;
use serde_json::{json, Value};

const MARKERS: &[&str] = &[
    "you are an interactive agent that helps users with software engineering tasks",
    "you are claude code",
    "anthropic's official cli",
    "# tone and style",
    "# doing tasks",
    "# using your tools",
    "# delivering work",
    "# harness",
];

const MIN_LENGTH: usize = 1500;

/// Kiro ends a response after a few minutes or ~32k output tokens, thinking
/// included, and delivers a long tool argument only when it is complete, so a
/// cut call loses the whole file. Fixed text keeps the prompt cache warm.
pub const WRITE_HINT: &str = "Long responses can be cut off before they finish, and a tool call that is cut off is lost entirely. Keep each call small: write a large file in parts of up to about 500 lines.";

const WRITE_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];

pub fn with_write_hint(name: &str, description: Option<String>) -> Option<String> {
    if !WRITE_TOOLS.contains(&name) {
        return description;
    }
    Some(match description {
        Some(d) if !d.is_empty() => format!("{d}\n\n{WRITE_HINT}"),
        _ => WRITE_HINT.to_owned(),
    })
}

pub fn is_claude_code_prompt(text: &str) -> bool {
    if text.chars().count() < MIN_LENGTH {
        return false;
    }
    let lowered = text.to_lowercase();
    MARKERS.iter().filter(|m| lowered.contains(*m)).count() >= 2
}

// ----- tool description shortening -------------------------------------------------------

pub const SHORTENED_MARKER: &str = "\n\n(…)";

/// Keeps the lead paragraph and every short line that names a required parameter,
/// cut at the last sentence boundary under `limit`.
pub fn shorten_description(description: &str, required: &[String], limit: usize) -> Option<String> {
    if description.chars().count() <= limit {
        return None;
    }
    let mut kept = String::new();
    let lead = description
        .split("\n\n")
        .find(|p| !p.trim().is_empty())
        .unwrap_or("")
        .trim();
    kept.push_str(&clip_sentence(lead, limit));
    for line in description.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || kept.contains(trimmed) || trimmed.chars().count() > 240 {
            continue;
        }
        let mentions_required = required
            .iter()
            .any(|r| trimmed.contains(&format!("`{r}`")) || trimmed.starts_with(&format!("- {r}")));
        let is_hard_rule = trimmed.starts_with("- IMPORTANT")
            || trimmed.starts_with("IMPORTANT:")
            || trimmed.starts_with("NEVER");
        if (mentions_required || is_hard_rule)
            && kept.chars().count() + trimmed.chars().count() + 1 < limit
        {
            kept.push('\n');
            kept.push_str(trimmed);
        }
    }
    kept.push_str(SHORTENED_MARKER);
    (kept.chars().count() < description.chars().count()).then_some(kept)
}

fn clip_sentence(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let cut: String = text.chars().take(limit).collect();
    match cut.rfind(". ") {
        Some(i) if i > limit / 3 => cut[..=i].to_owned(),
        _ => cut,
    }
}

#[derive(Default, Clone, Copy)]
pub struct ShortenStats {
    pub tools_seen: usize,
    pub tools_shortened: usize,
    pub bytes_before: usize,
    pub bytes_after: usize,
}

impl ShortenStats {
    pub fn as_json(&self) -> Value {
        json!({
            "toolsSeen": self.tools_seen,
            "toolsShortened": self.tools_shortened,
            "bytesBefore": self.bytes_before,
            "bytesAfter": self.bytes_after,
        })
    }
}

static LAST_SHORTEN: Mutex<Option<ShortenStats>> = Mutex::new(None);

pub fn record_shorten(stats: ShortenStats) {
    *LAST_SHORTEN.lock() = Some(stats);
}

pub fn last_stats() -> Value {
    let shorten = LAST_SHORTEN
        .lock()
        .map(|s| s.as_json())
        .unwrap_or(Value::Null);
    json!({"lastShorten": shorten})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_long_description_and_keeps_required_lines() {
        let long = format!(
            "Runs a command in the shell.\n\nDetails follow.\n- `command` is required and must be quoted.\n{}",
            "Extra guidance sentence. ".repeat(200)
        );
        let short = shorten_description(&long, &["command".into()], 1200).unwrap();
        assert!(short.starts_with("Runs a command in the shell."));
        assert!(short.contains("`command` is required"));
        assert!(short.ends_with(SHORTENED_MARKER));
        assert!(short.len() < 1400, "{}", short.len());
    }

    #[test]
    fn appends_the_write_hint_only_to_writing_tools() {
        let write = with_write_hint("Write", Some("Writes a file.".into())).unwrap();
        assert!(write.starts_with("Writes a file.\n\n"));
        assert!(write.ends_with(WRITE_HINT));
        assert_eq!(
            with_write_hint("Read", Some("Reads a file.".into())).as_deref(),
            Some("Reads a file.")
        );
    }

    #[test]
    fn leaves_short_descriptions_alone() {
        assert!(shorten_description("Reads a file.", &[], 1200).is_none());
    }
}
