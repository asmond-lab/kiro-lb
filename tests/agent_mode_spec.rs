use serde_json::json;

#[test]
fn spec_mode_keeps_the_vibe_task_type_like_the_ide() {
    std::env::set_var("KIRO_AGENT_TASK_TYPE", "spec");
    let request = json!({
        "model": "claude-opus-5.5",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let out = kiro_lb::convert_anthropic::anthropic_to_kiro(&request, "conv-1", "").unwrap();
    assert_eq!(out.payload["agentMode"], json!("spec"));
    assert_eq!(
        out.payload["conversationState"]["agentTaskType"],
        json!("vibe")
    );
}
