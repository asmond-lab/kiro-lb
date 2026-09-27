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
    "accesstoken",
    "refreshtoken",
    "access_token",
    "refresh_token",
    "clientsecret",
    "client_secret",
    "password",
    "x-api-key",
    "api_key",
    "apikey",
    "signature",
    "cookie",
    "set-cookie",
    "profilearn",
];

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            r"(?i)bearer\s+[A-Za-z0-9._\-~+/=]+",
            r"klb_[A-Za-z0-9_\-]{8,}",
            r"aoa[A-Za-z0-9_\-]{20,}",
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

fn is_sensitive(key: &str) -> bool {
    let k = key.to_lowercase().replace(['-', '_'], "");
    SENSITIVE_KEYS
        .iter()
        .any(|s| s.replace(['-', '_'], "") == k)
}

pub fn sanitize(v: &Value, keep_content: bool) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| {
                    let val = if is_sensitive(k) {
                        json!("[REDACTED]")
                    } else if !keep_content
                        && matches!(k.as_str(), "content" | "text" | "thinking")
                        && x.is_string()
                    {
                        json!(format!("[{} chars]", x.as_str().unwrap().chars().count()))
                    } else {
                        sanitize(x, keep_content)
                    };
                    (k.clone(), val)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|x| sanitize(x, keep_content)).collect()),
        Value::String(s) => json!(redact_patterns(s)),
        other => other.clone(),
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

    /// True when the bytes sent to the client carry a protocol error event,
    /// so a stream that failed after its 200 headers is still captured.
    pub fn client_saw_error(&self) -> bool {
        self.chunks.iter().any(|(k, d)| {
            k == "client" && {
                let t = String::from_utf8_lossy(d);
                t.contains("event: error")
                    || t.contains("response.failed")
                    || t.contains("\"error\":{")
            }
        })
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
            "version": 1, "status": status, "error": redact_patterns(error), "capturedAt": crate::store::now_f64(),
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

fn parse_anthropic(text: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let mut event: Option<String> = None;
    for line in text.lines() {
        if let Some(e) = line.strip_prefix("event:") {
            event = Some(e.trim().to_owned());
        } else if let (Some(d), Some(e)) = (line.strip_prefix("data:"), event.take()) {
            if let Ok(v) = serde_json::from_str(d.trim()) {
                out.push((e, v));
            }
        }
    }
    out
}

/// Replays a capture bundle through the stream validators and prints a verdict.
pub fn validate_bundle(path: &Path) -> Result<String, String> {
    use base64::Engine;
    let bundle: Value = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let records = bundle["records"].as_array().cloned().unwrap_or_default();
    let mut text = String::new();
    for r in records.iter().filter(|r| r["kind"] == "client") {
        let payload = base64::engine::general_purpose::STANDARD
            .decode(r["payload_base64"].as_str().unwrap_or(""))
            .unwrap_or_default();
        text.push_str(&String::from_utf8_lossy(&payload));
    }
    if text.contains("event:") {
        let mut v = AnthropicValidator::new();
        for (e, d) in parse_anthropic(&text) {
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
        } else if let Ok(p) = serde_json::from_str::<Value>(line) {
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
            if let Ok(b) = std::fs::read(first) {
                let v: Value = serde_json::from_slice(&b).unwrap_or(Value::Null);
                let _ = std::fs::write(
                    out,
                    serde_json::to_vec_pretty(&sanitize(&v, false)).unwrap_or_default(),
                );
                println!("exported sanitized fixture to {out}");
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
        assert_eq!(s["headers"]["Authorization"], "[REDACTED]");
        assert_eq!(s["accessToken"], "[REDACTED]");
        assert_eq!(s["messages"][0]["content"], "[5 chars]");
        assert_eq!(s["note"], "key [REDACTED]");
    }
}
