use futures_util::StreamExt;
use kiro_lb::auth::{KiroAuth, Source};
use kiro_lb::model_resolver::ModelInfoCache;
use kiro_lb::parser::MeteringEvent;
use kiro_lb::stream_anthropic::StreamCtx;
use kiro_lb::stream_core::{self, EventStream, KiroEvent, StreamError};
use kiro_lb::stream_openai::{self, OpenAIOptions};
use kiro_lb::upstream::http::Transport;
use kiro_lb::usage_tracking::RequestCtx;
use serde_json::{json, Value};
use std::sync::Arc;

const PRECOMPUTED: i64 = 157;

fn ctx() -> (StreamCtx, RequestCtx) {
    let http = reqwest::Client::new();
    let request = RequestCtx::new(None);
    let ctx = StreamCtx {
        model: "claude-sonnet-4.5".into(),
        models: Arc::new(ModelInfoCache::new()),
        auth: Arc::new(
            KiroAuth::new(
                Source::File("/nonexistent/creds.json".into()),
                "us-east-1",
                None,
                http.clone(),
            )
            .unwrap(),
        ),
        transport: Arc::new(Transport { shared: http }),
        input_tokens: PRECOMPUTED,
        request: request.clone(),
        search_followup: None,
    };
    (ctx, request)
}

fn no_context_usage() -> EventStream {
    let events: Vec<Result<KiroEvent, StreamError>> = vec![
        Ok(KiroEvent::Content("Hello there".into())),
        Ok(KiroEvent::Metering(
            MeteringEvent::parse(json!({"unit": "credit", "usage": 0.02})).unwrap(),
        )),
        Ok(KiroEvent::StopReason("end_turn".into())),
    ];
    Box::pin(futures_util::stream::iter(events))
}

fn metered(events: EventStream, request: &RequestCtx) -> EventStream {
    stream_core::meter_generation(events, request)
}

fn opts() -> OpenAIOptions {
    OpenAIOptions {
        include_reasoning: true,
        parallel_tool_calls: true,
        request_messages: vec![],
        request_tools: vec![],
    }
}

#[tokio::test]
async fn streaming_chat_reports_the_precomputed_input_count() {
    let (ctx, request) = ctx();
    let chunks: Vec<String> =
        stream_openai::stream(metered(no_context_usage(), &request), ctx, opts())
            .map(|c| c.unwrap())
            .collect()
            .await;
    let last_usage = chunks
        .iter()
        .rev()
        .filter_map(|c| c.strip_prefix("data: "))
        .filter_map(|b| serde_json::from_str::<Value>(b.trim()).ok())
        .find_map(|v| v.get("usage").filter(|u| !u.is_null()).cloned())
        .expect("usage chunk");
    assert_eq!(last_usage["prompt_tokens"], PRECOMPUTED);
    assert_eq!(request.usage.lock().input_tokens, Some(PRECOMPUTED));
}

#[tokio::test]
async fn non_streaming_chat_reports_the_precomputed_input_count() {
    let (ctx, request) = ctx();
    let v = stream_openai::collect(metered(no_context_usage(), &request), ctx, opts(), false)
        .await
        .unwrap();
    assert_eq!(v["usage"]["prompt_tokens"], PRECOMPUTED);
    assert_eq!(request.usage.lock().input_tokens, Some(PRECOMPUTED));
}

#[tokio::test]
async fn streaming_responses_reports_the_precomputed_input_count() {
    let (ctx, request) = ctx();
    let chat = stream_openai::stream(metered(no_context_usage(), &request), ctx, opts());
    let out: Vec<String> =
        kiro_lb::stream_responses::translate(chat, "m".into(), "resp_1".into(), Default::default())
            .map(|c| c.unwrap())
            .collect()
            .await;
    let completed = out
        .iter()
        .find(|s| s.starts_with("event: response.completed"))
        .expect("completed");
    let v: Value = serde_json::from_str(
        completed
            .lines()
            .find_map(|l| l.strip_prefix("data: "))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(v["response"]["usage"]["input_tokens"], PRECOMPUTED);
    assert_eq!(request.usage.lock().input_tokens, Some(PRECOMPUTED));
}

#[tokio::test]
async fn non_streaming_responses_reports_the_precomputed_input_count() {
    let (ctx, request) = ctx();
    let chat = stream_openai::collect(metered(no_context_usage(), &request), ctx, opts(), false)
        .await
        .unwrap();
    let response =
        kiro_lb::convert_responses::chat_completion_to_responses(&chat, "m", &Default::default());
    assert_eq!(response["usage"]["input_tokens"], PRECOMPUTED);
    assert_eq!(request.usage.lock().input_tokens, Some(PRECOMPUTED));
}
