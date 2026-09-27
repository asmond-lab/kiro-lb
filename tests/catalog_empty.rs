mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use common::*;
use serde_json::Value;
use std::time::{Duration, Instant};
use tower::ServiceExt;

const KEY: &str = "catalog-empty-key";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_store_lists_no_models_with_200() {
    data_dir("catalog-empty");
    std::env::set_var("PROXY_API_KEY", KEY);
    seed(&[]);
    let up = upstream().await;
    let pool = pool(&up.http, &[]);

    let started = Instant::now();
    assert!(pool.ensure_catalog(Duration::from_secs(10)).await);
    assert!(started.elapsed() < Duration::from_millis(200));

    let app = Router::new()
        .route("/v1/models", get(kiro_lb::routes_v1::models))
        .with_state(state(pool, &up.http, false));
    let res = app
        .oneshot(
            Request::get("/v1/models")
                .header("authorization", format!("Bearer {KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["object"], "list");
    assert_eq!(body["data"], Value::Array(vec![]));
    assert_eq!(up.management_calls() + up.refresh_calls(), 0);
}
