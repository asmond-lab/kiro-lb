//! Operator-triggered endpoint connectivity and latency probes. They spend real
//! quota and deliberately bypass the transport, so measuring cannot change routing.

use serde_json::{json, Value};
use std::time::{Duration, Instant};

use crate::app::Shared;
use crate::upstream::endpoints::{self, KiroEndpoint};
use crate::utils::kiro_headers;

pub const PING_REPS_MAX: i64 = 10;
pub const PING_REPS_DEFAULT: i64 = 1;
const PROMPT: &str = "Reply with the single word: ok";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn selected(only: Option<&str>) -> Result<Vec<&'static KiroEndpoint>, (u16, String)> {
    match only.filter(|o| !o.is_empty()) {
        None => Ok(endpoints::ENDPOINTS.iter().collect()),
        Some(k) => endpoints::by_key(k)
            .map(|e| vec![e])
            .ok_or((503, format!("unknown endpoint '{k}'"))),
    }
}

async fn once(
    state: &Shared,
    ep: &KiroEndpoint,
    auth: &crate::auth::KiroAuth,
    model: &str,
) -> Value {
    let url = match ep.url(&auth.api_region) {
        Ok(url) => url,
        Err(e) => {
            return json!({"ok": false, "statusCode": null, "ttfbMs": null, "error": e.to_string()})
        }
    };
    let token = match auth.access_token().await {
        Ok(t) => t,
        Err(e) => {
            return json!({"ok": false, "statusCode": null, "ttfbMs": null, "error": e.to_string()})
        }
    };
    let arn = auth.request_profile_arn();
    let mut body = json!({"conversationState": {"chatTriggerType": "MANUAL", "conversationId": uuid::Uuid::new_v4().to_string(), "currentMessage": {"userInputMessage": {"content": PROMPT, "modelId": model, "origin": "AI_EDITOR"}}, "history": []}});
    if let Some(a) = &arn {
        body["profileArn"] = json!(a);
    }
    let mut req = state
        .http
        .post(url)
        .timeout(Duration::from_secs(45))
        .json(&body);
    let mut headers = kiro_headers(&token);
    for (k, v) in ep.header_overrides() {
        headers.retain(|(h, _)| !h.eq_ignore_ascii_case(k));
        headers.push((k, v));
    }
    for (k, v) in headers {
        req = req.header(k, v);
    }
    if let Some(a) = &arn {
        req = req.header("x-amzn-kiro-profile-arn", a);
    }
    let started = Instant::now();
    match req.send().await {
        Ok(mut r) => {
            let status = r.status().as_u16();
            let _ = r.chunk().await;
            let ttfb = started.elapsed().as_millis() as i64;
            json!({"ok": status == 200, "statusCode": status, "ttfbMs": ttfb, "error": if status == 200 { Value::Null } else { json!(format!("HTTP {status}")) }})
        }
        Err(e) => {
            json!({"ok": false, "statusCode": null, "ttfbMs": null, "error": e.to_string().chars().take(200).collect::<String>()})
        }
    }
}

async fn account(
    state: &Shared,
    model: &str,
) -> Result<std::sync::Arc<crate::auth::KiroAuth>, (u16, String)> {
    let a = match state
        .pool
        .next_account(model, &Default::default(), None)
        .await
    {
        Some(a) => Some(a),
        None => state.pool.first_initialized(),
    };
    a.and_then(|a| a.auth())
        .ok_or((503, "no account is available to probe with".into()))
}

pub async fn test(
    state: &Shared,
    model: Option<&str>,
    only: Option<&str>,
) -> Result<Value, (u16, String)> {
    let Ok(_g) = LOCK.try_lock() else {
        return Err((409, "a probe is already running".into()));
    };
    let targets = selected(only)?;
    let model = model.unwrap_or("claude-sonnet-4.5").to_owned();
    let auth = account(state, &model).await?;
    let mut results = Vec::new();
    for ep in targets {
        let mut r = once(state, ep, &auth, &model).await;
        r["key"] = json!(ep.key);
        r["name"] = json!(ep.name);
        results.push(r);
    }
    Ok(json!({"model": model, "requestsSpent": results.len(), "results": results}))
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

pub async fn ping(
    state: &Shared,
    reps: i64,
    model: Option<&str>,
    only: Option<&str>,
) -> Result<Value, (u16, String)> {
    let Ok(_g) = LOCK.try_lock() else {
        return Err((409, "a probe is already running".into()));
    };
    let reps = reps.clamp(1, PING_REPS_MAX);
    let targets = selected(only)?;
    let model = model.unwrap_or("claude-sonnet-4.5").to_owned();
    let auth = account(state, &model).await?;
    let mut samples: Vec<Vec<f64>> = vec![vec![]; targets.len()];
    let mut failures: Vec<Vec<String>> = vec![vec![]; targets.len()];
    for _ in 0..reps {
        for (i, ep) in targets.iter().enumerate() {
            let r = once(state, ep, &auth, &model).await;
            match (r["ok"].as_bool(), r["ttfbMs"].as_f64()) {
                (Some(true), Some(t)) => samples[i].push(t),
                _ => failures[i].push(r["error"].as_str().unwrap_or("unknown").to_owned()),
            }
        }
    }
    let mut results = Vec::new();
    let mut medians: Vec<(&'static str, f64, f64)> = Vec::new();
    for (i, ep) in targets.iter().enumerate() {
        let mut v = samples[i].clone();
        let (med, min, max) = if v.is_empty() {
            (Value::Null, Value::Null, Value::Null)
        } else {
            let lo = v.iter().copied().fold(f64::MAX, f64::min);
            let hi = v.iter().copied().fold(f64::MIN, f64::max);
            let m = median(&mut v);
            medians.push((ep.key, m, hi - lo));
            (
                json!(m.round() as i64),
                json!(lo.round() as i64),
                json!(hi.round() as i64),
            )
        };
        results.push(json!({"key": ep.key, "name": ep.name, "samples": samples[i].len(), "medianMs": med, "minMs": min, "maxMs": max, "failures": failures[i]}));
    }
    let mut out = json!({"model": model, "reps": reps, "requestsSpent": reps * targets.len() as i64, "results": results});
    let verdict = if medians.is_empty() {
        json!({"fastest": null, "conclusive": false, "verdict": "No endpoint answered."})
    } else {
        let fastest = medians.iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        if medians.len() == 1 {
            json!({"fastest": fastest.0, "conclusive": false, "verdict": format!("Only {} answered; nothing to compare against.", fastest.0)})
        } else {
            let between = medians.iter().map(|m| m.1).fold(f64::MIN, f64::max)
                - medians.iter().map(|m| m.1).fold(f64::MAX, f64::min);
            let within = medians.iter().map(|m| m.2).fold(0.0, f64::max);
            let conclusive = between > within;
            let text = if conclusive {
                format!("{} is fastest by {}ms, which exceeds the widest single-endpoint spread of {}ms.", fastest.0, between.round(), within.round())
            } else {
                format!("Indistinguishable: the {}ms gap between endpoints is smaller than the {}ms spread within one endpoint. Raise repetitions for a firmer answer.", between.round(), within.round())
            };
            json!({"fastest": fastest.0, "conclusive": conclusive, "betweenSpreadMs": between.round() as i64, "withinSpreadMs": within.round() as i64, "verdict": text})
        }
    };
    out.as_object_mut()
        .unwrap()
        .extend(verdict.as_object().unwrap().clone());
    Ok(out)
}
