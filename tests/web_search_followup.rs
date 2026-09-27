use futures_util::StreamExt;
use kiro_lb::auth::{KiroAuth, Source};
use kiro_lb::model_resolver::ModelInfoCache;
use kiro_lb::parser::MeteringEvent;
use kiro_lb::stream_anthropic::{self, SearchFollowup, StreamCtx};
use kiro_lb::stream_core::{self, EventStream, KiroEvent, StreamError};
use kiro_lb::upstream::http::Transport;
use kiro_lb::usage_tracking::RequestCtx;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

fn data_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("kirolb-websearch-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("DASHBOARD_DATA_DIR", &dir);
        let creds = dir.join("creds.json");
        std::fs::write(
            &creds,
            json!({"accessToken": "access", "refreshToken": "refresh", "expiresAt": "2999-01-01T00:00:00Z", "region": "us-east-1"}).to_string(),
        )
        .unwrap();
        dir
    })
}

async fn mcp_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let Ok(n) = sock.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let inner =
                    json!({"results": [{"title": "t", "url": "https://e.x", "snippet": "s"}]})
                        .to_string();
                let body = json!({"jsonrpc": "2.0", "result": {"content": [{"type": "text", "text": inner}]}}).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

async fn ctx(followup: SearchFollowup) -> StreamCtx {
    let dir = data_dir();
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut auth = KiroAuth::new(
        Source::File(dir.join("creds.json").to_string_lossy().into_owned()),
        "us-east-1",
        None,
        http.clone(),
    );
    auth.q_host = mcp_server().await;
    StreamCtx {
        model: "claude-sonnet-4.5".into(),
        models: Arc::new(ModelInfoCache::new()),
        auth: Arc::new(auth),
        transport: Arc::new(Transport { shared: http }),
        input_tokens: 10,
        request: RequestCtx::new(None),
        search_followup: Some(followup),
    }
}

fn search_call() -> KiroEvent {
    KiroEvent::ToolUse(json!({"id": "toolu_search", "name": "web_search", "input": {"query": "q"}}))
}

fn holding(permit: tokio::sync::OwnedSemaphorePermit) -> EventStream {
    Box::pin(async_stream::stream! {
        let _permit = permit;
        yield Ok(search_call());
        futures_util::future::pending::<()>().await;
    })
}

fn finite() -> EventStream {
    Box::pin(futures_util::stream::iter(vec![Ok(search_call())]))
}

fn answer() -> EventStream {
    Box::pin(futures_util::stream::iter(vec![
        Ok(KiroEvent::Content("answer".into())),
        Ok(KiroEvent::StopReason("end_turn".into())),
    ]))
}

fn metering(credits: f64) -> KiroEvent {
    KiroEvent::Metering(MeteringEvent::parse(json!({"unit": "credit", "usage": credits})).unwrap())
}

fn failing() -> SearchFollowup {
    Arc::new(|_, _, _| Box::pin(async { Err(StreamError::UpstreamStatus(429)) }))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_followup_acquires_the_permit_the_original_stream_held() {
    let sem = Arc::new(Semaphore::new(1));
    let original = holding(sem.clone().try_acquire_owned().unwrap());
    let gate = sem.clone();
    let followup: SearchFollowup = Arc::new(move |_, _, _| {
        let gate = gate.clone();
        Box::pin(async move {
            match tokio::time::timeout(Duration::from_secs(1), gate.acquire_owned()).await {
                Ok(Ok(permit)) => {
                    let s: EventStream = Box::pin(async_stream::stream! {
                        let _permit = permit;
                        for ev in [KiroEvent::Content("answer".into()), KiroEvent::StopReason("end_turn".into())] {
                            yield Ok(ev);
                        }
                    });
                    Ok(s)
                }
                _ => Err(StreamError::Upstream("permit not released".into())),
            }
        })
    });
    let out = tokio::time::timeout(
        Duration::from_secs(10),
        stream_anthropic::stream(original, ctx(followup).await)
            .collect::<Vec<Result<String, StreamError>>>(),
    )
    .await
    .expect("stream finished");
    let chunks: Vec<String> = out
        .into_iter()
        .map(|c| c.expect("no stream error"))
        .collect();
    let body = chunks.concat();
    assert!(body.contains("\"answer\""), "{body}");
    assert!(body.contains("server_tool_use"), "{body}");
    assert!(!body.contains("\"type\":\"tool_use\""), "{body}");
    assert!(!body.contains("\"type\": \"tool_use\""), "{body}");
    assert!(body.contains("message_stop"), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streaming_followup_failure_fails_the_turn() {
    let out: Vec<Result<String, StreamError>> =
        stream_anthropic::stream(finite(), ctx(failing()).await)
            .collect()
            .await;
    assert!(
        matches!(out.last(), Some(Err(StreamError::UpstreamStatus(429)))),
        "{:?}",
        out.last()
    );
    let body: String = out
        .iter()
        .filter_map(|c| c.as_ref().ok().cloned())
        .collect();
    assert!(!body.contains("message_stop"), "{body}");
    assert!(!body.contains("\"type\":\"tool_use\""), "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_streaming_followup_failure_fails_the_turn() {
    let result = stream_anthropic::collect(finite(), ctx(failing()).await).await;
    assert!(
        matches!(result, Err(StreamError::UpstreamStatus(429))),
        "{result:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_followup_failure_never_credits_success() {
    let chunks = stream_anthropic::stream(finite(), ctx(failing()).await);
    let ended = Arc::new(AtomicBool::new(true));
    let seen = Arc::new(AtomicBool::new(false));
    let (e, s) = (ended.clone(), seen.clone());
    let body: Vec<_> = kiro_lb::routes_v1::sse_body(chunks, true, move |ok| {
        e.store(ok, Ordering::SeqCst);
        s.store(true, Ordering::SeqCst);
    })
    .collect()
    .await;
    assert!(!body.is_empty());
    assert!(seen.load(Ordering::SeqCst));
    assert!(!ended.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_followup_answer_replaces_the_client_tool_call() {
    let followup: SearchFollowup = Arc::new(|_, _, _| Box::pin(async { Ok(answer()) }));
    let v = stream_anthropic::collect(finite(), ctx(followup).await)
        .await
        .unwrap();
    let content = v["content"].as_array().unwrap();
    assert!(
        content.iter().any(|b| b["type"] == "server_tool_use"),
        "{v}"
    );
    assert!(content.iter().all(|b| b["type"] != "tool_use"), "{v}");
    assert!(
        content
            .iter()
            .any(|b| b["type"] == "text" && b["text"] == "answer"),
        "{v}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn search_followup_adds_its_physical_generation_credits() {
    let request = RequestCtx::new(None);
    let followup_request = request.clone();
    let followup: SearchFollowup = Arc::new(move |_, _, _| {
        let request = followup_request.clone();
        Box::pin(async move {
            let stream = Box::pin(futures_util::stream::iter(vec![
                Ok(metering(0.02)),
                Ok(KiroEvent::Content("answer".into())),
                Ok(KiroEvent::StopReason("end_turn".into())),
            ]));
            Ok(stream_core::meter_generation(stream, &request))
        })
    });
    let mut context = ctx(followup).await;
    context.request = request.clone();
    let first = Box::pin(futures_util::stream::iter(vec![
        Ok(metering(0.03)),
        Ok(search_call()),
    ]));
    let first = stream_core::meter_generation(first, &request);

    stream_anthropic::collect(first, context).await.unwrap();

    assert_eq!(request.usage.lock().credits, Some(0.05));
}
