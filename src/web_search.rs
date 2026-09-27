//! web_search through the Kiro MCP API. Path A answers a native Anthropic
//! server-side search directly; Path B intercepts a model's web_search tool call.

use rand::distributions::Alphanumeric;
use rand::Rng;
use serde_json::{json, Value};

use crate::auth::KiroAuth;
use crate::upstream::http::Transport;

fn random_id(n: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(n)
        .map(char::from)
        .collect()
}

pub async fn call_mcp(
    query: &str,
    auth: &KiroAuth,
    transport: &Transport,
) -> Option<(String, Value)> {
    let request_id = format!(
        "web_search_tooluse_{}_{}_{}",
        random_id(22),
        (crate::store::now_f64() * 1000.0) as i64,
        random_id(8)
    );
    let tool_use_id = format!(
        "srvtoolu_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..32]
    );
    let body = json!({"id": request_id, "jsonrpc": "2.0", "method": "tools/call", "params": {"name": "web_search", "arguments": {"query": query}}});
    let token = auth.access_token().await.ok()?;
    let mut req = transport
        .shared
        .post(format!("{}/mcp", auth.q_host))
        .timeout(std::time::Duration::from_secs(60))
        .header("Authorization", format!("Bearer {token}"))
        .header("x-amzn-codewhisperer-optout", "false")
        .json(&body);
    if let Some(arn) = auth.request_profile_arn() {
        req = req.header("x-amzn-kiro-profile-arn", arn);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("MCP API request error: {e}");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::error!("MCP API error: {}", resp.status());
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    if v.get("error").is_some_and(|e| !e.is_null()) {
        tracing::error!("MCP API returned error");
        return None;
    }
    let text = v
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let results: Value = serde_json::from_str(text).ok()?;
    Some((tool_use_id, results))
}

fn format_date(ms: f64) -> Option<String> {
    let secs = (ms / 1000.0) as i64;
    let iso = crate::auth::iso_from_epoch(secs as f64);
    let (date, time) = iso.split_once('T')?;
    let mut d = date.split('-');
    let (y, m, day) = (d.next()?, d.next()?.parse::<usize>().ok()?, d.next()?);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    Some(format!(
        "{day} {} {y} {}",
        MONTHS.get(m.checked_sub(1)?)?,
        time.get(..8)?
    ))
}

pub fn summary(query: &str, results: &Value) -> String {
    let mut s = format!("\n<web_search>\nSearch results for \"{query}\":\n\n");
    match results.get("results").and_then(Value::as_array) {
        Some(items) => {
            for (i, r) in items.iter().enumerate() {
                let get = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("");
                s.push_str(&format!(
                    "{}. Title: **{}**\n",
                    i + 1,
                    r.get("title").and_then(Value::as_str).unwrap_or("Untitled")
                ));
                if let Some(d) = r
                    .get("publishedDate")
                    .and_then(Value::as_f64)
                    .filter(|d| *d != 0.0)
                    .and_then(format_date)
                {
                    s.push_str(&format!("   Published: {d}\n"));
                }
                if !get("url").is_empty() {
                    s.push_str(&format!("   URL: {}\n", get("url")));
                }
                if !get("snippet").is_empty() {
                    s.push_str(&format!("   {}\n", get("snippet")));
                }
                s.push('\n');
            }
        }
        None => s.push_str("No results found.\n"),
    }
    s.push_str("</web_search>\n");
    s
}

pub fn search_content(results: &Value) -> Vec<Value> {
    results
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|r| {
            json!({
                "type": "web_search_result",
                "title": r.get("title").cloned().unwrap_or(json!("")),
                "url": r.get("url").cloned().unwrap_or(json!("")),
                "encrypted_content": r.get("snippet").cloned().unwrap_or(json!("")),
                "page_age": null,
            })
        })
        .collect()
}

pub fn chunks(text: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(size).map(|c| c.iter().collect()).collect()
}

pub fn extract_query(messages: &[Value]) -> Option<String> {
    let content = messages.first()?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect(),
        _ => return None,
    };
    let q = text
        .strip_prefix("Perform a web search for the query: ")
        .unwrap_or(&text)
        .trim()
        .to_owned();
    (!q.is_empty()).then_some(q)
}
