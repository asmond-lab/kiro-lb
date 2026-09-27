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
    pub ttft_ms: Option<i64>,
}

/// Everything a request needs to attribute its usage, shared with its stream task.
#[derive(Clone, Default)]
pub struct RequestCtx {
    pub api_key_id: Option<String>,
    pub account_id: Arc<Mutex<Option<String>>>,
    pub usage: Arc<Mutex<RequestUsage>>,
    pub capture: Option<Arc<Mutex<crate::debug::Capture>>>,
}

impl RequestCtx {
    pub fn new(api_key_id: Option<String>) -> Self {
        RequestCtx {
            api_key_id,
            ..Default::default()
        }
    }

    pub fn capture(&self, f: impl FnOnce(&mut crate::debug::Capture)) {
        if let Some(c) = &self.capture {
            f(&mut c.lock());
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
        timer: Option<&GenerationTimer>,
    ) {
        let generation_seconds = timer.and_then(GenerationTimer::decode_seconds);
        {
            let mut u = self.usage.lock();
            u.input_tokens = Some(prompt.max(0));
            u.output_tokens = Some(completion.max(0));
            u.generation_ms = generation_seconds.map(|g| (g * 1000.0) as i64);
            u.ttft_ms = timer.and_then(GenerationTimer::ttft_ms);
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
        if let Some(g) = generation_seconds.filter(|_| completion > 1) {
            entry[3] += (g * 1000.0) as i64;
            entry[4] += completion - 1;
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

/// Output speed follows the usual benchmark definition: tokens after the first
/// one divided by the time between the first and last output event. Time to
/// first token is measured and reported separately, never mixed into speed.
pub struct GenerationTimer {
    started: Instant,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl GenerationTimer {
    pub fn start() -> Self {
        GenerationTimer {
            started: Instant::now(),
            first: None,
            last: None,
        }
    }

    pub fn mark(&mut self) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    pub fn ttft_ms(&self) -> Option<i64> {
        self.first
            .map(|f| f.duration_since(self.started).as_millis() as i64)
    }

    pub fn decode_seconds(&self) -> Option<f64> {
        let secs = self.last?.duration_since(self.first?).as_secs_f64();
        (secs > 0.0).then_some(secs)
    }
}

pub fn tokens_per_second(output_tokens: i64, decode_ms: i64) -> Option<f64> {
    (output_tokens > 1 && decode_ms > 0)
        .then(|| (output_tokens - 1) as f64 / (decode_ms as f64 / 1000.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn speed_excludes_time_to_first_token() {
        let t0 = Instant::now();
        let timer = GenerationTimer {
            started: t0,
            first: Some(t0 + Duration::from_millis(1500)),
            last: Some(t0 + Duration::from_millis(3500)),
        };
        assert_eq!(timer.ttft_ms(), Some(1500));
        assert_eq!(timer.decode_seconds(), Some(2.0));
        assert_eq!(tokens_per_second(101, 2000), Some(50.0));
    }

    #[test]
    fn speed_is_undefined_without_a_decode_window() {
        let timer = GenerationTimer::start();
        assert_eq!(timer.ttft_ms(), None);
        assert_eq!(timer.decode_seconds(), None);
        assert_eq!(tokens_per_second(1, 500), None);
        assert_eq!(tokens_per_second(10, 0), None);
    }
}
