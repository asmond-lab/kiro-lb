use kiro_lb::convert_anthropic::anthropic_to_kiro;
use kiro_lb::convert_openai::openai_to_kiro;
use kiro_lb::payload_guard;
use serde_json::{json, Value};

fn first_history_text(payload: &Value) -> String {
    payload
        .pointer("/conversationState/history/0/userInputMessage/content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn history_prefix(payload: &Value, turns: usize) -> Vec<Value> {
    payload
        .pointer("/conversationState/history")
        .and_then(Value::as_array)
        .map(|h| h.iter().take(turns).cloned().collect())
        .unwrap_or_default()
}

#[test]
fn anthropic_mid_conversation_system_messages_keep_the_prompt_prefix() {
    let turn_one = json!({
        "model": "claude-opus-5.5", "max_tokens": 64,
        "system": "You are a coding agent.",
        "messages": [
            {"role": "user", "content": "fix the bug"},
            {"role": "assistant", "content": "looking"},
            {"role": "system", "content": "<total_tokens>1000 tokens left</total_tokens>"},
            {"role": "user", "content": "continue"}
        ]
    });
    let turn_two = json!({
        "model": "claude-opus-5.5", "max_tokens": 64,
        "system": "You are a coding agent.",
        "messages": [
            {"role": "user", "content": "fix the bug"},
            {"role": "assistant", "content": "looking"},
            {"role": "system", "content": "<total_tokens>1000 tokens left</total_tokens>"},
            {"role": "user", "content": "continue"},
            {"role": "assistant", "content": "done"},
            {"role": "system", "content": "<total_tokens>900 tokens left</total_tokens>"},
            {"role": "user", "content": "and the tests?"}
        ]
    });
    let a = anthropic_to_kiro(&turn_one, "c", "").unwrap().payload;
    let b = anthropic_to_kiro(&turn_two, "c", "").unwrap().payload;
    assert_eq!(first_history_text(&a), first_history_text(&b));
    assert!(!first_history_text(&b).contains("900 tokens left"));
    assert_eq!(history_prefix(&a, 2), history_prefix(&b, 2));
    let current = b
        .pointer("/conversationState/currentMessage/userInputMessage/content")
        .and_then(Value::as_str)
        .unwrap();
    assert!(
        current.starts_with(
            "<system-reminder>
<total_tokens>900 tokens left"
        ),
        "{current}"
    );
    assert!(current.ends_with("and the tests?"), "{current}");
}

#[test]
fn a_leading_system_message_is_still_the_system_prompt() {
    let req = json!({
        "model": "claude-opus-5.5", "max_tokens": 64,
        "messages": [
            {"role": "system", "content": "LEADING_SYSTEM"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
            {"role": "user", "content": "again"}
        ]
    });
    let p = anthropic_to_kiro(&req, "c", "").unwrap().payload;
    assert!(first_history_text(&p).starts_with("LEADING_SYSTEM"));
    assert!(!p.to_string().contains("system-reminder"));
}

#[test]
fn openai_mid_conversation_system_messages_keep_the_prompt_prefix() {
    let turn_one = json!({
        "model": "claude-opus-5.5",
        "messages": [
            {"role": "system", "content": "You are a coding agent."},
            {"role": "user", "content": "fix the bug"},
            {"role": "assistant", "content": "looking"},
            {"role": "system", "content": "reminder one"},
            {"role": "user", "content": "continue"}
        ]
    });
    let turn_two = json!({
        "model": "claude-opus-5.5",
        "messages": [
            {"role": "system", "content": "You are a coding agent."},
            {"role": "user", "content": "fix the bug"},
            {"role": "assistant", "content": "looking"},
            {"role": "system", "content": "reminder one"},
            {"role": "user", "content": "continue"},
            {"role": "assistant", "content": "done"},
            {"role": "developer", "content": "reminder two"},
            {"role": "user", "content": "and the tests?"}
        ]
    });
    let a = openai_to_kiro(&turn_one, "c", "").unwrap().payload;
    let b = openai_to_kiro(&turn_two, "c", "").unwrap().payload;
    assert!(first_history_text(&a).starts_with("You are a coding agent."));
    assert_eq!(first_history_text(&a), first_history_text(&b));
    assert!(!first_history_text(&b).contains("reminder two"));
    assert_eq!(history_prefix(&a, 2), history_prefix(&b, 2));
}

#[test]
fn thinking_signatures_are_not_measured_as_prompt_tokens() {
    let signature: String = "A".repeat(400_000);
    let with_sig = json!({"conversationState": {"history": [
        {"assistantResponseMessage": {"content": "ok", "reasoningContent": {"reasoningText": {"text": "short thought", "signature": signature}}}}
    ]}});
    let without = json!({"conversationState": {"history": [
        {"assistantResponseMessage": {"content": "ok", "reasoningContent": {"reasoningText": {"text": "short thought", "signature": ""}}}}
    ]}});
    let (with_tokens, with_bytes) = payload_guard::measure(&with_sig);
    let (base_tokens, base_bytes) = payload_guard::measure(&without);
    assert_eq!(with_tokens, base_tokens);
    assert_eq!(with_bytes, base_bytes);
    assert_eq!(
        with_sig.pointer("/conversationState/history/0/assistantResponseMessage/reasoningContent/reasoningText/signature").and_then(Value::as_str).map(str::len),
        Some(400_000)
    );
}
