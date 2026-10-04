mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use common::*;
use kiro_lb::pool::AccountManager;
use kiro_lb::routes_dashboard as d;
use kiro_lb::settings;
use serde_json::{json, Value};
use std::collections::HashSet;
use tower::ServiceExt;

const SECRET: &str = "factory-secret";
const SONNET: &str = "claude-sonnet-4.5";
const OTHER: &str = "claude-opus-4.6";

async fn register(app: &Router, secret: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = Request::post("/_internal/accounts/register")
        .header("host", "127.0.0.1:8000")
        .header("content-type", "application/json");
    if let Some(secret) = secret {
        req = req.header("x-handoff-secret", secret);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn tiered(id: &str, tier: &str) -> Value {
    let mut entry = healthy(id);
    entry["tier"] = json!(tier);
    entry
}

fn excluded(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registered_free_account_serves_free_routing_models() {
    let dir = data_dir("free-tier-routing");
    std::env::set_var("HANDOFF_SECRET", SECRET);
    seed(&[]);
    settings::load_tunables();
    let up = upstream().await;
    let pool = AccountManager::new(up.http.clone());
    pool.load_credentials();
    let app = Router::new()
        .route(
            "/_internal/accounts/register",
            post(d::internal_register_account),
        )
        .with_state(state(pool.clone(), &up.http, false));

    let (status, _) = register(&app, None, tiered("free-acct", "free")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = register(&app, Some("wrong"), tiered("free-acct", "free")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = register(&app, Some(SECRET), tiered("free-acct", "free\u{7}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = register(&app, Some(SECRET), tiered("free-acct", "free")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["initialized"], true, "{body}");
    let (status, body) = register(&app, Some(SECRET), tiered("pro-acct", "Pro")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = register(&app, Some(SECRET), tiered("pro-acct", "Pro")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "duplicates stay rejected");

    let (tier, config): (Option<String>, String) = kiro_lb::store::with(|c| {
        c.query_row(
            "SELECT tier, config_json FROM account_sources WHERE account_id = 'free-acct'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    })
    .unwrap();
    assert_eq!(tier.as_deref(), Some("free"));
    assert!(
        !config.contains("tier"),
        "tier lives in its column: {config}"
    );

    let reloaded = AccountManager::new(up.http.clone());
    reloaded.load_credentials();
    reloaded.load_state();
    for (id, tier) in [("free-acct", "free"), ("pro-acct", "Pro")] {
        assert_eq!(
            reloaded.get(id).unwrap().state.lock().tier.as_deref(),
            Some(tier),
            "{id} tier survives a reload"
        );
    }

    // Free routing follows the free account's catalog, not a configured list.
    pool.get("free-acct")
        .unwrap()
        .models
        .update(vec![json!({"modelId": SONNET})]);
    for model in [SONNET, "claude-sonnet-4-5-20250929"] {
        for _ in 0..20 {
            let selected = pool
                .next_account(model, &HashSet::new(), None)
                .await
                .unwrap();
            assert_eq!(selected.id, "free-acct", "{model}");
        }
    }

    let other = pool
        .next_account(OTHER, &excluded(&["free-acct"]), None)
        .await
        .unwrap();
    assert_eq!(other.id, "pro-acct", "other models use the whole pool");
    let other = pool
        .next_account(OTHER, &excluded(&["pro-acct"]), None)
        .await
        .unwrap();
    assert_eq!(other.id, "free-acct");

    let fallback = pool
        .next_account(SONNET, &excluded(&["free-acct"]), None)
        .await
        .unwrap();
    assert_eq!(
        fallback.id, "pro-acct",
        "fallback serves from the full pool"
    );

    let _ = std::fs::remove_dir_all(dir);
}
