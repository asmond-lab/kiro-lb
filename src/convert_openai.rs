//! OpenAI Chat Completions request to the unified format, then to the Kiro payload.

use serde_json::{json, Value};

use crate::convert_core::{
    build_kiro_payload, extract_images_from_content, extract_text_content, BuildError,
    KiroPayloadResult, UnifiedMessage, UnifiedTool,
};
use crate::model_resolver::get_model_id_for_kiro;
use crate::native_thinking::{apply_native_thinking, supports_native_thinking};

const RAW_JSON_DIRECTIVE: &str = "Respond with ONLY a valid JSON object. Do not wrap the output in markdown code fences and do not add any text before or after the JSON.";

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn user_tool_results(content: &Value) -> Vec<Value> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| str_field(i, "type") == Some("tool_result"))
        .map(|i| {
            let text = extract_text_content(i.get("content").unwrap_or(&json!("")));
            json!({
                "type": "tool_result",
                "tool_use_id": i.get("tool_use_id").cloned().unwrap_or(json!("")),
                "content": if text.is_empty() { "(empty result)".to_owned() } else { text },
                "is_error": i.get("is_error") == Some(&Value::Bool(true)),
            })
        })
        .collect()
}

fn assistant_tool_calls(msg: &Value) -> Vec<Value> {
    msg.get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|tc| tc.is_object())
        .map(|tc| {
            let f = tc.get("function").cloned().unwrap_or(json!({}));
            json!({
                "id": tc.get("id").cloned().unwrap_or(json!("")),
                "type": "function",
                "function": {"name": f.get("name").cloned().unwrap_or(json!("")), "arguments": f.get("arguments").cloned().unwrap_or(json!("{}"))},
            })
        })
        .collect()
}

pub fn convert_messages(messages: &[Value]) -> (String, Vec<UnifiedMessage>) {
    let mut system = String::new();
    let mut rest = Vec::new();
    for m in messages {
        match str_field(m, "role") {
            Some("system") | Some("developer") => {
                system.push_str(&extract_text_content(
                    m.get("content").unwrap_or(&Value::Null),
                ));
                system.push('\n');
            }
            _ => rest.push(m),
        }
    }
    let system = system.trim().to_owned();
    let mut processed = Vec::new();
    let mut pending_results: Vec<Value> = Vec::new();
    let mut pending_images: Vec<Value> = Vec::new();
    let flush =
        |processed: &mut Vec<UnifiedMessage>, results: &mut Vec<Value>, images: &mut Vec<Value>| {
            if results.is_empty() {
                return;
            }
            processed.push(UnifiedMessage {
                role: "user".into(),
                content: json!(""),
                tool_results: Some(std::mem::take(results)),
                images: Some(std::mem::take(images)).filter(|i| !i.is_empty()),
                ..Default::default()
            });
        };
    for m in rest {
        let content = m.get("content").cloned().unwrap_or(Value::Null);
        let role = str_field(m, "role").unwrap_or("user");
        if role == "tool" {
            let text = extract_text_content(&content);
            pending_results.push(json!({
                "type": "tool_result",
                "tool_use_id": str_field(m, "tool_call_id").unwrap_or(""),
                "content": if text.is_empty() { "(empty result)".to_owned() } else { text },
                "is_error": m.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            }));
            if content.is_array() {
                pending_images.extend(extract_images_from_content(&content));
            }
            continue;
        }
        flush(&mut processed, &mut pending_results, &mut pending_images);
        let mut msg = UnifiedMessage {
            role: role.into(),
            content: Value::String(extract_text_content(&content)),
            ..Default::default()
        };
        if role == "assistant" {
            msg.tool_calls = Some(assistant_tool_calls(m)).filter(|c| !c.is_empty());
            msg.reasoning = str_field(m, "reasoning")
                .filter(|s| !s.is_empty())
                .or_else(|| str_field(m, "reasoning_content"))
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
        } else if role == "user" {
            msg.tool_results = Some(user_tool_results(&content)).filter(|r| !r.is_empty());
            msg.images = Some(extract_images_from_content(&content)).filter(|i| !i.is_empty());
        }
        processed.push(msg);
    }
    flush(&mut processed, &mut pending_results, &mut pending_images);
    (system, processed)
}

pub fn convert_tools(tools: Option<&Value>) -> Option<Vec<UnifiedTool>> {
    let list = tools?.as_array().filter(|t| !t.is_empty())?;
    let out: Vec<UnifiedTool> = list
        .iter()
        .filter(|t| str_field(t, "type").unwrap_or("function") == "function")
        .filter_map(|t| {
            if let Some(f) = t.get("function").filter(|f| f.is_object()) {
                Some(UnifiedTool {
                    name: str_field(f, "name").unwrap_or("").to_owned(),
                    description: str_field(f, "description").map(str::to_owned),
                    input_schema: f.get("parameters").cloned().filter(|v| !v.is_null()),
                })
            } else if let Some(name) = str_field(t, "name") {
                Some(UnifiedTool {
                    name: name.to_owned(),
                    description: str_field(t, "description").map(str::to_owned),
                    input_schema: t.get("input_schema").cloned().filter(|v| !v.is_null()),
                })
            } else {
                tracing::warn!("Skipping invalid tool: no function or name field found");
                None
            }
        })
        .collect();
    (!out.is_empty()).then_some(out)
}

pub fn response_format_requests_json(rf: Option<&Value>) -> bool {
    matches!(
        rf.and_then(|r| str_field(r, "type")),
        Some("json_object") | Some("json_schema")
    )
}

pub fn response_format_directive(rf: Option<&Value>) -> String {
    if !response_format_requests_json(rf) {
        return String::new();
    }
    let rf = rf.unwrap();
    if str_field(rf, "type") == Some("json_schema") {
        let spec = rf
            .get("json_schema")
            .filter(|s| s.is_object())
            .cloned()
            .unwrap_or(json!({}));
        let name = str_field(&spec, "name")
            .filter(|s| !s.is_empty())
            .unwrap_or("response")
            .to_owned();
        let schema = spec
            .get("schema")
            .filter(|s| !s.is_null())
            .map(Value::to_string)
            .unwrap_or_else(|| "{}".into());
        return format!("{RAW_JSON_DIRECTIVE} The JSON object MUST conform to the following JSON schema named \"{name}\":\n{schema}");
    }
    RAW_JSON_DIRECTIVE.into()
}

pub fn openai_to_kiro(
    req: &Value,
    conversation_id: &str,
    profile_arn: &str,
) -> Result<KiroPayloadResult, BuildError> {
    let empty = Vec::new();
    let (mut system, unified) = convert_messages(
        req.get("messages")
            .and_then(Value::as_array)
            .unwrap_or(&empty),
    );
    let directive = response_format_directive(req.get("response_format"));
    if !directive.is_empty() {
        system = if system.is_empty() {
            directive
        } else {
            format!("{system}\n\n{directive}")
        };
    }
    let tools = convert_tools(req.get("tools"));
    let model_id = get_model_id_for_kiro(str_field(req, "model").unwrap_or(""));
    let mut result = build_kiro_payload(
        unified,
        &system,
        &model_id,
        tools,
        conversation_id,
        profile_arn,
    )?;
    let effort = str_field(req, "reasoning_effort");
    if effort.is_some() && supports_native_thinking(&model_id) {
        apply_native_thinking(&mut result.payload, &model_id, effort);
        result.serialized = crate::payload_guard::compact_json(&result.payload);
    }
    Ok(result)
}
