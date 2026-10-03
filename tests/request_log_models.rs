use kiro_lb::dashboard_store::{grouped_models, spellings_of};
use kiro_lb::usage_tracking::{tokens_per_second, tool_call_text, RequestCtx};
use serde_json::json;

#[test]
fn the_model_filter_joins_every_spelling_of_one_model() {
    let known: Vec<String> = [
        "claude-opus-5-5",
        "claude-opus-5-5[1m]",
        "claude-opus-5.5",
        "claude-sonnet-4.6",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let mut spellings = spellings_of("claude-opus-5-5", &known);
    spellings.sort();
    spellings.dedup();
    assert_eq!(
        spellings,
        ["claude-opus-5-5", "claude-opus-5-5[1m]", "claude-opus-5.5"]
    );
    assert_eq!(
        grouped_models(&known),
        ["claude-opus-5-5", "claude-sonnet-4-6"]
    );
}

#[test]
fn the_effort_recorded_is_the_one_sent_to_kiro() {
    let ctx = RequestCtx::new(None);
    ctx.note_effort(
        &json!({"additionalModelRequestFields": {"output_config": {"effort": "xhigh"}}}),
    );
    assert_eq!(ctx.usage.lock().effort.as_deref(), Some("xhigh"));
    ctx.note_effort(&json!({"additionalModelRequestFields": {"reasoning": {"effort": "none"}}}));
    assert_eq!(ctx.usage.lock().effort.as_deref(), Some("none"));
    ctx.note_effort(&json!({}));
    assert_eq!(ctx.usage.lock().effort, None);
}

#[test]
fn tool_calls_count_as_output() {
    let text = tool_call_text([("Read", "{\"file_path\":\"/x\"}".to_owned())]);
    assert_eq!(text, "Read{\"file_path\":\"/x\"}");
    assert!(tokens_per_second(40, 2000).is_some());
}
