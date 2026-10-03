use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;

const MIN_RATIO: f64 = 0.5;
const MAX_RATIO: f64 = 2.0;
const MODEL_WEIGHT: f64 = 0.2;
const SESSION_CAPACITY: usize = 10_000;
const COUNT_TOKENS_REFRESH_SECONDS: f64 = 600.0;

#[derive(Default)]
struct State {
    sessions: HashMap<u64, (f64, f64)>,
    models: HashMap<String, f64>,
    count_tokens: HashMap<String, (f64, f64)>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State::default()))
}

fn key(model: &str) -> String {
    crate::model_resolver::get_model_id_for_kiro(model)
}

fn scale(estimate: i64, ratio: f64) -> i64 {
    (estimate as f64 * ratio).round() as i64
}

pub fn observe(session: Option<u64>, model: &str, estimate: i64, reported: i64) {
    if estimate <= 0 || reported <= 0 {
        return;
    }
    let ratio = (reported as f64 / estimate as f64).clamp(MIN_RATIO, MAX_RATIO);
    let now = crate::store::now_f64();
    let mut s = state().lock();
    let entry = s.models.entry(key(model)).or_insert(ratio);
    *entry += (ratio - *entry) * MODEL_WEIGHT;
    if let Some(k) = session {
        if s.sessions.len() >= SESSION_CAPACITY && !s.sessions.contains_key(&k) {
            if let Some(oldest) = s
                .sessions
                .iter()
                .min_by(|a, b| a.1 .1.total_cmp(&b.1 .1))
                .map(|(k, _)| *k)
            {
                s.sessions.remove(&oldest);
            }
        }
        s.sessions.insert(k, (ratio, now));
    }
}

pub fn calibrate(session: Option<u64>, model: &str, estimate: i64) -> i64 {
    let s = state().lock();
    let ratio = session
        .and_then(|k| s.sessions.get(&k).map(|(r, _)| *r))
        .or_else(|| s.models.get(&key(model)).copied())
        .unwrap_or(1.0);
    scale(estimate, ratio)
}

pub fn calibrate_count(model: &str, estimate: i64) -> i64 {
    let now = crate::store::now_f64();
    let k = key(model);
    let mut s = state().lock();
    let live = s.models.get(&k).copied();
    let ratio = match s.count_tokens.get(&k) {
        Some((r, at)) if now - at < COUNT_TOKENS_REFRESH_SECONDS => *r,
        _ => {
            let r = live.unwrap_or(1.0);
            if live.is_some() {
                s.count_tokens.insert(k, (r, now));
            }
            r
        }
    };
    scale(estimate, ratio)
}

pub fn reset() {
    *state().lock() = State::default();
}
