use bytes::Bytes;
use futures_util::StreamExt;
use kiro_lb::auth::{KiroAuth, Source};
use kiro_lb::model_resolver::ModelInfoCache;
use kiro_lb::parser::{AwsEventStreamParser, MeteringEvent, ParsedEvent};
use kiro_lb::stream_anthropic::StreamCtx;
use kiro_lb::stream_core::{self, EventStream, KiroEvent, StreamError};
use kiro_lb::stream_openai::{self, OpenAIOptions};
use kiro_lb::upstream::http::Transport;
use kiro_lb::usage_tracking::RequestCtx;
use serde_json::{json, Value};
use std::sync::Arc;

fn reading(value: Value) -> MeteringEvent {
    MeteringEvent::parse(value).expect("valid metering event")
}

fn events(items: Vec<Result<KiroEvent, StreamError>>) -> EventStream {
    Box::pin(futures_util::stream::iter(items))
}

fn metered(request: &RequestCtx, items: Vec<Result<KiroEvent, StreamError>>) -> EventStream {
    stream_core::meter_generation(events(items), request)
}

fn context(request: RequestCtx) -> StreamCtx {
    let http = reqwest::Client::new();
    StreamCtx {
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
        input_tokens: 3,
        request,
        search_followup: None,
    }
}

fn options() -> OpenAIOptions {
    OpenAIOptions {
        include_reasoning: true,
        parallel_tool_calls: true,
        request_messages: vec![],
        request_tools: vec![],
    }
}

#[test]
fn parser_accepts_the_precise_credit_schema_and_amount_alias() {
    let precise = 0.045_823_315_091_210_62;
    let mut parser = AwsEventStreamParser::new();
    let parsed = parser.feed(
        format!(
            "headers{}headers{}",
            json!({"unit": "credit", "unitPlural": "credits", "usage": precise}),
            json!({"amount": 0.01, "unit": "credits"})
        )
        .as_bytes(),
    );
    let credits: Vec<f64> = parsed
        .iter()
        .filter_map(|event| match event {
            ParsedEvent::Metering(reading) => reading.credits(),
            _ => None,
        })
        .collect();
    assert_eq!(credits, vec![precise, 0.01]);
}

#[test]
fn schema_rejects_invalid_values_and_excludes_other_units() {
    for invalid in [
        json!({"unit": "credit"}),
        json!({"unit": "credit", "usage": true}),
        json!({"unit": "credit", "usage": "0.1"}),
        json!({"unit": "credit", "usage": -0.1}),
        json!({"unit": "credit", "usage": null}),
        json!({"unit": "credit", "usage": 0.1, "unitPlural": 4}),
    ] {
        assert!(MeteringEvent::parse(invalid).is_none());
    }
    assert!(
        MeteringEvent::parse(json!({"unit": "credit", "usage": 0.1, "amount": 2.0}))
            .is_some_and(|reading| reading.credits() == Some(0.1))
    );
    assert_eq!(
        reading(json!({"unit": "token", "usage": 10})).credits(),
        None
    );
    let mut parser = AwsEventStreamParser::new();
    assert!(parser
        .feed(br#"{"unit":"credit","usage":1e999}"#)
        .is_empty());
}

#[tokio::test]
async fn repeated_snapshots_replace_but_separate_generations_add() {
    let request = RequestCtx::new(None);
    let first = metered(
        &request,
        vec![
            Ok(KiroEvent::Metering(reading(
                json!({"unit": "credit", "usage": 0.04582331509121062}),
            ))),
            Ok(KiroEvent::Metering(reading(
                json!({"unit": "credits", "amount": 0}),
            ))),
        ],
    );
    first.collect::<Vec<_>>().await;
    assert_eq!(request.usage.lock().credits, Some(0.0));

    let second = metered(
        &request,
        vec![Ok(KiroEvent::Metering(reading(
            json!({"unit": "credit", "usage": 0.01}),
        )))],
    );
    second.collect::<Vec<_>>().await;
    assert_eq!(request.usage.lock().credits, Some(0.01));
}

#[tokio::test]
async fn missing_and_wrong_units_remain_unknown() {
    let request = RequestCtx::new(None);
    metered(&request, vec![Ok(KiroEvent::Content("answer".into()))])
        .collect::<Vec<_>>()
        .await;
    metered(
        &request,
        vec![Ok(KiroEvent::Metering(reading(
            json!({"unit": "token", "usage": 10}),
        )))],
    )
    .collect::<Vec<_>>()
    .await;
    assert_eq!(request.usage.lock().credits, None);
}

#[tokio::test]
async fn error_and_disconnect_retain_the_latest_observed_snapshot() {
    let failed = RequestCtx::new(None);
    let mut stream = metered(
        &failed,
        vec![
            Ok(KiroEvent::Metering(reading(
                json!({"unit": "credit", "usage": 0.03}),
            ))),
            Err(StreamError::Upstream("broken stream".into())),
        ],
    );
    assert!(stream.next().await.unwrap().is_ok());
    assert!(stream.next().await.unwrap().is_err());
    assert_eq!(failed.usage.lock().credits, Some(0.03));

    let disconnected = RequestCtx::new(None);
    let mut stream = metered(
        &disconnected,
        vec![
            Ok(KiroEvent::Metering(reading(
                json!({"unit": "credit", "usage": 0.04}),
            ))),
            Ok(KiroEvent::Content("unread".into())),
        ],
    );
    assert!(stream.next().await.unwrap().is_ok());
    drop(stream);
    assert_eq!(disconnected.usage.lock().credits, Some(0.04));
}

#[tokio::test]
async fn account_changes_do_not_relabel_or_erase_request_credits() {
    let request = RequestCtx::new(None);
    request.set_account("account-a");
    metered(
        &request,
        vec![Ok(KiroEvent::Metering(reading(
            json!({"unit": "credit", "usage": 0.02}),
        )))],
    )
    .collect::<Vec<_>>()
    .await;
    request.set_account("account-b");
    metered(
        &request,
        vec![Ok(KiroEvent::Metering(reading(
            json!({"unit": "credits", "usage": 0.01}),
        )))],
    )
    .collect::<Vec<_>>()
    .await;
    assert_eq!(request.usage.lock().credits, Some(0.03));
}

#[tokio::test]
async fn openai_stream_exposes_the_final_generation_snapshot_including_zero() {
    let request = RequestCtx::new(None);
    let upstream = metered(
        &request,
        vec![
            Ok(KiroEvent::Content("answer".into())),
            Ok(KiroEvent::Metering(reading(
                json!({"unit": "credit", "usage": 0.05}),
            ))),
            Ok(KiroEvent::Metering(reading(
                json!({"unit": "credits", "usage": 0}),
            ))),
            Ok(KiroEvent::StopReason("end_turn".into())),
        ],
    );
    let chunks: Vec<String> = stream_openai::stream(upstream, context(request.clone()), options())
        .map(|chunk| chunk.unwrap())
        .collect()
        .await;
    let usage = chunks
        .iter()
        .filter_map(|chunk| chunk.strip_prefix("data: "))
        .filter_map(|body| serde_json::from_str::<Value>(body.trim()).ok())
        .find_map(|chunk| chunk.get("usage").cloned())
        .expect("terminal usage");
    assert_eq!(usage["credits_used"], 0.0);
    assert_eq!(request.usage.lock().credits, Some(0.0));
}

#[tokio::test]
async fn parsed_route_pipeline_records_metering_for_non_streaming_openai() {
    let request = RequestCtx::new(None);
    let bytes: stream_core::ByteStream =
        Box::pin(futures_util::stream::iter(vec![
            Ok::<Bytes, reqwest::Error>(Bytes::from_static(
                br#"{"content":"answer"}{"unit":"credit","usage":0.0125}{"stopReason":"end_turn"}"#,
            )),
        ]));
    let parsed = stream_core::parse_kiro_stream(bytes, 1.0, 1.0);
    let metered = stream_core::meter_generation(parsed, &request);

    let response = stream_openai::collect(metered, context(request.clone()), options(), false)
        .await
        .unwrap();

    assert_eq!(response["usage"]["credits_used"], 0.0125);
    assert_eq!(request.usage.lock().credits, Some(0.0125));
}
