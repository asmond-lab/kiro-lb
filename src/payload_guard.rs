//! Payload size guard. The upstream CONTENT_LENGTH_EXCEEDS_THRESHOLD tracks cl100k
//! tokens of the compact JSON, not bytes; the byte cap is a legacy extra.

use serde_json::{Map, Value};
use std::collections::HashSet;

use crate::tokenizer::count_tokens;

#[derive(Debug, Clone)]
pub struct PayloadTooLarge {
    pub size: usize,
    pub limit: usize,
    pub unit: &'static str,
}

impl std::fmt::Display for PayloadTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let word = if self.unit == "tokens" {
            "token"
        } else {
            "byte"
        };
        write!(
            f,
            "Request payload is {} {}, over the {} {word} limit Kiro accepts. Shorten the conversation or send fewer tools. Set AUTO_TRIM_PAYLOAD=true to drop the oldest history instead (this silently loses earlier context).",
            self.size, self.unit, self.limit
        )
    }
}

pub fn compact_json(payload: &Value) -> String {
    serde_json::to_string(payload).unwrap_or_default()
}

pub fn measure_text(serialized: &str) -> (usize, usize) {
    (
        count_tokens(serialized, false, Some("claude-haiku-4.5")),
        serialized.len(),
    )
}

pub fn measure(payload: &Value) -> (usize, usize) {
    measure_text(&compact_json(payload))
}

#[derive(Debug, Default)]
pub struct TrimStats {
    pub original_bytes: usize,
    pub final_bytes: usize,
    pub original_entries: usize,
    pub final_entries: usize,
    pub original_tokens: usize,
    pub final_tokens: usize,
}

fn history_mut(payload: &mut Value) -> Option<&mut Vec<Value>> {
    payload
        .pointer_mut("/conversationState/history")
        .and_then(Value::as_array_mut)
}

fn over_limit(payload: &Value, max_bytes: Option<usize>, max_tokens: Option<usize>) -> bool {
    let s = compact_json(payload);
    if let Some(t) = max_tokens {
        if count_tokens(&s, false, Some("claude-haiku-4.5")) > t {
            return true;
        }
    }
    max_bytes.is_some_and(|b| s.len() > b)
}

fn repair_orphans(history: &mut [Value], current: Option<&mut Value>) {
    let mut entries: Vec<&mut Value> = history.iter_mut().collect();
    if let Some(c) = current {
        entries.push(c);
    }
    let mut prev_ids: HashSet<String> = HashSet::new();
    for entry in entries {
        if let Some(a) = entry.get("assistantResponseMessage") {
            prev_ids = a
                .get("toolUses")
                .and_then(Value::as_array)
                .map(|t| {
                    t.iter()
                        .filter_map(|u| {
                            u.get("toolUseId")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default();
            continue;
        }
        let valid = std::mem::take(&mut prev_ids);
        let Some(user) = entry
            .get_mut("userInputMessage")
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        let Some(results) = user
            .get("userInputMessageContext")
            .and_then(|c| c.get("toolResults"))
            .and_then(Value::as_array)
            .cloned()
        else {
            continue;
        };
        let (kept, orphaned): (Vec<Value>, Vec<Value>) = results.into_iter().partition(|r| {
            r.get("toolUseId")
                .and_then(Value::as_str)
                .is_some_and(|id| valid.contains(id))
        });
        if orphaned.is_empty() {
            continue;
        }
        let texts: Vec<String> = orphaned
            .iter()
            .flat_map(|r| match r.get("content") {
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| {
                        p.get("text")
                            .and_then(Value::as_str)
                            .filter(|t| !t.is_empty())
                            .map(str::to_owned)
                    })
                    .collect(),
                Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
                _ => vec![],
            })
            .collect();
        let ctx = user
            .get_mut("userInputMessageContext")
            .and_then(Value::as_object_mut)
            .unwrap();
        if kept.is_empty() {
            ctx.remove("toolResults");
            if ctx.is_empty() {
                user.remove("userInputMessageContext");
            }
        } else {
            ctx.insert("toolResults".into(), Value::Array(kept));
        }
        if !texts.is_empty() {
            let current = user
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            user.insert(
                "content".into(),
                Value::String(format!(
                    "{current}\n[trimmed tool result] {}",
                    texts.join("; ")
                )),
            );
        }
    }
}

pub fn trim_to_limit(
    payload: &mut Value,
    max_bytes: Option<usize>,
    max_tokens: Option<usize>,
    known: (usize, usize),
) -> TrimStats {
    let (original_tokens, original_bytes) = known;
    let Some(history) = history_mut(payload).filter(|h| !h.is_empty()) else {
        return TrimStats {
            original_bytes,
            final_bytes: original_bytes,
            original_tokens,
            final_tokens: original_tokens,
            ..Default::default()
        };
    };
    let original_entries = history.len();
    for entry in history.iter_mut() {
        if let Some(a) = entry
            .get_mut("assistantResponseMessage")
            .and_then(Value::as_object_mut)
        {
            if a.get("toolUses")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
            {
                a.remove("toolUses");
            }
        }
    }
    let entry_bytes: Vec<usize> = history.iter().map(|e| compact_json(e).len() + 1).collect();
    let tokens_per_byte = if original_bytes > 0 {
        original_tokens as f64 / original_bytes as f64
    } else {
        0.0
    };
    let (mut rem_t, mut rem_b) = (original_tokens as f64, original_bytes as f64);
    let target = max_tokens.map(|t| t as f64 * 0.97);
    let mut index = 0;
    while index < history.len() {
        let over_t = target.is_some_and(|t| rem_t > t);
        let over_b = max_bytes.is_some_and(|b| rem_b > b as f64);
        if !(over_t || over_b) {
            break;
        }
        for _ in 0..2 {
            if index < history.len() {
                rem_t -= entry_bytes[index] as f64 * tokens_per_byte;
                rem_b -= entry_bytes[index] as f64;
                index += 1;
            }
        }
    }
    history.drain(..index);
    loop {
        let snapshot = payload.clone();
        let h = history_mut(payload).unwrap();
        if h.is_empty() || !over_limit(&snapshot, max_bytes, max_tokens) {
            break;
        }
        let n = h.len().min(2);
        h.drain(..n);
    }
    let state = payload
        .get_mut("conversationState")
        .and_then(Value::as_object_mut)
        .unwrap();
    let mut history = state
        .remove("history")
        .and_then(|h| {
            if let Value::Array(a) = h {
                Some(a)
            } else {
                None
            }
        })
        .unwrap_or_default();
    while history
        .first()
        .is_some_and(|e| e.get("userInputMessage").is_none())
    {
        history.remove(0);
    }
    repair_orphans(&mut history, state.get_mut("currentMessage"));
    let final_entries = history.len();
    if !history.is_empty() {
        insert_before_key(state, "history", Value::Array(history));
    }
    let (final_tokens, final_bytes) = measure(payload);
    TrimStats {
        original_bytes,
        final_bytes,
        original_entries,
        final_entries,
        original_tokens,
        final_tokens,
    }
}

fn insert_before_key(map: &mut Map<String, Value>, key: &str, value: Value) {
    map.insert(key.to_owned(), value);
}
