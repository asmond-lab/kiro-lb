use parking_lot::RwLock;
use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;
use std::time::Instant;

use crate::config::{
    self, DEFAULT_MAX_INPUT_TOKENS, HIDDEN_FROM_LIST, HIDDEN_MODELS, MODEL_ALIASES,
};

struct Patterns {
    ctx_suffix: Regex,
    standard: Regex,
    no_minor: Regex,
    legacy: Regex,
    dot_with_date: Regex,
    inverted: Regex,
    family: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        ctx_suffix: Regex::new(r"(?i)\[\d+[mk]\]$").unwrap(),
        standard: Regex::new(
            r"^(claude-(?:haiku|sonnet|opus)-\d+)-(\d{1,2})(?:-(?:\d{8}|latest|\d+))?$",
        )
        .unwrap(),
        no_minor: Regex::new(r"^(claude-(?:haiku|sonnet|opus)-\d+)(?:-\d{8})?$").unwrap(),
        legacy: Regex::new(r"^(claude)-(\d+)-(\d+)-(haiku|sonnet|opus)(?:-(?:\d{8}|latest|\d+))?$")
            .unwrap(),
        dot_with_date: Regex::new(
            r"^(claude-(?:\d+\.\d+-)?(?:haiku|sonnet|opus)(?:-\d+\.\d+)?)-\d{8}$",
        )
        .unwrap(),
        inverted: Regex::new(r"^claude-(\d+)\.(\d+)-(haiku|sonnet|opus)-(.+)$").unwrap(),
        family: Regex::new(r"(?i)(haiku|sonnet|opus)").unwrap(),
    })
}

/// Client model name to Kiro format. Unknown names pass through unchanged.
pub fn normalize_model_name(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    let p = patterns();
    let name = p.ctx_suffix.replace(name, "").into_owned();
    let lower = name.to_lowercase();
    if let Some(c) = p.standard.captures(&lower) {
        return format!("{}.{}", &c[1], &c[2]);
    }
    if let Some(c) = p.no_minor.captures(&lower) {
        return c[1].to_owned();
    }
    if let Some(c) = p.legacy.captures(&lower) {
        return format!("{}-{}.{}-{}", &c[1], &c[2], &c[3], &c[4]);
    }
    if let Some(c) = p.dot_with_date.captures(&lower) {
        return c[1].to_owned();
    }
    if let Some(c) = p.inverted.captures(&lower) {
        return format!("claude-{}-{}.{}", &c[3], &c[1], &c[2]);
    }
    name
}

fn lookup<'a>(table: &'a [(&str, &str)], key: &str) -> Option<&'a str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

pub fn get_model_id_for_kiro(model_name: &str) -> String {
    let mut normalized = normalize_model_name(model_name);
    if let Some(target) =
        lookup(MODEL_ALIASES, &normalized).or_else(|| lookup(MODEL_ALIASES, model_name))
    {
        normalized = normalize_model_name(target);
    }
    lookup(HIDDEN_MODELS, &normalized)
        .map(str::to_owned)
        .unwrap_or(normalized)
}

pub fn extract_model_family(model_name: &str) -> Option<String> {
    patterns()
        .family
        .captures(model_name)
        .map(|c| c[1].to_lowercase())
}

#[derive(Clone, Debug)]
pub struct ModelResolution {
    pub internal_id: String,
    pub source: &'static str,
    pub normalized: String,
    pub is_verified: bool,
}

/// Model metadata from ListAvailableModels, shared per account.
#[derive(Default)]
pub struct ModelInfoCache {
    inner: RwLock<(HashMap<String, Value>, Option<Instant>)>,
}

impl ModelInfoCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&self, models: Vec<Value>) {
        tracing::info!("Updating model cache. Found {} models.", models.len());
        let map = models
            .into_iter()
            .filter_map(|m| {
                m.get("modelId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .map(|id| (id, m.clone()))
            })
            .collect();
        *self.inner.write() = (map, Some(Instant::now()));
    }

    pub fn seed_fallback(&self) {
        let models = config::FALLBACK_MODELS
            .iter()
            .map(|m| {
                serde_json::json!({
                    "modelId": m.model_id,
                    "tokenLimits": {"maxInputTokens": m.max_input_tokens, "maxOutputTokens": m.max_output_tokens},
                })
            })
            .collect();
        self.update(models);
    }

    pub fn get(&self, id: &str) -> Option<Value> {
        self.inner.read().0.get(id).cloned()
    }

    pub fn is_valid_model(&self, id: &str) -> bool {
        self.inner.read().0.contains_key(id)
    }

    /// The window contextUsagePercentage is a percentage of. Five models advertise
    /// 1000000 but charge against 666667 (measured), so the measured value wins.
    pub fn max_input_tokens(&self, id: &str) -> u64 {
        if let Some(f) = config::fallback_limits(id).filter(|f| f.max_input_tokens == 666_667) {
            return f.max_input_tokens;
        }
        self.inner
            .read()
            .0
            .get(id)
            .and_then(|m| m.pointer("/tokenLimits/maxInputTokens"))
            .and_then(Value::as_u64)
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_INPUT_TOKENS)
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().0.is_empty()
    }

    pub fn is_stale(&self) -> bool {
        self.inner
            .read()
            .1
            .is_none_or(|t| t.elapsed().as_secs() > config::MODEL_CACHE_TTL)
    }

    pub fn all_model_ids(&self) -> Vec<String> {
        self.inner.read().0.keys().cloned().collect()
    }

    pub fn all_models(&self) -> Vec<Value> {
        self.inner.read().0.values().cloned().collect()
    }
}

pub fn resolve(cache: &ModelInfoCache, external: &str) -> ModelResolution {
    let resolved = lookup(MODEL_ALIASES, external).unwrap_or(external);
    let normalized = normalize_model_name(resolved);
    if cache.is_valid_model(&normalized) {
        return ModelResolution {
            internal_id: normalized.clone(),
            source: "cache",
            normalized,
            is_verified: true,
        };
    }
    if let Some(internal) = lookup(HIDDEN_MODELS, &normalized) {
        return ModelResolution {
            internal_id: internal.to_owned(),
            source: "hidden",
            normalized,
            is_verified: true,
        };
    }
    tracing::info!("Model '{external}' (normalized: '{normalized}') not in cache, mapped to runtime ID: '{normalized}'");
    ModelResolution {
        internal_id: normalized.clone(),
        source: "passthrough",
        normalized,
        is_verified: false,
    }
}

pub fn available_models(cache: &ModelInfoCache) -> Vec<String> {
    let mut models: BTreeSet<String> = cache.all_model_ids().into_iter().collect();
    models.extend(HIDDEN_MODELS.iter().map(|(k, _)| (*k).to_owned()));
    for hidden in HIDDEN_FROM_LIST {
        models.remove(*hidden);
    }
    models.extend(MODEL_ALIASES.iter().map(|(k, _)| (*k).to_owned()));
    models.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_examples() {
        for (i, o) in [
            ("claude-haiku-4-5-20251001", "claude-haiku-4.5"),
            ("claude-sonnet-4-5", "claude-sonnet-4.5"),
            ("claude-sonnet-4", "claude-sonnet-4"),
            ("claude-sonnet-4-20250514", "claude-sonnet-4"),
            ("claude-3-7-sonnet", "claude-3.7-sonnet"),
            ("claude-3-7-sonnet-20250219", "claude-3.7-sonnet"),
            ("claude-4.5-opus-high", "claude-opus-4.5"),
            ("claude-opus-5-5[1m]", "claude-opus-5.5"),
            ("Claude-Opus-5.5[1M]", "Claude-Opus-5.5"),
            ("claude-opus-5-5", "claude-opus-5.5"),
            ("claude-opus-5", "claude-opus-5"),
            ("claude-opus-5[1m]", "claude-opus-5"),
            ("claude-sonnet-5", "claude-sonnet-5"),
            ("claude-sonnet-5[1m]", "claude-sonnet-5"),
            ("claude-opus-4-8", "claude-opus-4.8"),
            ("claude-opus-4-8[1m]", "claude-opus-4.8"),
            ("claude-opus-4-7", "claude-opus-4.7"),
            ("claude-opus-4-7[1m]", "claude-opus-4.7"),
            ("claude-opus-4-6", "claude-opus-4.6"),
            ("claude-opus-4-6[1m]", "claude-opus-4.6"),
            ("claude-sonnet-4-6", "claude-sonnet-4.6"),
            ("claude-sonnet-4-6[1m]", "claude-sonnet-4.6"),
            ("claude-haiku-4-5", "claude-haiku-4.5"),
            ("claude-opus-4-5", "claude-opus-4.5"),
            ("claude-opus-4-5-20251101", "claude-opus-4.5"),
            ("claude-sonnet-4-5-20250929", "claude-sonnet-4.5"),
            ("claude-sonnet-4-5[1m]", "claude-sonnet-4.5"),
            ("auto", "auto"),
        ] {
            assert_eq!(normalize_model_name(i), o, "{i}");
        }
        assert_eq!(get_model_id_for_kiro("auto-kiro"), "auto");
    }
}
