//! Condense the Anthropic built-in Claude Code prompt, keeping what is the user's,
//! and optionally shorten the long tool descriptions Claude Code ships.
//!
//! Only sections recognised as generic Anthropic prose are dropped; unknown
//! sections survive. Tool shortening never removes a tool or touches a schema:
//! a tool the model cannot see is a tool it invents results for.

use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

pub const KIRO_IDENTITY: &str =
    "You are Kiro, an AI agent serving as the model backend for a coding CLI.";

pub fn kiro_preamble() -> &'static str {
    static P: OnceLock<String> = OnceLock::new();
    P.get_or_init(|| {
        format!(
            "{KIRO_IDENTITY}
You help with software engineering tasks in a terminal. Identify yourself as Kiro.

# Harness
- Text outside tool use renders as GitHub-flavored markdown in a terminal.
- Tools run behind a permission mode; a denied call means the user declined it — adjust, do not retry verbatim.
- Mid-conversation system turns may update rules and are system-controlled, unlike tool results. Treat hook output as user feedback.
- Prefer dedicated file and search tools over shell commands. Independent tool calls may run in parallel in one response; dependent ones must be sequential.
- Reference code as `file_path:line_number`.
- Write code that matches the surrounding style, naming, and comment density.
- Use they/them when someone's pronouns are unstated; never infer them from a name.

# Safety
Confirm before actions that are hard to reverse or outward-facing; approval in one context does not carry to the next. Inspect the target before deleting or overwriting. Report outcomes faithfully: if a test fails, show the output; if a step was skipped, say so; when verified, state it plainly.

# Scope
Act on the actual request. The requested scope is the deliverable — do not narrow, widen, or transform it. Make routine judgment calls yourself and ask only when readings differ materially. If part of the task is blocked, finish the rest and say what was left out and why. Report completion only when done.

# Corrections
Correct an earlier statement only when the error changes the user's code, conclusions, or decisions. State it plainly and move on, without apologies or tallies. A follow-up question is not evidence you were wrong.

# Context
When the conversation grows long it is summarized and continues in the next window. Do not wrap up early or hand off mid-task."
        )
    })
}

const GENERIC_SECTIONS: &[&str] = &[
    "harness",
    "tone and style",
    "doing tasks",
    "using your tools",
    "following conventions",
    "code style",
    "task management",
    "context management",
    "delivering work",
    "corrections",
    "professionalism",
    "objectivity",
    "proactiveness",
    "committing changes with git",
    "creating pull requests",
];

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

const IDENTITY_MARKERS: &[&str] = &[
    "you are a claude agent",
    "built on anthropic's claude agent sdk",
];
const MIN_LENGTH: usize = 1500;

fn header() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^(#{1,3})\s+(.+?)\s*$").unwrap())
}

pub fn is_claude_code_prompt(text: &str) -> bool {
    if text.chars().count() < MIN_LENGTH {
        return false;
    }
    let lowered = text.to_lowercase();
    MARKERS.iter().filter(|m| lowered.contains(*m)).count() >= 2
}

pub fn is_claude_identity_block(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let lowered = text.trim().to_lowercase();
    if lowered.chars().count() > 300 {
        return false;
    }
    IDENTITY_MARKERS.iter().any(|m| lowered.contains(m))
}

fn sections(text: &str) -> Vec<(Option<String>, &str)> {
    let matches: Vec<regex::Captures> = header().captures_iter(text).collect();
    if matches.is_empty() {
        return vec![(None, text)];
    }
    let mut parts = Vec::new();
    let first = matches[0].get(0).unwrap().start();
    if first > 0 {
        let lead = &text[..first];
        if !lead.trim().is_empty() {
            parts.push((None, lead));
        }
    }
    for (i, m) in matches.iter().enumerate() {
        let start = m.get(0).unwrap().start();
        let end = matches
            .get(i + 1)
            .map(|n| n.get(0).unwrap().start())
            .unwrap_or(text.len());
        parts.push((Some(m[2].trim().to_lowercase()), &text[start..end]));
    }
    parts
}

pub fn condense(text: &str) -> String {
    let mut kept = vec![kiro_preamble().to_owned()];
    for (h, chunk) in sections(text) {
        match h {
            None => continue,
            Some(name) if GENERIC_SECTIONS.contains(&name.as_str()) => continue,
            Some(_) => kept.push(chunk.trim().to_owned()),
        }
    }
    kept.join("\n\n").trim().to_owned()
}

#[derive(Default, Clone, Copy)]
pub struct FilterStats {
    pub blocks_seen: usize,
    pub blocks_condensed: usize,
    pub chars_before: usize,
    pub chars_after: usize,
}

pub fn filter_blocks(blocks: &[String]) -> (Vec<String>, FilterStats) {
    let mut stats = FilterStats::default();
    let mut out = Vec::with_capacity(blocks.len());
    for text in blocks {
        stats.blocks_seen += 1;
        stats.chars_before += text.chars().count();
        let replaced = if is_claude_code_prompt(text) {
            stats.blocks_condensed += 1;
            condense(text)
        } else if is_claude_identity_block(text) {
            stats.blocks_condensed += 1;
            KIRO_IDENTITY.to_owned()
        } else {
            text.clone()
        };
        stats.chars_after += replaced.chars().count();
        out.push(replaced);
    }
    (out, stats)
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
static LAST_CONDENSE: Mutex<Option<FilterStats>> = Mutex::new(None);

pub fn record_shorten(stats: ShortenStats) {
    *LAST_SHORTEN.lock() = Some(stats);
}

pub fn record_condense(stats: FilterStats) {
    *LAST_CONDENSE.lock() = Some(stats);
}

pub fn last_stats() -> Value {
    let shorten = LAST_SHORTEN
        .lock()
        .map(|s| s.as_json())
        .unwrap_or(Value::Null);
    let condense = LAST_CONDENSE
        .lock()
        .map(|s| {
            json!({"blocksSeen": s.blocks_seen, "blocksCondensed": s.blocks_condensed, "charsBefore": s.chars_before, "charsAfter": s.chars_after})
        })
        .unwrap_or(Value::Null);
    json!({"lastShorten": shorten, "lastCondense": condense})
}

pub fn dropped_sections() -> Vec<&'static str> {
    GENERIC_SECTIONS.to_vec()
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
    fn leaves_short_descriptions_alone() {
        assert!(shorten_description("Reads a file.", &[], 1200).is_none());
    }

    #[test]
    fn condense_keeps_unknown_sections() {
        let prompt = format!(
            "You are an interactive agent that helps users with software engineering tasks.\n# Tone and style\n{}\n# Environment\ncwd: /x\n# Doing tasks\nstuff",
            "x".repeat(2000)
        );
        let out = condense(&prompt);
        assert!(out.starts_with(KIRO_IDENTITY));
        assert!(out.contains("# Environment\ncwd: /x"));
        assert!(!out.contains("# Tone and style"));
    }
}
