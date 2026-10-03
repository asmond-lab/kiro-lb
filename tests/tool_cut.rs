use bytes::Bytes;
use futures_util::StreamExt;
use kiro_lb::stream_core::{self, KiroEvent, StreamError};

async fn run(bytes: &'static str) -> Vec<Result<KiroEvent, StreamError>> {
    let body: stream_core::ByteStream =
        Box::pin(futures_util::stream::iter(vec![
            Ok::<Bytes, reqwest::Error>(Bytes::from_static(bytes.as_bytes())),
        ]));
    stream_core::parse_kiro_stream(body, 1.0, 1.0)
        .collect()
        .await
}

fn stop_reasons(events: &[Result<KiroEvent, StreamError>]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Ok(KiroEvent::StopReason(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn tool_count(events: &[Result<KiroEvent, StreamError>]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Ok(KiroEvent::ToolUse(_))))
        .count()
}

#[tokio::test]
async fn a_tool_call_cut_off_ends_the_turn_as_max_tokens_instead_of_failing() {
    let events = run(r#"{"content":"Writing the file."}{"name":"Write","toolUseId":"t1","input":"{\"file_path\": \"/a.txt\""}"#).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(tool_count(&events), 0);
    assert_eq!(stop_reasons(&events), vec!["MAX_TOKENS"]);
}

#[tokio::test]
async fn the_opus_load_event_after_output_is_a_cut_not_an_error() {
    let events = run(r#"{"content":"Thinking out loud."}{"reason":"MODEL_TEMPORARILY_UNAVAILABLE","message":"Encountered unexpectedly high load"}"#).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(stop_reasons(&events), vec!["MAX_TOKENS"]);
}

#[tokio::test]
async fn the_load_event_before_any_output_stays_an_upstream_error() {
    let events = run(r#"{"reason":"MODEL_TEMPORARILY_UNAVAILABLE","message":"Encountered unexpectedly high load"}"#).await;
    assert!(
        matches!(events.last(), Some(Err(StreamError::Upstream(_)))),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_cut_after_a_complete_tool_keeps_the_tool_turn() {
    let events = run(r#"{"name":"Read","toolUseId":"t1","input":"{\"file_path\": \"/a.txt\"}","stop":true}{"name":"Write","toolUseId":"t2","input":"{\"file_path\": \"/b.txt\""}"#).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(tool_count(&events), 1);
    assert!(stop_reasons(&events).is_empty());
}

#[tokio::test]
async fn malformed_tool_input_that_is_not_a_cut_still_fails() {
    let events =
        run(r#"{"name":"Write","toolUseId":"t1","input":"{\"file_path\": tru}","stop":true}"#)
            .await;
    assert!(
        matches!(events.last(), Some(Err(StreamError::MalformedToolInput))),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_tool_only_response_cut_mid_argument_still_ends_as_max_tokens() {
    let events = run(
        r#"{"name":"Write","toolUseId":"t1","input":"{\"file_path\": \"/a.txt\"","stop":true}"#,
    )
    .await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(stop_reasons(&events), vec!["MAX_TOKENS"]);
}

#[tokio::test]
async fn an_empty_thinking_frame_does_not_turn_a_load_error_into_a_cut() {
    let events = run(r#"{"text":""}{"reason":"MODEL_TEMPORARILY_UNAVAILABLE","message":"Encountered unexpectedly high load"}"#).await;
    assert!(
        matches!(events.last(), Some(Err(StreamError::Upstream(_)))),
        "{events:?}"
    );
}

#[tokio::test]
async fn a_load_event_inside_an_unfinished_tool_call_is_a_cut() {
    let events = run(r#"{"name":"Write","toolUseId":"t1","input":"{\"file_path\": \"/a.txt\""}{"reason":"MODEL_TEMPORARILY_UNAVAILABLE","message":"Encountered unexpectedly high load"}"#).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(tool_count(&events), 0, "the partial call is dropped");
    assert_eq!(stop_reasons(&events), vec!["MAX_TOKENS"]);
}

#[tokio::test]
async fn a_load_event_inside_a_call_whose_partial_input_parses_still_drops_it() {
    let events = run(r#"{"name":"Write","toolUseId":"t1","input":"{\"file_path\": \"/a.txt\"}"}{"reason":"MODEL_TEMPORARILY_UNAVAILABLE","message":"Encountered unexpectedly high load"}"#).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        tool_count(&events),
        0,
        "a call cut before its stop frame is never emitted"
    );
    assert_eq!(stop_reasons(&events), vec!["MAX_TOKENS"]);
}
