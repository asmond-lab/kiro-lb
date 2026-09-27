use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use futures_util::StreamExt;
use kiro_lb::app::{self, AppState, Shared};
use kiro_lb::pool::AccountManager;
use kiro_lb::routes_v1::retry_on_first_token_timeout;
use kiro_lb::stream_core::{EventStream, KiroEvent, StreamError};
use kiro_lb::upstream::http::Transport;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
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

fn app(state: Shared) -> Router {
    Router::new()
        .route(
            "/v1/chat/completions",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "late"
            }),
        )
        .route(
            "/api/dashboard/accounts/{label}/enabled",
            post(|| async { "changed" }),
        )
        .route("/api/dashboard/accounts", get(|| async { "list" }))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            app::data_plane_middleware,
        ))
        .with_state(state)
}

#[tokio::test]
async fn a_cancelled_request_does_not_leak_the_inflight_count() {
    let s = state();
    let call = app(s.clone()).oneshot(
        Request::post("/v1/chat/completions")
            .body(Body::empty())
            .unwrap(),
    );
    let _ = tokio::time::timeout(Duration::from_millis(100), call).await;
    assert_eq!(s.inflight.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn quiesce_rejects_account_mutations_but_not_reads() {
    let s = state();
    s.quiesced.store(true, Ordering::SeqCst);
    let mutate = app(s.clone())
        .oneshot(
            Request::post("/api/dashboard/accounts/a/enabled")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mutate.status(), StatusCode::SERVICE_UNAVAILABLE);
    let read = app(s.clone())
        .oneshot(
            Request::get("/api/dashboard/accounts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
}

#[tokio::test]
async fn an_account_mutation_counts_toward_the_drain() {
    let s = state();
    let seen = Arc::new(AtomicI64::new(-1));
    let probe = seen.clone();
    let probe_state = s.clone();
    let router = Router::new()
        .route(
            "/api/dashboard/accounts/{label}/enabled",
            post(move || {
                let probe = probe.clone();
                let st = probe_state.clone();
                async move {
                    probe.store(st.inflight.load(Ordering::SeqCst), Ordering::SeqCst);
                    "changed"
                }
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            s.clone(),
            app::data_plane_middleware,
        ))
        .with_state(s.clone());
    router
        .oneshot(
            Request::post("/api/dashboard/accounts/a/enabled")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert_eq!(s.inflight.load(Ordering::SeqCst), 0);
}

fn holding(
    permit: tokio::sync::OwnedSemaphorePermit,
    first: Result<KiroEvent, StreamError>,
) -> EventStream {
    Box::pin(async_stream::stream! {
        let _permit = permit;
        yield first;
        futures_util::future::pending::<()>().await;
    })
}

#[tokio::test]
async fn a_first_token_retry_releases_its_own_permit_first() {
    let sem = Arc::new(Semaphore::new(1));
    let first = holding(
        sem.clone().try_acquire_owned().unwrap(),
        Err(StreamError::FirstTokenTimeout(0.15)),
    );
    let reconnects = Arc::new(AtomicUsize::new(0));
    let (sem2, count) = (sem.clone(), reconnects.clone());
    let stream = retry_on_first_token_timeout(first, 2, 0.15, move || {
        let (sem, count) = (sem2.clone(), count.clone());
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            let permit = tokio::time::timeout(Duration::from_secs(1), sem.acquire_owned())
                .await
                .map_err(|_| StreamError::Upstream("queue timeout".into()))?
                .unwrap();
            Ok(holding(permit, Ok(KiroEvent::Content("hi".into()))))
        }
    });
    let first_event = tokio::time::timeout(Duration::from_secs(2), Box::pin(stream).next())
        .await
        .expect("retry must not wait for its own slot")
        .unwrap();
    assert!(matches!(first_event, Ok(KiroEvent::Content(ref c)) if c == "hi"));
    assert_eq!(reconnects.load(Ordering::SeqCst), 1);
}
