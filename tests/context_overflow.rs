use kiro_lb::app::context_overflow_error;
use serde_json::Value;

async fn body(r: axum::response::Response) -> (u16, Value) {
    let status = r.status().as_u16();
    let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn anthropic_overflow_matches_the_prompt_too_long_shape() {
    let (status, v) = body(context_overflow_error(true, 1_050_000, 1_000_000)).await;
    assert_eq!(status, 400);
    assert_eq!(v["type"], "error");
    assert_eq!(v["error"]["type"], "invalid_request_error");
    let message = v["error"]["message"].as_str().unwrap();
    let re = regex::Regex::new(r"^prompt is too long: (\d+) tokens > (\d+) maximum$").unwrap();
    let c = re.captures(message).expect(message);
    assert_eq!(&c[1], "1050000");
    assert_eq!(&c[2], "1000000");
}

#[tokio::test]
async fn openai_overflow_carries_context_length_exceeded() {
    let (status, v) = body(context_overflow_error(false, 10, 200_000)).await;
    assert_eq!(status, 400);
    assert_eq!(v["error"]["code"], "context_length_exceeded");
    assert_eq!(v["error"]["type"], "invalid_request_error");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("maximum context length is 200000 tokens"),
        "{message}"
    );
    assert!(
        message.contains("resulted in 200001 tokens"),
        "an estimate below the limit is reported just over it: {message}"
    );
}
