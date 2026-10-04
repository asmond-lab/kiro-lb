//! The private InferX execution contract. Reject unsupported modalities rather
//! than letting the general-purpose converters silently discard them.
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashSet;

pub const MAX_BODY: usize = 8 * 1024 * 1024 + 4096;

pub fn metering() -> Value {
    json!({"version":1,"provider":"kiro","unit":"token",
        "input":{"source":"estimated","method":"kiro-lb:payload-cl100k-image-area-v1"},
        "output":{"source":"estimated","method":"kiro-lb:model-tokenizer-cjk-text-reasoning-tools-v1"}})
}

fn fields(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|map| map.keys().all(|k| allowed.contains(&k.as_str())))
}

fn text(value: &Value, max: usize) -> bool {
    value.as_str().is_some_and(|s| s.chars().count() <= max)
}

fn identifier(value: &Value, max: usize) -> bool {
    text(value, max) && value.as_str().is_some_and(|s| !s.is_empty())
}

fn name(value: &Value) -> bool {
    identifier(value, 64)
        && value
            .as_str()
            .unwrap()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn image(value: &Value) -> bool {
    if !fields(value, &["url", "detail"]) || value.get("detail").is_some_and(|v| v != "auto") {
        return false;
    }
    let Some((header, encoded)) = value["url"].as_str().and_then(|s| s.split_once(',')) else {
        return false;
    };
    if encoded.len() > 7_000_000 {
        return false;
    }
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    if bytes.len() > 5 * 1024 * 1024 {
        return false;
    }
    match header {
        "data:image/png;base64" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "data:image/jpeg;base64" => bytes.starts_with(b"\xff\xd8\xff"),
        "data:image/gif;base64" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "data:image/webp;base64" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}

fn content(value: &Value, images: &mut usize) -> bool {
    if value.is_string() {
        return text(value, 100_000);
    }
    let Some(parts) = value.as_array().filter(|v| !v.is_empty() && v.len() <= 64) else {
        return false;
    };
    parts.iter().all(|part| match part["type"].as_str() {
        Some("text") => fields(part, &["type", "text"]) && text(&part["text"], 100_000),
        Some("image_url") => {
            *images += 1;
            fields(part, &["type", "image_url"]) && image(&part["image_url"])
        }
        _ => false,
    })
}

fn calls(value: &Value, pending: &mut HashSet<String>, seen: &mut HashSet<String>) -> bool {
    let Some(calls) = value.as_array().filter(|v| !v.is_empty() && v.len() <= 32) else {
        return false;
    };
    calls.iter().all(|call| {
        if !fields(call, &["id", "type", "function"])
            || call["type"] != "function"
            || !identifier(&call["id"], 256)
            || !fields(&call["function"], &["name", "arguments"])
            || !name(&call["function"]["name"])
            || !text(&call["function"]["arguments"], 100_000)
        {
            return false;
        }
        let args = call["function"]["arguments"].as_str().unwrap();
        if !serde_json::from_str::<Value>(args).is_ok_and(|v| v.is_object()) {
            return false;
        }
        let id = call["id"].as_str().unwrap().to_owned();
        pending.insert(id.clone());
        seen.insert(id)
    })
}

fn messages(value: &Value) -> bool {
    let Some(messages) = value.as_array().filter(|v| !v.is_empty() && v.len() <= 128) else {
        return false;
    };
    let mut images = 0;
    let mut pending = HashSet::new();
    let mut seen = HashSet::new();
    for message in messages {
        let valid = match message["role"].as_str() {
            Some("tool") => {
                fields(message, &["role", "content", "tool_call_id"])
                    && content(&message["content"], &mut images)
                    && message["tool_call_id"]
                        .as_str()
                        .is_some_and(|id| pending.remove(id))
            }
            _ if !pending.is_empty() => false,
            Some("system" | "developer") => {
                fields(message, &["role", "content"]) && text(&message["content"], 100_000)
            }
            Some("user") => {
                fields(message, &["role", "content"]) && content(&message["content"], &mut images)
            }
            Some("assistant") => {
                fields(message, &["role", "content", "tool_calls"])
                    && (text(&message["content"], 100_000)
                        || message.get("content") == Some(&Value::Null)
                            && message.get("tool_calls").is_some())
                    && message
                        .get("tool_calls")
                        .is_none_or(|v| calls(v, &mut pending, &mut seen))
            }
            _ => false,
        };
        if !valid {
            return false;
        }
    }
    pending.is_empty() && images <= 4
}

fn tools(value: &Value) -> bool {
    let Some(tools) = value.as_array().filter(|v| v.len() <= 64) else {
        return false;
    };
    let mut names = HashSet::new();
    tools.iter().all(|tool| {
        let f = &tool["function"];
        fields(tool, &["type", "function"])
            && tool["type"] == "function"
            && fields(f, &["name", "description", "parameters", "strict"])
            && name(&f["name"])
            && names.insert(f["name"].as_str().unwrap())
            && f.get("description").is_none_or(|v| text(v, 100_000))
            && f["parameters"].is_object()
            && f["parameters"]["type"] == "object"
            && f.get("strict").is_none_or(|v| v == false)
    })
}

pub fn valid(request: &Value) -> bool {
    fields(
        request,
        &[
            "model",
            "messages",
            "max_tokens",
            "stream",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "stream_options",
        ],
    ) && identifier(&request["model"], 256)
        && request["max_tokens"]
            .as_i64()
            .is_some_and(|n| (1..=4096).contains(&n))
        && request["stream"].is_boolean()
        && messages(&request["messages"])
        && request.get("tools").is_none_or(tools)
        && request.get("tool_choice").is_none_or(|v| v == "auto")
        && request
            .get("parallel_tool_calls")
            .is_none_or(Value::is_boolean)
        && request
            .get("stream_options")
            .is_none_or(|v| fields(v, &["include_usage"]) && v["include_usage"].is_boolean())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_silent_converter_fallbacks_and_incomplete_tool_history() {
        let base = json!({"model":"claude-sonnet-4","stream":true,"max_tokens":32,"messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}]});
        assert!(valid(&base));
        for (field, value) in [
            ("tool_choice", json!("required")),
            ("parallel_tool_calls", json!("false")),
            ("tools", json!([{"type":"web_search"}])),
            ("store", json!(false)),
            (
                "messages",
                json!([{"role":"tool","tool_call_id":"missing","content":"orphan"}]),
            ),
            (
                "messages",
                json!([{"role":"assistant","content":null,"tool_calls":[{"type":"function","id":"call-1","function":{"name":"test","arguments":"{}"}}]}]),
            ),
        ] {
            let mut request = base.clone();
            request[field] = value;
            assert!(!valid(&request), "accepted {request}");
        }
        for url in [
            "https://127.0.0.1/private",
            "data:image/svg+xml;base64,PHN2Zz4=",
            "data:image/png;base64,%%%",
            "data:image/png;base64,aGVsbG8=",
        ] {
            let mut request = base.clone();
            request["messages"][0]["content"] =
                json!([{"type":"image_url","image_url":{"url":url}}]);
            assert!(!valid(&request));
        }
    }

    #[test]
    fn complete_parallel_tool_results_are_required_in_either_result_order() {
        let mut request = json!({"model":"claude-sonnet-4","stream":false,"max_tokens":32,"messages":[
            {"role":"user","content":"hello"},
            {"role":"assistant","content":null,"tool_calls":[
                {"id":"a","type":"function","function":{"name":"one","arguments":"{\"x\":1}"}},
                {"id":"b","type":"function","function":{"name":"two","arguments":"{\"x\":2}"}}
            ]},
            {"role":"tool","tool_call_id":"b","content":"two"},
            {"role":"tool","tool_call_id":"a","content":"one"}
        ]});
        assert!(valid(&request));
        request["messages"][3]["tool_call_id"] = json!("b");
        assert!(!valid(&request));
    }
}
