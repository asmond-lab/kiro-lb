//! Generation endpoints and rotation state. Only `runtime` is verified for every
//! credential type; the alternates exist for failover. Nothing is ever removed from
//! the attempt order: a cooldown only moves an endpoint to the back.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::utils::{ide_short_user_agent, ide_user_agent};

pub const GENERATE_TARGET: &str = "AmazonCodeWhispererStreamingService.GenerateAssistantResponse";
pub const RUNTIME_GENERATE_TARGET: &str = "KiroRuntimeService.GenerateAssistantResponse";

pub struct KiroEndpoint {
    pub key: &'static str,
    pub name: &'static str,
    pub url_template: &'static str,
    pub amz_target: Option<&'static str>,
    pub content_type: &'static str,
    pub api_label: Option<&'static str>,
    pub client_attribution: Option<&'static str>,
}

impl KiroEndpoint {
    pub fn url(&self, region: &str) -> String {
        self.url_template.replace("{region}", region)
    }

    pub fn header_overrides(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![("Content-Type", self.content_type.to_owned())];
        if let Some(t) = self.amz_target {
            out.push(("x-amz-target", t.to_owned()));
        }
        if let Some(a) = self.client_attribution {
            out.push(("x-amzn-kiro-client-attribution", a.to_owned()));
        }
        if let Some(label) = self.api_label {
            out.push(("User-Agent", ide_user_agent(label)));
            out.push(("x-amz-user-agent", ide_short_user_agent()));
        }
        out
    }
}

pub static ENDPOINTS: [KiroEndpoint; 3] = [
    KiroEndpoint {
        key: "runtime",
        name: "Kiro Runtime",
        url_template: "https://runtime.{region}.kiro.dev/",
        amz_target: Some(RUNTIME_GENERATE_TARGET),
        content_type: "application/x-amz-json-1.0",
        api_label: Some("kiroruntime"),
        client_attribution: Some("kiro-ide"),
    },
    KiroEndpoint {
        key: "codewhisperer",
        name: "CodeWhisperer",
        url_template: "https://codewhisperer.{region}.amazonaws.com/generateAssistantResponse",
        amz_target: Some(GENERATE_TARGET),
        content_type: "application/x-amz-json-1.0",
        api_label: None,
        client_attribution: None,
    },
    KiroEndpoint {
        key: "amazonq",
        name: "AmazonQ",
        url_template: "https://q.{region}.amazonaws.com/generateAssistantResponse",
        amz_target: Some("AmazonQDeveloperStreamingService.SendMessage"),
        content_type: "application/x-amz-json-1.0",
        api_label: None,
        client_attribution: None,
    },
];

pub fn by_key(key: &str) -> Option<&'static KiroEndpoint> {
    ENDPOINTS.iter().find(|e| e.key == key)
}

#[derive(Default)]
struct RotationState {
    affinity: HashMap<(String, String), &'static str>,
    cooldown_until: HashMap<&'static str, Instant>,
}

static STATE: Mutex<Option<RotationState>> = Mutex::new(None);

fn with_state<T>(f: impl FnOnce(&mut RotationState) -> T) -> T {
    let mut guard = STATE.lock();
    f(guard.get_or_insert_with(RotationState::default))
}

pub fn selected(order: &[String]) -> Vec<&'static KiroEndpoint> {
    let mut out: Vec<&'static KiroEndpoint> = Vec::new();
    for key in order {
        if let Some(e) = by_key(key.trim()) {
            if !out.iter().any(|x| x.key == e.key) {
                out.push(e);
            }
        }
    }
    if out.is_empty() {
        ENDPOINTS.iter().collect()
    } else {
        out
    }
}

pub fn is_cooling(key: &str) -> bool {
    with_state(|s| {
        let Some((&k, &until)) = s.cooldown_until.get_key_value(key) else {
            return false;
        };
        if until <= Instant::now() {
            s.cooldown_until.remove(k);
            false
        } else {
            true
        }
    })
}

pub fn record_failure(key: &'static str, cooldown_seconds: f64) {
    if cooldown_seconds <= 0.0 {
        return;
    }
    with_state(|s| {
        s.cooldown_until.insert(
            key,
            Instant::now() + Duration::from_secs_f64(cooldown_seconds),
        );
    });
}

pub fn record_success(account: &str, model: &str, key: &'static str) {
    with_state(|s| {
        s.affinity
            .insert((account.to_owned(), model.to_owned()), key);
        s.cooldown_until.remove(key);
    });
}

pub fn attempt_order(account: &str, model: &str, order: &[String]) -> Vec<&'static KiroEndpoint> {
    let endpoints = selected(order);
    let preferred = with_state(|s| {
        s.affinity
            .get(&(account.to_owned(), model.to_owned()))
            .copied()
    });
    let mut ranked: Vec<&'static KiroEndpoint> = Vec::new();
    if let Some(p) = preferred.and_then(by_key) {
        if endpoints.iter().any(|e| e.key == p.key) && !is_cooling(p.key) {
            ranked.push(p);
        }
    }
    let rest: Vec<&'static KiroEndpoint> = endpoints
        .into_iter()
        .filter(|e| !ranked.iter().any(|r| r.key == e.key))
        .collect();
    let (cooling, ready): (Vec<_>, Vec<_>) = rest.into_iter().partition(|e| is_cooling(e.key));
    ranked.extend(ready);
    ranked.extend(cooling);
    ranked
}

pub fn reset() {
    *STATE.lock() = None;
}
