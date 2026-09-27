//! Prometheus exposition from state the gateway already keeps. No secrets in
//! labels, bounded cardinality: the client-controlled `model` label is clamped to
//! what the pool serves and everything else collapses into `other`.

use std::collections::{BTreeMap, HashSet};

use crate::app::Shared;
use crate::model_resolver::normalize_model_name;
use crate::pool::{account_label, routing_state};
use crate::usage_tracking::{ROOT_KEY_ID, UNKNOWN_ACCOUNT_ID};
use crate::{config, dashboard_store, store};

pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

pub const FAMILIES: [(&str, &str, &str); 20] = [
    (
        "kiro_lb_up",
        "gauge",
        "Always 1; scrape liveness for the gateway process.",
    ),
    (
        "kiro_lb_uptime_seconds",
        "gauge",
        "Seconds since the gateway process started.",
    ),
    (
        "kiro_lb_build_info",
        "gauge",
        "Gateway version, as a label on a constant 1.",
    ),
    (
        "kiro_lb_requests_total",
        "counter",
        "Data-plane requests by model, protocol and status class.",
    ),
    (
        "kiro_lb_request_latency_seconds",
        "summary",
        "Latency of successful requests by model and protocol.",
    ),
    (
        "kiro_lb_tokens_total",
        "counter",
        "Tokens attributed to an API key, by model and direction.",
    ),
    (
        "kiro_lb_generation_seconds_total",
        "counter",
        "Upstream generation time, by model. Denominator for tokens/sec.",
    ),
    (
        "kiro_lb_timed_output_tokens_total",
        "counter",
        "Output tokens from requests that were also timed. Numerator for tokens/sec.",
    ),
    (
        "kiro_lb_key_requests_total",
        "counter",
        "Requests attributed to an API key, by model.",
    ),
    (
        "kiro_lb_account_tokens_total",
        "counter",
        "Tokens attributed to the serving account, by model and direction.",
    ),
    (
        "kiro_lb_account_model_requests_total",
        "counter",
        "Requests attributed to the serving account, by model.",
    ),
    (
        "kiro_lb_accounts",
        "gauge",
        "Accounts in the pool by routing state.",
    ),
    (
        "kiro_lb_account_requests_total",
        "counter",
        "Upstream requests per account by outcome.",
    ),
    (
        "kiro_lb_account_failures",
        "gauge",
        "Consecutive failures feeding the circuit breaker, per account.",
    ),
    (
        "kiro_lb_account_eligible_in_seconds",
        "gauge",
        "Seconds until an excluded account may serve traffic again.",
    ),
    (
        "kiro_lb_account_quota_used",
        "gauge",
        "Credits consumed this period, per account.",
    ),
    (
        "kiro_lb_account_quota_limit",
        "gauge",
        "Credit allowance for the period, per account.",
    ),
    (
        "kiro_lb_account_quota_percent",
        "gauge",
        "Percentage of the credit allowance consumed, per account.",
    ),
    (
        "kiro_lb_account_quota_reset_seconds",
        "gauge",
        "Seconds until the credit allowance resets, per account.",
    ),
    (
        "kiro_lb_models",
        "gauge",
        "Models the pool can currently serve.",
    ),
];

const ROUTING_STATES: [&str; 8] = [
    "available",
    "uninitialized",
    "cooling_down",
    "rate_limited",
    "quota_exhausted",
    "quota_depleted",
    "suspended",
    "auth_dead",
];

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').to_owned()
    }
}

fn line(name: &str, labels: &[(&str, String)], v: f64) -> String {
    if labels.is_empty() {
        return format!("{name} {}", number(v));
    }
    let rendered: Vec<String> = labels
        .iter()
        .map(|(k, val)| format!("{k}=\"{}\"", escape(val)))
        .collect();
    format!("{name}{{{}}} {}", rendered.join(","), number(v))
}

fn protocol(route: &str) -> &'static str {
    match route {
        "/v1/chat/completions" | "/v1/models" => "openai",
        "/v1/messages" | "/v1/messages/count_tokens" => "anthropic",
        _ => "other",
    }
}

fn model(v: Option<&str>, known: &HashSet<String>) -> String {
    let Some(v) = v.filter(|v| !v.is_empty()) else {
        return "unknown".into();
    };
    if known.contains(v) {
        return v.to_owned();
    }
    let n = normalize_model_name(v);
    if known.contains(&n) {
        n
    } else {
        "other".into()
    }
}

fn status_class(code: i64) -> String {
    if code <= 0 {
        "unknown".into()
    } else {
        format!("{}xx", code / 100)
    }
}

pub fn render(state: &Shared) -> String {
    dashboard_store::flush_key_model_usage();
    let mut key_names: BTreeMap<String, String> =
        BTreeMap::from([(ROOT_KEY_ID.to_owned(), "root".to_owned())]);
    for k in dashboard_store::list_data_api_keys() {
        if let (Some(id), Some(n)) = (k["id"].as_str(), k["name"].as_str()) {
            key_names.insert(id.to_owned(), n.to_owned());
        }
    }
    let mut known: HashSet<String> = state.pool.all_available_models().into_iter().collect();
    if known.is_empty() {
        known.extend(
            config::FALLBACK_MODELS
                .iter()
                .map(|m| m.model_id.to_owned()),
        );
    }
    known.extend(config::MODEL_ALIASES.iter().map(|(k, _)| (*k).to_owned()));
    known.extend(config::HIDDEN_MODELS.iter().map(|(k, _)| (*k).to_owned()));
    let mut out: Vec<String> = Vec::new();
    for (name, kind, help) in FAMILIES {
        out.push(format!("# HELP {name} {help}"));
        out.push(format!("# TYPE {name} {kind}"));
    }
    out.push(line("kiro_lb_up", &[], 1.0));
    out.push(line(
        "kiro_lb_uptime_seconds",
        &[],
        (store::now_f64() - state.started_at).floor(),
    ));
    out.push(line(
        "kiro_lb_build_info",
        &[("version", config::APP_VERSION.into())],
        1.0,
    ));
    out.push(line("kiro_lb_models", &[], known.len() as f64));
    let db = store::with(|c| {
        let mut lines = Vec::new();
        let mut counted: BTreeMap<(String, String, String), i64> = BTreeMap::new();
        let mut stmt =
            c.prepare("SELECT route, model, status_code, requests FROM request_metric_rollups")?;
        for r in stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .flatten()
        {
            *counted
                .entry((
                    model(Some(&r.1), &known),
                    protocol(&r.0).into(),
                    status_class(r.2),
                ))
                .or_default() += r.3;
        }
        for ((m, p, s), n) in counted {
            lines.push(line(
                "kiro_lb_requests_total",
                &[("model", m), ("protocol", p), ("status_class", s)],
                n as f64,
            ));
        }
        let mut timed: BTreeMap<(String, String), (i64, i64)> = BTreeMap::new();
        let mut stmt =
            c.prepare("SELECT route, model, requests, latency_ms FROM request_latency_rollups")?;
        for r in stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .flatten()
        {
            let e = timed
                .entry((model(Some(&r.1), &known), protocol(&r.0).into()))
                .or_default();
            e.0 += r.3;
            e.1 += r.2;
        }
        for ((m, p), (ms, n)) in timed {
            lines.push(line(
                "kiro_lb_request_latency_seconds_sum",
                &[("model", m.clone()), ("protocol", p.clone())],
                ms as f64 / 1000.0,
            ));
            lines.push(line(
                "kiro_lb_request_latency_seconds_count",
                &[("model", m), ("protocol", p)],
                n as f64,
            ));
        }
        let mut totals: BTreeMap<(String, String), (i64, i64, i64)> = BTreeMap::new();
        let mut generation: BTreeMap<String, i64> = BTreeMap::new();
        let mut timed_out: BTreeMap<String, i64> = BTreeMap::new();
        let mut stmt = c.prepare("SELECT key_id, model, prompt_tokens, completion_tokens, requests, generation_ms, timed_completion_tokens FROM key_model_usage")?;
        for r in stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                ))
            })?
            .flatten()
        {
            let m = model(Some(&r.1), &known);
            let e = totals.entry((r.0.clone(), m.clone())).or_default();
            e.0 += r.2;
            e.1 += r.3;
            e.2 += r.4;
            *generation.entry(m.clone()).or_default() += r.5.unwrap_or(0);
            *timed_out.entry(m).or_default() += r.6.unwrap_or(0);
        }
        for ((k, m), (p, cpl, n)) in totals {
            let name = key_names.get(&k).cloned().unwrap_or_else(|| k.clone());
            let base = [
                ("key_id", k.clone()),
                ("key_name", name.clone()),
                ("model", m.clone()),
            ];
            lines.push(line(
                "kiro_lb_tokens_total",
                &[
                    base[0].clone(),
                    base[1].clone(),
                    base[2].clone(),
                    ("direction", "input".into()),
                ],
                p as f64,
            ));
            lines.push(line(
                "kiro_lb_tokens_total",
                &[
                    base[0].clone(),
                    base[1].clone(),
                    base[2].clone(),
                    ("direction", "output".into()),
                ],
                cpl as f64,
            ));
            lines.push(line("kiro_lb_key_requests_total", &base, n as f64));
        }
        for (m, g) in &generation {
            lines.push(line(
                "kiro_lb_generation_seconds_total",
                &[("model", m.clone())],
                *g as f64 / 1000.0,
            ));
            lines.push(line(
                "kiro_lb_timed_output_tokens_total",
                &[("model", m.clone())],
                *timed_out.get(m).unwrap_or(&0) as f64,
            ));
        }
        let mut acct: BTreeMap<(String, String), (i64, i64, i64)> = BTreeMap::new();
        let mut stmt = c.prepare("SELECT account_id, model, SUM(prompt_tokens), SUM(completion_tokens), SUM(requests) FROM account_model_usage GROUP BY account_id, model")?;
        for r in stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            })?
            .flatten()
        {
            let label = if r.0 == UNKNOWN_ACCOUNT_ID {
                r.0.clone()
            } else {
                account_label(&r.0)
            };
            let e = acct.entry((label, model(Some(&r.1), &known))).or_default();
            e.0 += r.2.unwrap_or(0);
            e.1 += r.3.unwrap_or(0);
            e.2 += r.4.unwrap_or(0);
        }
        for ((a, m), (p, cpl, n)) in acct {
            lines.push(line(
                "kiro_lb_account_tokens_total",
                &[
                    ("account", a.clone()),
                    ("model", m.clone()),
                    ("direction", "input".into()),
                ],
                p as f64,
            ));
            lines.push(line(
                "kiro_lb_account_tokens_total",
                &[
                    ("account", a.clone()),
                    ("model", m.clone()),
                    ("direction", "output".into()),
                ],
                cpl as f64,
            ));
            lines.push(line(
                "kiro_lb_account_model_requests_total",
                &[("account", a), ("model", m)],
                n as f64,
            ));
        }
        Ok(lines)
    });
    if let Ok(lines) = db {
        out.extend(lines);
    }
    let now = store::now_f64();
    let mut counts: BTreeMap<&str, i64> = ROUTING_STATES.iter().map(|s| (*s, 0)).collect();
    let mut account_lines = Vec::new();
    for a in state.pool.accounts() {
        let (st, eligible) = routing_state(&a, now);
        *counts.entry(st).or_default() += 1;
        let label = account_label(&a.id);
        let s = a.state.lock();
        account_lines.push(line(
            "kiro_lb_account_requests_total",
            &[("account", label.clone()), ("outcome", "success".into())],
            s.stats.success as f64,
        ));
        account_lines.push(line(
            "kiro_lb_account_requests_total",
            &[("account", label.clone()), ("outcome", "failure".into())],
            s.stats.failed as f64,
        ));
        account_lines.push(line(
            "kiro_lb_account_failures",
            &[("account", label.clone())],
            s.failures as f64,
        ));
        account_lines.push(line(
            "kiro_lb_account_eligible_in_seconds",
            &[("account", label.clone())],
            eligible as f64,
        ));
        drop(s);
        let usage = dashboard_store::cached_usage(&a.id);
        for (metric, field) in [
            ("kiro_lb_account_quota_used", "currentUsage"),
            ("kiro_lb_account_quota_limit", "usageLimit"),
            ("kiro_lb_account_quota_percent", "usagePercent"),
        ] {
            if let Some(v) = usage.get(field).and_then(|v| v.as_f64()) {
                account_lines.push(line(metric, &[("account", label.clone())], v));
            }
        }
        if let Some(d) = usage.get("daysUntilReset").and_then(|v| v.as_f64()) {
            account_lines.push(line(
                "kiro_lb_account_quota_reset_seconds",
                &[("account", label.clone())],
                d * 86400.0,
            ));
        }
    }
    for st in ROUTING_STATES {
        out.push(line(
            "kiro_lb_accounts",
            &[("state", st.into())],
            *counts.get(st).unwrap_or(&0) as f64,
        ));
    }
    out.extend(account_lines);
    out.join("\n") + "\n"
}
