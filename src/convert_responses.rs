//! OpenAI Responses API <-> Chat Completions. A translation facade: the request is
//! turned into a chat request and runs through the chat pipeline unchanged.
//!
//! Codex declares tools in an `additional_tools` input item nested in `namespace`
//! entries, and its shell is a `custom` grammar tool. Kiro needs a JSON schema, so a
//! freeform tool is bridged as a one-string-field function and unwrapped back into
//! a `custom_tool_call` item. Dropping it instead left the model inventing output.

use serde_json::{json, Map, Value};
use std::collections::HashSet;

pub const FREEFORM_BODY_FIELD: &str = "input";
const FORWARDABLE: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

pub fn new_response_id() -> String {
    format!("resp_{}", uuid::Uuid::new_v4().simple())
}

pub fn new_item_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn content_to_text(content: &Value) -> (String, Vec<Value>) {
    match content {
        Value::Null => (String::new(), vec![]),
        Value::String(s) => (s.clone(), vec![]),
        Value::Array(parts) => {
            let mut texts = Vec::new();
            let mut images = Vec::new();
            for part in parts.iter().filter(|p| p.is_object()) {
                let kind = s(part, "type").unwrap_or("");
                match kind {
                    "input_text" | "output_text" | "text" | "summary_text" => {
                        if let Some(t) = part.get("text").filter(|t| truthy(t)) {
                            texts.push(py_str(t));
                        }
                    }
                    "input_image" | "image_url" | "image" => {
                        let url = match part.get("image_url") {
                            Some(Value::Object(o)) => o.get("url").cloned(),
                            other => other.cloned(),
                        };
                        if let Some(u) = url.filter(truthy) {
                            images.push(json!({"type": "image_url", "image_url": {"url": u}}));
                        }
                    }
                    _ => {
                        if let Some(t) = part.get("text").filter(|t| truthy(t)) {
                            texts.push(py_str(t));
                        }
                    }
                }
            }
            (texts.join("\n"), images)
        }
        other => (other.to_string(), vec![]),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}

fn call_id(item: &Value, generate: bool) -> String {
    s(item, "call_id")
        .filter(|v| !v.is_empty())
        .or_else(|| s(item, "id").filter(|v| !v.is_empty()))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if generate {
                new_item_id("call")
            } else {
                String::new()
            }
        })
}

fn flatten_tools(entries: Option<&Value>) -> Vec<Value> {
    let mut flat = Vec::new();
    for entry in entries
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e.is_object())
    {
        if s(entry, "type")
            .unwrap_or("")
            .eq_ignore_ascii_case("namespace")
        {
            flat.extend(flatten_tools(entry.get("tools")));
        } else {
            flat.push(entry.clone());
        }
    }
    flat
}

fn convert_input(input: Option<&Value>) -> (Vec<Value>, Vec<Value>) {
    let mut messages = Vec::new();
    let mut inline_tools = Vec::new();
    match input {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => {
            if !text.is_empty() {
                messages.push(json!({"role": "user", "content": text}));
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                let kind = s(item, "type")
                    .filter(|k| !k.is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        if item.get("role").is_some_and(truthy) {
                            "message".into()
                        } else {
                            String::new()
                        }
                    });
                match kind.as_str() {
                    "message" | "input_text" | "output_text" => {
                        let (text, images) =
                            content_to_text(item.get("content").unwrap_or(&Value::Null));
                        let role = s(item, "role").filter(|r| !r.is_empty()).unwrap_or("user");
                        if !images.is_empty() {
                            let mut parts = Vec::new();
                            if !text.is_empty() {
                                parts.push(json!({"type": "text", "text": text}));
                            }
                            parts.extend(images);
                            messages.push(json!({"role": role, "content": parts}));
                        } else if !text.is_empty() {
                            messages.push(json!({"role": role, "content": text}));
                        }
                    }
                    "function_call" | "custom_tool_call" => {
                        let arguments = match (item.get("arguments"), item.get("input")) {
                            (Some(Value::String(a)), _) => a.clone(),
                            (_, Some(Value::String(i))) => {
                                format!("{{\"input\": {}}}", Value::String(i.clone()))
                            }
                            _ => "{}".into(),
                        };
                        messages.push(json!({
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{"id": call_id(item, true), "type": "function", "function": {"name": s(item, "name").unwrap_or(""), "arguments": arguments}}],
                        }));
                    }
                    "function_call_output" | "custom_tool_call_output" => {
                        let content = match item.get("output") {
                            Some(Value::String(o)) => o.clone(),
                            Some(v @ Value::Array(_)) => content_to_text(v).0,
                            Some(Value::Object(o)) => {
                                match o
                                    .get("content")
                                    .filter(|v| truthy(v))
                                    .or_else(|| o.get("text").filter(|v| truthy(v)))
                                {
                                    Some(Value::String(t)) => t.clone(),
                                    _ => Value::Object(o.clone()).to_string(),
                                }
                            }
                            None | Some(Value::Null) => String::new(),
                            Some(other) => py_str(other),
                        };
                        messages.push(json!({"role": "tool", "content": content, "tool_call_id": call_id(item, false)}));
                    }
                    "additional_tools" => inline_tools.extend(flatten_tools(item.get("tools"))),
                    "reasoning" => tracing::debug!(
                        "Dropping reasoning item: reasoning is never replayed upstream"
                    ),
                    other => tracing::debug!(
                        "Dropping unsupported Responses input item: {}",
                        if other.is_empty() { "<untyped>" } else { other }
                    ),
                }
            }
        }
        _ => {}
    }
    (messages, inline_tools)
}

fn freeform_description(entry: &Value) -> String {
    let description = entry
        .get("description")
        .filter(|d| truthy(d))
        .map(py_str)
        .unwrap_or_default();
    let syntax = entry
        .get("format")
        .filter(|f| f.is_object())
        .and_then(|f| {
            f.get("syntax")
                .filter(|v| truthy(v))
                .or_else(|| f.get("type").filter(|v| truthy(v)))
        })
        .map(py_str)
        .unwrap_or_default();
    let mut note = format!(
        "\n\nCall this tool with a single JSON field `{FREEFORM_BODY_FIELD}` holding the complete tool body verbatim, exactly as it would be written by hand. Do not wrap it in markdown fences, do not escape it beyond what JSON requires, and do not split it into other fields."
    );
    if !syntax.is_empty() {
        note.push_str(&format!(" The body is {syntax} source."));
    }
    format!("{}{note}", description.trim_end())
}

pub fn freeform_tool_names(req: &Value) -> HashSet<String> {
    let mut declared: Vec<Value> = req
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for item in req
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if s(item, "type") == Some("additional_tools") {
            declared.extend(flatten_tools(item.get("tools")));
        }
    }
    flatten_tools(Some(&Value::Array(declared)))
        .iter()
        .filter(|e| s(e, "type").unwrap_or("").eq_ignore_ascii_case("custom"))
        .filter_map(|e| e.get("name").filter(|n| truthy(n)).map(py_str))
        .collect()
}

fn convert_tools(declared: Vec<Value>) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    for entry in flatten_tools(Some(&Value::Array(declared))) {
        let kind = s(&entry, "type")
            .filter(|k| !k.is_empty())
            .unwrap_or("function")
            .to_lowercase();
        if kind == "custom" {
            let Some(name) = entry.get("name").filter(|n| truthy(n)).map(py_str) else {
                continue;
            };
            out.push(json!({"type": "function", "function": {
                "name": name,
                "description": freeform_description(&entry),
                "parameters": {"type": "object", "properties": {FREEFORM_BODY_FIELD: {"type": "string", "description": "The complete tool body, verbatim."}}, "required": [FREEFORM_BODY_FIELD]},
            }}));
            continue;
        }
        if kind != "function" {
            continue;
        }
        let nested = entry
            .get("function")
            .filter(|f| f.is_object())
            .cloned()
            .unwrap_or(json!({}));
        let Some(name) = entry
            .get("name")
            .filter(|n| truthy(n))
            .or_else(|| nested.get("name").filter(|n| truthy(n)))
            .cloned()
        else {
            continue;
        };
        let description = entry
            .get("description")
            .filter(|d| !d.is_null())
            .or_else(|| nested.get("description"))
            .cloned()
            .unwrap_or(Value::Null);
        let parameters = entry
            .get("parameters")
            .filter(|d| !d.is_null())
            .or_else(|| nested.get("parameters"))
            .cloned()
            .unwrap_or(Value::Null);
        out.push(json!({"type": "function", "function": {"name": name, "description": description, "parameters": parameters}}));
    }
    (!out.is_empty()).then_some(out)
}

pub fn normalize_effort(effort: Option<&str>) -> Option<String> {
    let v = effort?.trim().to_lowercase();
    if v.is_empty() {
        return None;
    }
    if FORWARDABLE.contains(&v.as_str()) {
        return Some(v);
    }
    match v.as_str() {
        "ultra" | "persistent" => Some("max".into()),
        _ => None,
    }
}

pub fn responses_request_to_chat(req: &Value) -> Result<Value, String> {
    if req.get("previous_response_id").is_some_and(truthy) {
        return Err("previous_response_id is not supported: this gateway stores no responses. Send the full conversation in `input` with store=false.".into());
    }
    let mut messages = Vec::new();
    if let Some(i) = req.get("instructions").filter(|v| truthy(v)) {
        messages.push(json!({"role": "system", "content": i}));
    }
    let (converted, inline_tools) = convert_input(req.get("input"));
    messages.extend(converted);
    if messages.is_empty() {
        return Err("`input` must contain at least one message".into());
    }
    let mut payload = Map::new();
    payload.insert(
        "model".into(),
        req.get("model").cloned().unwrap_or(json!("")),
    );
    payload.insert("messages".into(), Value::Array(messages));
    payload.insert(
        "stream".into(),
        json!(req.get("stream").is_some_and(truthy)),
    );
    let mut declared: Vec<Value> = req
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    declared.extend(inline_tools);
    if let Some(tools) = convert_tools(declared) {
        payload.insert("tools".into(), Value::Array(tools));
    }
    match req.get("tool_choice") {
        Some(Value::String(c)) => {
            payload.insert("tool_choice".into(), json!(c));
        }
        Some(Value::Object(c)) => {
            let name = c
                .get("name")
                .filter(|n| truthy(n))
                .or_else(|| c.get("function").and_then(|f| f.get("name")));
            if c.get("type").and_then(Value::as_str) == Some("function") && name.is_some() {
                payload.insert(
                    "tool_choice".into(),
                    json!({"type": "function", "function": {"name": name}}),
                );
            } else {
                payload.insert("tool_choice".into(), Value::Object(c.clone()));
            }
        }
        _ => {}
    }
    for (from, to) in [
        ("parallel_tool_calls", "parallel_tool_calls"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
    ] {
        if let Some(v) = req.get(from).filter(|v| !v.is_null()) {
            payload.insert(to.into(), v.clone());
        }
    }
    if let Some(m) = req.get("max_output_tokens").filter(|v| truthy(v)) {
        payload.insert("max_tokens".into(), m.clone());
    }
    if let Some(e) = normalize_effort(req.pointer("/reasoning/effort").and_then(Value::as_str)) {
        payload.insert("reasoning_effort".into(), json!(e));
    }
    Ok(Value::Object(payload))
}

pub fn usage_block(usage: Option<&Value>) -> Value {
    let get = |k: &str| {
        usage
            .and_then(|u| u.get(k))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    let (p, c) = (get("prompt_tokens"), get("completion_tokens"));
    let total = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(Value::as_i64)
        .filter(|t| *t != 0)
        .unwrap_or(p + c);
    json!({"input_tokens": p, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": c, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": total})
}

pub fn message_item(text: &str, id: &str, status: &str) -> Value {
    json!({"id": id, "type": "message", "role": "assistant", "status": status, "content": [{"type": "output_text", "text": text, "annotations": []}]})
}

pub fn reasoning_item(text: &str, id: &str) -> Value {
    let summary = if text.is_empty() {
        json!([])
    } else {
        json!([{"type": "summary_text", "text": text}])
    };
    json!({"id": id, "type": "reasoning", "summary": summary, "content": [], "encrypted_content": null})
}

fn args_string(arguments: Option<&Value>) -> String {
    match arguments {
        Some(Value::String(a)) => a.clone(),
        Some(v) if truthy(v) => v.to_string(),
        _ => "{}".into(),
    }
}

pub fn freeform_body(arguments: Option<&Value>) -> String {
    let arguments = args_string(arguments);
    let Ok(parsed) = serde_json::from_str::<Value>(&arguments) else {
        return arguments;
    };
    match parsed {
        Value::String(s) => s,
        Value::Object(o) => {
            if let Some(Value::String(v)) = o.get(FREEFORM_BODY_FIELD) {
                return v.clone();
            }
            if o.len() == 1 {
                if let Some(Value::String(v)) = o.values().next() {
                    return v.clone();
                }
            }
            arguments
        }
        _ => arguments,
    }
}

pub fn output_item_for(call: &Value, id: &str, freeform: &HashSet<String>) -> Value {
    let f = call.get("function").cloned().unwrap_or(json!({}));
    let name = s(&f, "name").unwrap_or("").to_owned();
    let cid = s(call, "id")
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| new_item_id("call"));
    if freeform.contains(&name) {
        json!({"id": id, "type": "custom_tool_call", "status": "completed", "name": name, "input": freeform_body(f.get("arguments")), "call_id": cid})
    } else {
        json!({"id": id, "type": "function_call", "status": "completed", "name": name, "arguments": args_string(f.get("arguments")), "call_id": cid})
    }
}

pub fn response_envelope(
    response_id: &str,
    model: &str,
    status: &str,
    output: Vec<Value>,
    usage: Option<Value>,
    created_at: Option<i64>,
    error: Value,
) -> Value {
    let mut env = json!({
        "id": response_id, "object": "response",
        "created_at": created_at.filter(|c| *c != 0).unwrap_or_else(crate::store::now_i64),
        "status": status, "model": model, "output": output,
        "parallel_tool_calls": true, "tool_choice": "auto", "tools": [], "metadata": {},
        "error": error, "incomplete_details": null, "instructions": null,
    });
    if let Some(u) = usage {
        env["usage"] = u;
    }
    env
}

pub fn chat_completion_to_responses(
    body: &Value,
    model: &str,
    freeform: &HashSet<String>,
) -> Value {
    let choice = body.pointer("/choices/0").cloned().unwrap_or(json!({}));
    let message = choice.get("message").cloned().unwrap_or(json!({}));
    let mut output = Vec::new();
    let reasoning = message
        .get("reasoning")
        .filter(|v| truthy(v))
        .or_else(|| message.get("reasoning_content").filter(|v| truthy(v)));
    if let Some(r) = reasoning {
        output.push(reasoning_item(&py_str(r), &new_item_id("rs")));
    }
    let text = message
        .get("content")
        .filter(|v| truthy(v))
        .map(py_str)
        .unwrap_or_default();
    if !text.is_empty() {
        output.push(message_item(&text, &new_item_id("msg"), "completed"));
    }
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|c| c.is_object())
    {
        output.push(output_item_for(call, &new_item_id("fc"), freeform));
    }
    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
        .unwrap_or("stop");
    let status = if finish == "length" {
        "incomplete"
    } else {
        "completed"
    };
    let model = s(body, "model").filter(|m| !m.is_empty()).unwrap_or(model);
    let mut env = response_envelope(
        &new_response_id(),
        model,
        status,
        output,
        Some(usage_block(body.get("usage"))),
        body.get("created").and_then(Value::as_i64),
        Value::Null,
    );
    if status == "incomplete" {
        env["incomplete_details"] = json!({"reason": "max_output_tokens"});
    }
    env["output_text"] = json!(text);
    env
}
