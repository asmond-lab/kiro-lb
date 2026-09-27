use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use kiro_lb::app::{AppState, Shared};
use kiro_lb::pool::AccountManager;
use kiro_lb::routes_dashboard as d;
use kiro_lb::upstream::http::Transport;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::Arc;
use tower::ServiceExt;

fn source(id: &str) -> Value {
    json!({"type": "internal", "id": id, "enabled": true, "credential": {"refreshToken": format!("rt-{id}"), "authMethod": "social"}})
}

fn router(state: Shared) -> Router {
    Router::new()
        .route("/api/dashboard/login", post(d::login))
        .route(
            "/api/dashboard/accounts/{label}/enabled",
            post(d::set_enabled),
        )
        .with_state(state)
}

async fn cookie(app: &Router) -> String {
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/dashboard/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"password":"test-password"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_disables_cannot_both_remove_the_last_account() {
    let dir = std::env::temp_dir().join(format!("kirolb-mut-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    std::env::set_var("DASHBOARD_PASSWORD", "test-password");
    kiro_lb::store::initialize().unwrap();
    kiro_lb::store::with(|c| {
        kiro_lb::store::replace_account_sources(c, &[source("a"), source("b")], true)
    })
    .unwrap();

    let http = reqwest::Client::new();
    let pool = AccountManager::new(http.clone());
    pool.load_credentials();
    assert_eq!(pool.accounts().len(), 2);
    let state: Shared = Arc::new(AppState {
        pool: pool.clone(),
        transport: Arc::new(Transport {
            shared: http.clone(),
        }),
        http,
        started_at: 0.0,
        quiesced: AtomicBool::new(false),
        inflight: AtomicI64::new(0),
        drained: tokio::sync::Notify::new(),
    });
    let app = router(state);
    let session = cookie(&app).await;
    let disable = |id: &'static str| {
        let (app, session) = (app.clone(), session.clone());
        let label = kiro_lb::pool::account_label(id);
        tokio::spawn(async move {
            app.oneshot(
                Request::post(format!("/api/dashboard/accounts/{label}/enabled"))
                    .header("cookie", session)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
        })
    };
    let (a, b) = (disable("a"), disable("b"));
    let mut statuses = vec![a.await.unwrap(), b.await.unwrap()];
    statuses.sort();
    let persisted: Vec<String> = kiro_lb::store::load_account_sources()
        .into_iter()
        .filter(|e| e["enabled"].as_bool().unwrap_or(true))
        .map(|e| e["id"].as_str().unwrap().to_owned())
        .collect();
    let live: Vec<String> = pool.accounts().iter().map(|a| a.id.clone()).collect();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(statuses, vec![StatusCode::OK, StatusCode::CONFLICT]);
    assert_eq!(live.len(), 1);
    assert_eq!(persisted, live);
}
