//! Token accounting per key, account and model. The identities travel in a
//! per-request context instead of ContextVars; counts accumulate in memory and are
//! flushed in batches so the data path never writes per request.

use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::model_resolver::normalize_model_name;

pub const ROOT_KEY_ID: &str = "root";
pub const UNKNOWN_ACCOUNT_ID: &str = "unknown";
const CREDIT_FIELDS: [&str; 4] = ["creditUsage", "credit_usage", "creditsConsumed", "credits"];

#[derive(Default, Debug, Clone)]
pub struct RequestUsage {
    pub model: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub credits: Option<f64>,
    pub generation_ms: Option<i64>,
}

/// Everything a request needs to attribute its usage, shared with its stream task.
#[derive(Clone, Default)]
pub struct RequestCtx {
    pub api_key_id: Option<String>,
    pub account_id: Arc<Mutex<Option<String>>>,
    pub usage: Arc<Mutex<RequestUsage>>,
}

impl RequestCtx {
    pub fn new(api_key_id: Option<String>) -> Self {
        RequestCtx {
            api_key_id,
            ..Default::default()
        }
    }

    pub fn note_model(&self, model: &str) {
        self.usage.lock().model = Some(model.to_owned());
    }

    pub fn set_account(&self, id: &str) {
        *self.account_id.lock() = Some(id.to_owned());
    }

    pub fn report_credits(&self, amount: &Value) {
        let raw = match amount {
            Value::Object(o) => match CREDIT_FIELDS.iter().find_map(|f| o.get(*f)) {
                Some(v) => v.clone(),
                None => return,
            },
            other => other.clone(),
        };
        let value = match &raw {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse().ok(),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        };
        if let Some(v) = value.filter(|v| *v > 0.0) {
            let mut u = self.usage.lock();
            u.credits = Some(u.credits.unwrap_or(0.0) + v);
        }
    }

    pub fn record_tokens(
        &self,
        model: &str,
        prompt: i64,
        completion: i64,
        generation_seconds: Option<f64>,
    ) {
        {
            let mut u = self.usage.lock();
            u.input_tokens = Some(prompt.max(0));
            u.output_tokens = Some(completion.max(0));
            u.generation_ms = generation_seconds
                .filter(|g| *g > 0.0)
                .map(|g| (g * 1000.0) as i64);
        }
        let Some(key) = self.api_key_id.clone() else {
            return;
        };
        if model.is_empty() {
            return;
        }
        let account = self
            .account_id
            .lock()
            .clone()
            .unwrap_or_else(|| UNKNOWN_ACCOUNT_ID.to_owned());
        let normalized = normalize_model_name(model);
        let model = if normalized.is_empty() {
            model.to_owned()
        } else {
            normalized
        };
        let completion = completion.max(0);
        let mut pending = PENDING.lock();
        let entry = pending.entry((key, account, model)).or_insert([0; 5]);
        entry[0] += prompt.max(0);
        entry[1] += completion;
        entry[2] += 1;
        if let Some(g) = generation_seconds.filter(|g| *g > 0.0) {
            entry[3] += (g * 1000.0) as i64;
            entry[4] += completion;
        }
    }
}

type Key = (String, String, String);
static PENDING: Mutex<Option<HashMap<Key, [i64; 5]>>> = Mutex::new(None);

trait LockExt {
    fn entry(&mut self, k: Key) -> std::collections::hash_map::Entry<'_, Key, [i64; 5]>;
}

impl LockExt for parking_lot::MutexGuard<'_, Option<HashMap<Key, [i64; 5]>>> {
    fn entry(&mut self, k: Key) -> std::collections::hash_map::Entry<'_, Key, [i64; 5]> {
        self.get_or_insert_with(HashMap::new).entry(k)
    }
}

pub type UsageRow = (String, String, String, i64, i64, i64, i64, i64);

pub fn drain_pending() -> Vec<UsageRow> {
    let mut guard = PENDING.lock();
    guard
        .take()
        .unwrap_or_default()
        .into_iter()
        .map(|((k, a, m), c)| (k, a, m, c[0], c[1], c[2], c[3], c[4]))
        .collect()
}

pub fn restore_pending(rows: Vec<UsageRow>) {
    let mut guard = PENDING.lock();
    for (k, a, m, p, c, r, g, t) in rows {
        let e = guard.entry((k, a, m)).or_insert([0; 5]);
        for (i, v) in [p, c, r, g, t].into_iter().enumerate() {
            e[i] += v;
        }
    }
}

pub struct GenerationTimer(Instant);

impl GenerationTimer {
    pub fn start() -> Self {
        GenerationTimer(Instant::now())
    }

    pub fn elapsed(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
}
