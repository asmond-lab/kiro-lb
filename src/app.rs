//! Shared application state and the request-log middleware.

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::dashboard_store::{self, RequestRecord};
use crate::pool::AccountManager;
use crate::upstream::http::Transport;
use crate::usage_tracking::RequestCtx;

pub struct AppState {
    pub pool: Arc<AccountManager>,
    pub transport: Arc<Transport>,
    pub http: reqwest::Client,
    pub started_at: f64,
    pub quiesced: AtomicBool,
    pub inflight: AtomicI64,
    pub drained: tokio::sync::Notify,
}

pub type Shared = Arc<AppState>;

pub fn json_response(status: u16, body: Value) -> Response {
    let mut r = (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        axum::Json(body),
    )
        .into_response();
    r.headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    r
}

pub fn detail(status: u16, message: impl Into<String>) -> Response {
    json_response(status, json!({"detail": message.into()}))
}

pub fn anthropic_error(status: u16, kind: &str, message: impl Into<String>) -> Response {
    json_response(
        status,
        json!({"type": "error", "error": {"type": kind, "message": message.into()}}),
    )
}

pub fn openai_error_type(status: u16) -> &'static str {
    match status {
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        400..=499 => "invalid_request_error",
        _ => "api_error",
    }
}

pub fn openai_error(status: u16, message: impl Into<String>) -> Response {
    json_response(
        status,
        json!({"error": {"message": message.into(), "type": openai_error_type(status), "param": null, "code": null}}),
    )
}

pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| peer.map(|p| p.ip().to_string()))
}

/// Records every /v1 request once its body finishes, off the runtime, and
/// gates new work while a blue/green handoff drains the slot.
pub async fn data_plane_middleware(
    State(state): State<Shared>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_owned();
    if !path.starts_with("/v1/") {
        return next.run(req).await;
    }
    if state.quiesced.load(Ordering::SeqCst) {
        return openai_error(503, "Service temporarily unavailable");
    }
    let started = Instant::now();
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let ip = client_ip(req.headers(), peer);
    let ua = req
        .headers()
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let ctx = RequestCtx::new(None);
    let mut req = req;
    req.extensions_mut().insert(ctx.clone());
    state.inflight.fetch_add(1, Ordering::SeqCst);
    let response = next.run(req).await;
    let status = response.status().as_u16();
    let (parts, body) = response.into_parts();
    let guard = InflightGuard {
        state: state.clone(),
    };
    let mut stream = body.into_data_stream();
    let relay = async_stream::stream! {
        let _guard = guard;
        while let Some(chunk) = stream.next().await {
            yield chunk;
        }
        let u = ctx.usage.lock().clone();
        let record = RequestRecord {
            route: path,
            model: u.model,
            status,
            latency_ms: started.elapsed().as_millis() as i64,
            client_ip: ip,
            user_agent: ua,
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            credits: u.credits,
            generation_ms: u.generation_ms,
        };
        let _ = tokio::task::spawn_blocking(move || dashboard_store::record_request(record)).await;
    };
    Response::from_parts(parts, Body::from_stream(relay))
}

struct InflightGuard {
    state: Shared,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if self.state.inflight.fetch_sub(1, Ordering::SeqCst) <= 1 {
            self.state.drained.notify_waiters();
        }
    }
}

pub fn bytes_body(b: Bytes) -> Body {
    Body::from(b)
}
