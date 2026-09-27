use axum::body::{Body, Bytes};
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use kiro_lb::app::{self, AppState, Shared};
use kiro_lb::pool::AccountManager;
use kiro_lb::upstream::http::Transport;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

fn state() -> Shared {
    let http = reqwest::Client::new();
    Arc::new(AppState {
        pool: AccountManager::new(http.clone()),
        transport: Arc::new(Transport {
            shared: http.clone(),
        }),
        http,
        started_at: 0.0,
        quiesced: AtomicBool::new(false),
        inflight: AtomicI64::new(0),
        drained: tokio::sync::Notify::new(),
    })
}

fn bundles(dir: &PathBuf) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("capture-") && n.ends_with(".json"))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn sse(chunks: Vec<&'static str>) -> Response {
    let stream = futures_util::stream::iter(
        chunks
            .into_iter()
            .map(|c| Ok::<Bytes, std::io::Error>(Bytes::from_static(c.as_bytes()))),
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn send(router: &Router, path: &str, body: Value) -> String {
    let res = router
        .clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_failing_after_200_is_captured_in_errors_mode() {
    let root = std::env::temp_dir().join(format!(
        "kirolb-streamfail-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let debug_dir = root.join("debug");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::env::set_var("DEBUG_MODE", "errors");
    std::env::set_var("DEBUG_CAPTURE_SUCCESS", "false");
    std::env::set_var("DEBUG_DIR", &debug_dir);
    std::env::set_var("DASHBOARD_DATA_DIR", &data_dir);
    kiro_lb::store::initialize().unwrap();

    let s = state();
    let router = Router::new()
        .route(
            "/v1/messages",
            post(|| async {
                sse(vec![
                    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-sonnet-4.5\"}}\n\n",
                    "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"Internal server error\"}}\n\n",
                ])
            }),
        )
        .route(
            "/v1/chat/completions",
            post(|| async {
                sse(vec![
                    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"}}]}\n\n",
                    "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: [DONE]\n\n",
                ])
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            s.clone(),
            app::data_plane_middleware,
        ))
        .with_state(s);

    let ok = send(
        &router,
        "/v1/chat/completions",
        json!({"model": "gpt-4o", "stream": true, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let after_success = bundles(&debug_dir);

    let failed = send(
        &router,
        "/v1/messages",
        json!({"model": "claude-sonnet-4.5", "stream": true, "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while bundles(&debug_dir).is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let found = bundles(&debug_dir);
    let text = found
        .first()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&root);

    assert!(ok.contains("[DONE]"), "{ok}");
    assert!(failed.contains("event: error"), "{failed}");
    assert!(after_success.is_empty(), "{after_success:?}");
    assert_eq!(found.len(), 1, "{found:?}");
    let bundle: Value = serde_json::from_str(&text).unwrap();
    assert!(bundle["status"].as_u64().unwrap() >= 400, "{bundle}");
    assert_eq!(bundle["status"], 500);
    assert_eq!(bundle["request"]["model"], "claude-sonnet-4.5");
}
