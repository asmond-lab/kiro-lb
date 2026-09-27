//! Failure capture and offline replay. Capture is off unless DEBUG_MODE is set;
//! bundles are redacted by default, size-bounded, written atomically with 0600,
//! and retained up to DEBUG_CAPTURE_RETENTION.

use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::config::{self, DebugMode};
use crate::stream_core::{AnthropicValidator, OpenAIValidator};

const SENSITIVE_KEYS: &[&str] = &[
    "authorization",
    "proxyauthorization",
    "xapikey",
    "apikey",
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "clientsecret",
    "cookie",
    "setcookie",
    "signature",
    "thinkingsignature",
    "profilearn",
    "password",
    "token",
    "secret",
    "credential",
    "privatekey",
];

const SENSITIVE_SUFFIXES: &[&str] = &[
    "token",
    "secret",
    "password",
    "credential",
    "privatekey",
    "accesskey",
    "accesskeyid",
    "secretaccesskey",
    "session",
    "sessionid",
    "sessionkey",
];

/// These strings are structural only inside protocol envelopes, never inside
/// opaque metadata, tool inputs, schemas, or JSON-encoded prompt text.
const STRUCTURAL_KEYS: &[&str] = &[
    "type",
    "role",
    "model",
    "modelid",
    "name",
    "id",
    "tooluseid",
    "toolcallid",
    "callid",
    "stopreason",
    "finishreason",
    "status",
    "format",
    "origin",
    "chattriggertype",
    "agentmode",
    "event",
    "object",
];

fn protocol_container(key: &str, value: &Value, root: bool) -> bool {
    match key {
        "input" => root && value.is_array(),
        "content" | "system" => value.is_array(),
        "messages"
        | "history"
        | "conversationstate"
        | "currentmessage"
        | "userinputmessage"
        | "assistantresponsemessage"
        | "userinputmessagecontext"
        | "tools"
        | "function"
        | "toolspecification"
        | "tooluses"
        | "toolcalls"
        | "toolresults"
        | "images"
        | "source"
        | "choices"
        | "delta"
        | "message"
        | "contentblock"
        | "usage"
        | "response"
        | "output"
        | "item"
        | "part" => true,
        _ => false,
    }
}

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            r"(?i)\bbearer\s+[A-Za-z0-9._\-~+/=]+",
            r"\bklb_[A-Za-z0-9_\-]{8,}",
            r"\bapik_[A-Za-z0-9_\-]{8,}",
            r"\baoa[A-Za-z0-9_\-]{20,}",
            r"\beyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{5,}\.[A-Za-z0-9_\-]{5,}",
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            r#"(?i)[?&](?:key|token|signature|credential)=[^&#\s"'<>\\{}\[\],]+"#,
            r"arn:aws:codewhisperer:[a-z0-9-]+:\d+:profile/[A-Za-z0-9]+",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    })
}

pub fn redact_patterns(text: &str) -> String {
    let mut out = text.to_owned();
    for re in patterns() {
        out = re.replace_all(&out, "[REDACTED]").into_owned();
    }
    out
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn is_sensitive(key: &str) -> bool {
    let k = normalize_key(key);
    SENSITIVE_KEYS.contains(&k.as_str()) || SENSITIVE_SUFFIXES.iter().any(|s| k.ends_with(s))
}

fn redacted_text(s: &str) -> Value {
    json!({"$redacted_text": true, "chars": s.chars().count()})
}

fn is_binary(s: &str) -> bool {
    s.len() >= 256
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b))
}

/// Overall nesting budget, shared across JSON documents decoded from
/// strings, so a string that wraps JSON inside JSON cannot recurse without
/// bound. Deeper values are replaced wholesale.
const MAX_SANITIZE_DEPTH: usize = 64;

pub fn sanitize(v: &Value, keep_content: bool) -> Value {
    sanitize_at(v, keep_content, None, true, 0)
}

fn sanitize_at(
    v: &Value,
    keep_content: bool,
    key: Option<&str>,
    protocol: bool,
    depth: usize,
) -> Value {
    let normalized = key.map(normalize_key).unwrap_or_default();
    if key.is_some_and(is_sensitive) {
        return json!("[REDACTED]");
    }
    if depth >= MAX_SANITIZE_DEPTH {
        return json!("[REDACTED_DEPTH]");
    }
    match v {
        Value::Object(_) if !keep_content && !protocol => json!("[REDACTED]"),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| {
                    let structural = protocol
                        && if x.is_object() || x.is_array() {
                            protocol_container(&normalize_key(k), x, key.is_none())
                        } else {
                            STRUCTURAL_KEYS.contains(&normalize_key(k).as_str())
                        };
                    (
                        redact_patterns(k),
                        sanitize_at(x, keep_content, Some(k), structural, depth + 1),
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| sanitize_at(x, keep_content, key, protocol, depth + 1))
                .collect(),
        ),
        Value::String(s) => {
            if is_binary(s) && matches!(normalized.as_str(), "data" | "bytes") {
                return json!("[REDACTED_BINARY]");
            }
            if !keep_content {
                return if protocol && STRUCTURAL_KEYS.contains(&normalized.as_str()) {
                    json!(redact_patterns(s))
                } else {
                    redacted_text(s)
                };
            }
            if let Ok(inner @ (Value::Object(_) | Value::Array(_))) =
                serde_json::from_str::<Value>(s)
            {
                return json!(serde_json::to_string(&sanitize_at(
                    &inner,
                    keep_content,
                    None,
                    false,
                    depth + 1
                ))
                .unwrap_or_default());
            }
            json!(redact_patterns(s))
        }
        other => other.clone(),
    }
}

/// Error strings can quote upstream bodies that echo the prompt, so they get
/// the same content rule as request fields.
fn sanitize_error(error: &str, keep_content: bool) -> Value {
    if error.is_empty() || keep_content {
        json!(redact_patterns(error))
    } else {
        redacted_text(error)
    }
}

#[derive(Default)]
pub struct Capture {
    request: Option<Value>,
    kiro_request: Option<Value>,
    chunks: Vec<(String, Vec<u8>)>,
    bytes: usize,
    truncated: bool,
}

pub fn enabled() -> bool {
    config::get().debug_mode != DebugMode::Off
}

impl Capture {
    pub fn new() -> Option<std::sync::Arc<Mutex<Capture>>> {
        enabled().then(|| std::sync::Arc::new(Mutex::new(Capture::default())))
    }

    fn room(&mut self, n: usize) -> bool {
        if self.bytes + n > config::get().debug_capture_max_bytes as usize {
            self.truncated = true;
            return false;
        }
        self.bytes += n;
        true
    }

    pub fn request(&mut self, v: &Value) {
        self.request = Some(sanitize(v, config::get().debug_capture_content));
    }

    pub fn kiro_request(&mut self, v: &Value) {
        self.kiro_request = Some(sanitize(v, config::get().debug_capture_content));
    }

    pub fn chunk(&mut self, kind: &str, data: &[u8]) {
        if self.room(data.len()) {
            self.chunks.push((kind.to_owned(), data.to_vec()));
        }
    }

    pub fn flush(&self, status: u16, error: &str) {
        let cfg = config::get();
        if cfg.debug_mode == DebugMode::Off
            || (status < 400 && !cfg.debug_capture_success && cfg.debug_mode != DebugMode::All)
        {
            return;
        }
        use base64::Engine;
        let keep = cfg.debug_capture_content;
        let records: Vec<Value> = self
            .chunks
            .iter()
            .map(|(k, d)| {
                let text = String::from_utf8_lossy(d);
                let payload = if keep { redact_patterns(&text).into_bytes() } else { format!("[{} bytes]", d.len()).into_bytes() };
                json!({"kind": k, "payload_base64": base64::engine::general_purpose::STANDARD.encode(payload)})
            })
            .collect();
        let bundle = json!({
            "version": 1, "status": status, "error": sanitize_error(error, keep), "capturedAt": crate::store::now_f64(),
            "request": self.request, "kiroRequest": self.kiro_request, "records": records, "truncated": self.truncated,
        });
        if let Err(e) = write_bundle(&bundle) {
            tracing::warn!("Could not write debug capture: {e}");
        }
    }
}

fn write_bundle(bundle: &Value) -> std::io::Result<()> {
    let cfg = config::get();
    let dir = PathBuf::from(&cfg.debug_dir);
    std::fs::create_dir_all(&dir)?;
    let name = format!(
        "capture-{}-{}.json",
        crate::store::now_i64(),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(bundle)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, dir.join(&name))?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("capture-"))
        })
        .collect();
    files.sort();
    while files.len() > cfg.debug_capture_retention.max(1) as usize {
        let _ = std::fs::remove_file(files.remove(0));
    }
    for stale in std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("tmp"))
    {
        if stale
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|a| a.as_secs() > 3600)
        {
            let _ = std::fs::remove_file(stale);
        }
    }
    Ok(())
}

fn parse_anthropic(text: &str) -> Result<Vec<(String, Value)>, String> {
    let mut out = Vec::new();
    let mut event: Option<String> = None;
    for line in text.lines() {
        if let Some(e) = line.strip_prefix("event:") {
            event = Some(e.trim().to_owned());
        } else if let (Some(d), Some(e)) = (line.strip_prefix("data:"), event.take()) {
            let v = serde_json::from_str(d.trim()).map_err(|e| format!("Invalid SSE JSON: {e}"))?;
            out.push((e, v));
        }
    }
    Ok(out)
}

fn client_text(bundle: &Value) -> Result<String, String> {
    use base64::Engine;
    let records = bundle["records"]
        .as_array()
        .ok_or("Missing capture records")?;
    let mut bytes = Vec::new();
    for r in records.iter().filter(|r| r["kind"] == "client") {
        let encoded = r["payload_base64"]
            .as_str()
            .ok_or("Invalid client payload")?;
        bytes.extend(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| e.to_string())?,
        );
    }
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    if !text.lines().any(|line| line.starts_with("data:")) {
        return Err("Capture has no replayable client data (content may be disabled)".into());
    }
    Ok(text)
}

/// Removes content from a capture while preserving its replay framing and
/// protocol fields. Opaque or malformed client records cannot be exported.
pub fn sanitize_bundle(bundle: &Value) -> Result<Value, String> {
    use base64::Engine;
    // Reassemble transport chunks, including split JSON and UTF-8 characters.
    // Raw upstream bytes are omitted from the content-free replay fixture.
    let text = client_text(bundle)?;
    let mut stream = String::new();
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            stream.push_str("data: ");
            if data.trim() == "[DONE]" {
                stream.push_str("[DONE]");
            } else {
                let v: Value = serde_json::from_str(data.trim())
                    .map_err(|e| format!("Invalid SSE JSON: {e}"))?;
                stream.push_str(
                    &serde_json::to_string(&sanitize(&v, false)).map_err(|e| e.to_string())?,
                );
            }
            stream.push('\n');
        } else if line.starts_with("event:") {
            stream.push_str(&redact_patterns(line));
            stream.push('\n');
        } else if line.is_empty() {
            stream.push('\n');
        }
    }
    let mut out = sanitize(bundle, false);
    for key in ["request", "kiroRequest"] {
        if let Some(v) = bundle.get(key) {
            out[key] = sanitize(v, false);
        }
    }
    out["records"] = json!([{
        "kind": "client",
        "payload_base64": base64::engine::general_purpose::STANDARD.encode(stream),
    }]);
    Ok(out)
}

/// Replays a capture bundle through the stream validators and prints a verdict.
pub fn validate_bundle(path: &Path) -> Result<String, String> {
    let bundle: Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let records = bundle["records"].as_array().cloned().unwrap_or_default();
    let text = client_text(&bundle)?;
    if text.lines().any(|line| line.starts_with("event:")) {
        let mut v = AnthropicValidator::new();
        for (e, d) in parse_anthropic(&text)? {
            v.accept(&e, &d).map_err(|e| e.to_string())?;
        }
        return Ok(format!(
            "anthropic stream valid ({} records)",
            records.len()
        ));
    }
    let mut v = OpenAIValidator::default();
    for line in text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
    {
        if line == "[DONE]" {
            v.accept(None, true).map_err(|e| e.to_string())?;
        } else {
            let p = serde_json::from_str::<Value>(line)
                .map_err(|e| format!("Invalid SSE JSON: {e}"))?;
            v.accept(Some(&p), false).map_err(|e| e.to_string())?;
        }
    }
    Ok(format!("openai stream valid ({} records)", records.len()))
}

pub fn replay_cli(args: &[String]) -> i32 {
    let Some(target) = args.first() else {
        eprintln!("usage: kirolb replay <capture.json | capture-dir> [--export out.json]");
        return 2;
    };
    let path = PathBuf::from(target);
    let files: Vec<PathBuf> = if path.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        v.sort();
        v
    } else {
        vec![path]
    };
    let mut failed = 0;
    for f in &files {
        match validate_bundle(f) {
            Ok(msg) => println!("{}: {msg}", f.display()),
            Err(e) => {
                failed += 1;
                println!("{}: INVALID - {e}", f.display());
            }
        }
    }
    if let Some(i) = args.iter().position(|a| a == "--export") {
        if let (Some(out), Some(first)) = (args.get(i + 1), files.first()) {
            let export = || -> Result<(), String> {
                let bytes = std::fs::read(first).map_err(|e| e.to_string())?;
                let bundle = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                let sanitized = sanitize_bundle(&bundle)?;
                let bytes = serde_json::to_vec_pretty(&sanitized).map_err(|e| e.to_string())?;
                std::fs::write(out, bytes).map_err(|e| e.to_string())
            };
            match export() {
                Ok(()) => println!("exported sanitized fixture to {out}"),
                Err(e) => {
                    failed += 1;
                    eprintln!("Could not export capture: {e}");
                }
            }
        }
    }
    i32::from(failed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_secrets_and_content() {
        let v = json!({"headers": {"Authorization": "Bearer abc"}, "accessToken": "t", "messages": [{"content": "hello"}], "note": "key klb_ABCDEFGHIJKLMNOP"});
        let s = sanitize(&v, false);
        assert_eq!(s["headers"], "[REDACTED]");
        assert_eq!(s["accessToken"], "[REDACTED]");
        assert_eq!(
            s["messages"][0]["content"],
            json!({"$redacted_text": true, "chars": 5})
        );
        assert_eq!(s["note"], json!({"$redacted_text": true, "chars": 24}));
        let kept = sanitize(&v, true);
        assert_eq!(kept["headers"]["Authorization"], "[REDACTED]");
        assert_eq!(kept["note"], "key [REDACTED]");
        assert_eq!(kept["accessToken"], "[REDACTED]");
    }

    fn sentinels(v: &Value) -> Vec<String> {
        let text = serde_json::to_string(v).unwrap();
        [
            "PRIVATE_A",
            "PRIVATE_B",
            "PRIVATE_C",
            "PRIVATE_D",
            "PRIVATE_E",
        ]
        .into_iter()
        .filter(|s| text.contains(s))
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn content_off_redacts_every_non_structural_string() {
        let shapes = [
            json!({"model": "claude-sonnet-4.5", "input": "PRIVATE_A", "instructions": "PRIVATE_B", "metadata": {"note": "PRIVATE_C", "tags": ["PRIVATE_D"]}}),
            json!({"model": "m", "messages": [{"role": "user", "content": [{"type": "text", "text": "PRIVATE_A"}]}], "user": "PRIVATE_B", "tools": [{"type": "function", "function": {"name": "f", "description": "PRIVATE_C", "parameters": {"properties": {"q": {"description": "PRIVATE_D"}}}}}]}),
            json!({"model": "m", "system": [{"type": "text", "text": "PRIVATE_A"}], "messages": [{"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "PRIVATE_B"}]}], "metadata": {"user_id": "PRIVATE_C"}}),
            json!({"conversationState": {"currentMessage": {"userInputMessage": {"content": "PRIVATE_A", "modelId": "claude-sonnet-4.5", "origin": "AI_EDITOR", "userInputMessageContext": {"tools": [{"toolSpecification": {"name": "f", "description": "PRIVATE_B"}}], "toolResults": [{"toolUseId": "t1", "content": [{"text": "PRIVATE_C"}], "status": "success"}]}}}, "history": [{"assistantResponseMessage": {"content": "PRIVATE_D", "toolUses": [{"toolUseId": "t1", "name": "f", "input": {"q": "PRIVATE_E"}}]}}]}}),
        ];
        for shape in &shapes {
            let s = sanitize(shape, false);
            assert!(
                sentinels(&s).is_empty(),
                "leaked {:?} from {shape}",
                sentinels(&s)
            );
        }
        assert_eq!(sanitize(&shapes[0], false)["model"], "claude-sonnet-4.5");
        let kiro = sanitize(&shapes[3], false);
        let msg = &kiro["conversationState"]["currentMessage"]["userInputMessage"];
        assert_eq!(msg["modelId"], "claude-sonnet-4.5");
        assert_eq!(
            msg["userInputMessageContext"]["toolResults"][0]["status"],
            "success"
        );
        assert_eq!(
            kiro["conversationState"]["history"][0]["assistantResponseMessage"]["toolUses"][0]
                ["name"],
            "f"
        );
    }

    #[test]
    fn content_on_keeps_text_but_still_redacts_secrets() {
        let v = json!({"input": "PRIVATE_A", "apiKey": "PRIVATE_B", "note": "Bearer abcdef123"});
        let s = sanitize(&v, true);
        assert_eq!(s["input"], "PRIVATE_A");
        assert_eq!(s["apiKey"], "[REDACTED]");
        assert_eq!(s["note"], "[REDACTED]");
    }

    #[test]
    fn json_encoded_strings_follow_the_content_policy() {
        let v = json!({"arguments": "{\"query\":\"PRIVATE_A\",\"token\":\"x\",\"klb_ABCDEFGHIJKLMNOP\":\"value\"}"});
        let s = sanitize(&v, false);
        assert!(sentinels(&s).is_empty());
        assert_eq!(s["arguments"]["$redacted_text"], true);
        let s = sanitize(&v, true);
        assert!(!s.to_string().contains("klb_ABCDEFGHIJKLMNOP"));
        let inner: Value = serde_json::from_str(s["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(inner["query"], "PRIVATE_A");
        assert_eq!(inner["token"], "[REDACTED]");
    }

    #[test]
    fn structural_names_in_prompts_and_tool_inputs_are_not_protocol_fields() {
        let v = json!({
            "model": "m", "metadata": {"name": "PRIVATE_A", "PRIVATE_B": "value"},
            "input": [{"role": "user", "content": "{\"name\":\"PRIVATE_C\",\"PRIVATE_D\":42}"}],
            "messages": [{"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "lookup", "input": {"name": "PRIVATE_E", "id": "PRIVATE_A"}}]}]
        });
        let s = sanitize(&v, false);
        assert!(sentinels(&s).is_empty(), "{s}");
        assert_eq!(s["model"], "m");
        assert_eq!(s["input"][0]["role"], "user");
        assert_eq!(s["messages"][0]["content"][0]["name"], "lookup");
        assert_eq!(s["messages"][0]["content"][0]["id"], "t1");
    }

    #[test]
    fn embedded_json_recursion_is_bounded_with_content_on_and_off() {
        let mut text = "hello".to_owned();
        for _ in 0..12 {
            text = format!(
                "{}{}{}",
                "[".repeat(100),
                serde_json::to_string(&text).unwrap(),
                "]".repeat(100)
            );
        }
        let request = json!({"messages": [{"role": "user", "content": text}]});
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                assert_eq!(
                    sanitize(&request, false)["messages"][0]["content"]["$redacted_text"],
                    true
                );
                assert!(sanitize(&request, true)
                    .to_string()
                    .contains("REDACTED_DEPTH"));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn query_redaction_keeps_serialized_frames_parseable() {
        for (url, expected) in [
            (
                "https://example.test/?token=PRIVATE_A",
                "https://example.test/[REDACTED]",
            ),
            (
                "https://example.test/?token=PRIVATE_A&next=ok",
                "https://example.test/[REDACTED]&next=ok",
            ),
        ] {
            let frame = json!({"type": "content_block_start", "index": 0, "content_block": {
                "type": "tool_use", "id": "t1", "name": "fetch", "input": {"url": url}
            }});
            let text = redact_patterns(&frame.to_string());
            assert!(!text.contains("PRIVATE_A"));
            let parsed: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(parsed["content_block"]["name"], "fetch");
            assert_eq!(parsed["content_block"]["input"]["url"], expected);
        }
    }

    #[test]
    fn exported_streams_keep_their_verdict_without_private_content() {
        use base64::Engine;
        let root = std::env::temp_dir().join(format!("kirolb-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let input = root.join("input.json");
        let output = root.join("output.json");
        let streams = [
            ("data: {\"choices\":[{\"delta\":{\"content\":\"PRIVATE_A한글 event: error\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n", true),
            ("data: [DONE]\n\n", false),
            ("event: message_start\ndata: {\"type\":\"message_start\"}\n\nevent: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"PRIVATE_B\"}}\n\nevent: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"PRIVATE_C\"}}\n\nevent: content_block_stop\ndata: {\"index\":0}\n\nevent: message_delta\ndata: {}\n\nevent: message_stop\ndata: {}\n\n", true),
            ("event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"PRIVATE_D\"}}\n\n", false),
        ];
        for (stream, valid) in streams {
            // Single-byte chunks also split multibyte UTF-8 and JSON tokens.
            let records: Vec<Value> = stream.as_bytes().chunks(1).map(|b| json!({
                "kind": "client", "payload_base64": base64::engine::general_purpose::STANDARD.encode(b)
            })).collect();
            let bundle = json!({"version": 1, "request": {"model": "m", "input": "PRIVATE_E"}, "records": records});
            std::fs::write(&input, bundle.to_string()).unwrap();
            assert_eq!(validate_bundle(&input).is_ok(), valid);
            assert_eq!(
                replay_cli(&[
                    input.display().to_string(),
                    "--export".into(),
                    output.display().to_string()
                ]),
                i32::from(!valid)
            );
            assert_eq!(validate_bundle(&output).is_ok(), valid);
            let exported: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
            assert!(sentinels(&exported).is_empty());
            assert!(!client_text(&exported).unwrap().contains("PRIVATE_"));
            assert_eq!(exported["request"]["model"], "m");
        }
        let missing =
            json!({"records": [{"kind": "client", "payload_base64": {"$redacted_text": true}}]});
        assert!(sanitize_bundle(&missing).is_err());
        std::fs::write(&input, missing.to_string()).unwrap();
        assert!(validate_bundle(&input).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn error_text_follows_the_content_rule() {
        assert_eq!(
            sanitize_error("upstream said PRIVATE_A", false),
            json!({"$redacted_text": true, "chars": 23})
        );
        assert_eq!(
            sanitize_error("Bearer abcdef123 failed", true),
            json!("[REDACTED] failed")
        );
        assert_eq!(sanitize_error("", false), json!(""));
    }
}
