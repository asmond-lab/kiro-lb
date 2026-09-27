//! Device-authorization login: social (Google/GitHub) on the Kiro desktop auth host,
//! and Builder ID over AWS SSO OIDC. The two contracts are near-inverses, so the
//! polling logic is deliberately not shared.

use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

use crate::{config, store::now_f64};

const AUTH_HOST: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";
const CLIENT_ID: &str = "kiro-cli";
const SOCIAL_REGION: &str = "us-east-1";
const BUILDER_ID_START_URL: &str = "https://view.awsapps.com/start";
const BUILDER_ID_REGION: &str = "us-east-1";
const BUILDER_ID_SCOPES: [&str; 3] = [
    "codewhisperer:completions",
    "codewhisperer:analysis",
    "codewhisperer:conversations",
];
const FLOW_GRACE: f64 = 60.0;

#[derive(Clone)]
pub struct DeviceFlow {
    pub id: String,
    pub provider: &'static str,
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_at: f64,
    interval: f64,
    pub status: String,
    detail: Option<String>,
    pub token: Option<Value>,
    registration: Option<Value>,
}

impl DeviceFlow {
    pub fn view(&self) -> Value {
        json!({
            "flowId": self.id, "provider": self.provider, "status": self.status, "detail": self.detail,
            "userCode": self.user_code, "verificationUri": self.verification_uri,
            "verificationUriComplete": self.verification_uri_complete,
            "expiresInSeconds": (self.expires_at - now_f64()).max(0.0) as i64,
        })
    }
}

static FLOWS: Mutex<Option<HashMap<String, DeviceFlow>>> = Mutex::new(None);

pub fn resolve_provider(raw: &str) -> Result<&'static str, String> {
    match raw.trim().to_lowercase().as_str() {
        "google" => Ok("Google"),
        "github" => Ok("Github"),
        "builder-id" | "builderid" | "builder_id" => Ok("BuilderId"),
        _ => Err("provider must be google or github".into()),
    }
}

fn flow_id() -> String {
    use base64::Engine;
    use rand::RngCore;
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

async fn social_post(http: &reqwest::Client, path: &str, body: Value) -> Result<Value, String> {
    let resp = http
        .post(format!("{AUTH_HOST}{path}"))
        .json(&body)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if status >= 400 {
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or(text);
        return Err(format!("HTTP {status}: {msg}"));
    }
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

async fn oidc_call(
    http: &reqwest::Client,
    region: &str,
    path: &str,
    body: Value,
) -> Result<Value, String> {
    let region = config::validate_region(region).map_err(|e| e.to_string())?;
    let resp = http
        .post(format!("https://oidc.{region}.amazonaws.com{path}"))
        .json(&body)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let errtype = resp
        .headers()
        .get("x-amzn-errortype")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_owned();
    let text = resp.text().await.unwrap_or_default();
    let payload: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    if status >= 400 {
        let code = payload
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
            .unwrap_or(if errtype.is_empty() {
                format!("HTTP_{status}")
            } else {
                errtype
            });
        let msg = payload
            .get("error_description")
            .or_else(|| payload.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or(text);
        return Err(format!("{code}: {msg}"));
    }
    Ok(payload)
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64).filter(|n| *n != 0.0)
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .map(|x| match x {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            o => o.to_string(),
        })
        .unwrap_or_default()
}

pub async fn start(http: &reqwest::Client, provider: &'static str) -> Result<Value, String> {
    let flow = if provider == "BuilderId" {
        let reg = oidc_call(
            http,
            BUILDER_ID_REGION,
            "/client/register",
            json!({"clientName": "kiro-cli", "clientType": "public", "scopes": BUILDER_ID_SCOPES}),
        )
        .await?;
        let auth = oidc_call(http, BUILDER_ID_REGION, "/device_authorization", json!({"clientId": reg["clientId"], "clientSecret": reg["clientSecret"], "startUrl": BUILDER_ID_START_URL})).await?;
        DeviceFlow {
            id: flow_id(),
            provider,
            device_code: s(&auth, "deviceCode"),
            user_code: s(&auth, "userCode"),
            verification_uri: s(&auth, "verificationUri"),
            verification_uri_complete: s(&auth, "verificationUriComplete"),
            expires_at: now_f64() + num(&auth, "expiresIn").unwrap_or(600.0),
            interval: num(&auth, "interval").unwrap_or(5.0).max(1.0),
            status: "pending".into(),
            detail: None,
            token: None,
            registration: Some(
                json!({"clientId": reg["clientId"], "clientSecret": reg["clientSecret"], "region": BUILDER_ID_REGION}),
            ),
        }
    } else {
        let p = social_post(
            http,
            "/oauth/device/authorization",
            json!({"clientId": CLIENT_ID, "loginProvider": provider}),
        )
        .await?;
        DeviceFlow {
            id: flow_id(),
            provider,
            device_code: s(&p, "deviceCode"),
            user_code: s(&p, "userCode"),
            verification_uri: s(&p, "verificationUri"),
            verification_uri_complete: s(&p, "verificationUriComplete"),
            expires_at: now_f64() + num(&p, "expiresInMilliseconds").unwrap_or(300_000.0) / 1000.0,
            interval: (num(&p, "intervalInMilliseconds").unwrap_or(5000.0) / 1000.0).max(1.0),
            status: "pending".into(),
            detail: None,
            token: None,
            registration: None,
        }
    };
    let view = flow.view();
    let mut guard = FLOWS.lock();
    let flows = guard.get_or_insert_with(HashMap::new);
    let cutoff = now_f64() - FLOW_GRACE;
    flows.retain(|_, f| f.expires_at >= cutoff);
    tracing::info!("Started {provider} device login {}", flow.id);
    flows.insert(flow.id.clone(), flow);
    Ok(view)
}

fn get(id: &str) -> Option<DeviceFlow> {
    FLOWS.lock().as_ref().and_then(|f| f.get(id).cloned())
}

/// Publishes a poll result only while the flow is still registered: a poll
/// that was in flight when the operator cancelled must not bring it back.
fn put(flow: DeviceFlow) {
    if let Some(f) = FLOWS.lock().as_mut() {
        if let Some(slot) = f.get_mut(&flow.id) {
            *slot = flow;
        }
    }
}

pub fn discard(id: &str) {
    if let Some(f) = FLOWS.lock().as_mut() {
        f.remove(id);
    }
}

pub async fn poll(http: &reqwest::Client, id: &str) -> Result<DeviceFlow, String> {
    let mut flow = get(id).ok_or("Unknown or expired login flow")?;
    if matches!(flow.status.as_str(), "approved" | "failed" | "expired") {
        return Ok(flow);
    }
    if now_f64() > flow.expires_at {
        flow.status = "expired".into();
        flow.detail = Some("The approval window closed before the login was confirmed".into());
        put(flow.clone());
        return Ok(flow);
    }
    if flow.provider == "BuilderId" {
        let reg = flow.registration.clone().unwrap_or(json!({}));
        let region = reg
            .get("region")
            .and_then(Value::as_str)
            .unwrap_or(BUILDER_ID_REGION)
            .to_owned();
        match oidc_call(http, &region, "/token", json!({"clientId": reg["clientId"], "clientSecret": reg["clientSecret"], "deviceCode": flow.device_code, "grantType": "urn:ietf:params:oauth:grant-type:device_code"})).await {
            Err(reason) => {
                if reason.contains("authorization_pending") || reason.contains("AuthorizationPending") {
                } else if reason.contains("slow_down") || reason.contains("SlowDown") {
                    flow.interval += 5.0;
                } else {
                    flow.status = if reason.contains("expired_token") || reason.contains("ExpiredToken") { "expired" } else { "failed" }.into();
                    flow.detail = Some(reason);
                }
            }
            Ok(p) => match p.get("accessToken").filter(|t| !t.is_null()) {
                Some(t) => {
                    flow.token = Some(json!({"accessToken": t, "refreshToken": p.get("refreshToken"), "expiresIn": p.get("expiresIn"), "profileArn": null}));
                    flow.status = "approved".into();
                    flow.detail = None;
                }
                None => {
                    flow.status = "failed".into();
                    flow.detail = Some("Builder ID returned no access token".into());
                }
            },
        }
    } else {
        match social_post(
            http,
            "/oauth/device/poll",
            json!({"clientId": CLIENT_ID, "deviceCode": flow.device_code}),
        )
        .await
        {
            Err(e) => {
                flow.status = "failed".into();
                flow.detail = Some(e);
            }
            Ok(p) => {
                if let Some(t) = p
                    .get("accessToken")
                    .filter(|t| t.as_str().is_some_and(|s| !s.is_empty()))
                {
                    flow.token = Some(
                        json!({"accessToken": t, "refreshToken": p.get("refreshToken"), "profileArn": p.get("profileArn"), "identityProvider": p.get("identityProvider"), "expiresIn": p.get("expiresIn")}),
                    );
                    flow.status = "approved".into();
                    flow.detail = None;
                } else {
                    let upstream = p
                        .get("status")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("authorization_pending")
                        .to_owned();
                    if upstream != "authorization_pending" {
                        flow.status = if upstream.contains("expired") {
                            "expired"
                        } else {
                            "failed"
                        }
                        .into();
                        flow.detail = Some(format!("Device authorization {upstream}"));
                    }
                }
            }
        }
    }
    put(flow.clone());
    Ok(flow)
}

pub fn internal_credentials(flow: &DeviceFlow) -> Result<Value, String> {
    let token = flow
        .token
        .as_ref()
        .ok_or_else(|| format!("{} login is not approved", flow.provider))?;
    let expires_in = token
        .get("expiresIn")
        .and_then(Value::as_f64)
        .filter(|v| *v != 0.0)
        .unwrap_or(3600.0);
    let expires =
        crate::auth::iso_from_epoch((now_f64() + expires_in).floor()).replace("+00:00", "Z");
    let mut doc = json!({"refreshToken": token.get("refreshToken"), "accessToken": token.get("accessToken"), "expiresAt": expires, "region": SOCIAL_REGION});
    if flow.provider == "BuilderId" {
        let reg = flow
            .registration
            .as_ref()
            .ok_or("Builder ID login has no client registration")?;
        doc["region"] = reg["region"].clone();
        doc["clientId"] = reg["clientId"].clone();
        doc["clientSecret"] = reg["clientSecret"].clone();
        doc["startUrl"] = json!(BUILDER_ID_START_URL);
    } else {
        for k in ["profileArn", "identityProvider"] {
            if let Some(v) = token
                .get(k)
                .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            {
                doc[k] = v.clone();
            }
        }
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use std::net::TcpListener;

    fn flow(id: &str) -> DeviceFlow {
        DeviceFlow {
            id: id.into(),
            provider: "Google",
            device_code: "dc".into(),
            user_code: "uc".into(),
            verification_uri: String::new(),
            verification_uri_complete: String::new(),
            expires_at: now_f64() + 600.0,
            interval: 5.0,
            status: "pending".into(),
            detail: None,
            token: None,
            registration: None,
        }
    }

    #[tokio::test]
    async fn invalid_registration_region_is_rejected_without_an_outbound_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let proxy =
            reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let http = reqwest::Client::builder().proxy(proxy).build().unwrap();

        let error = oidc_call(
            &http,
            "us-east-1@localhost",
            "/token",
            json!({"refreshToken": "unused"}),
        )
        .await
        .unwrap_err();

        assert!(error.starts_with("invalid region:"));
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn a_poll_that_completes_after_cancel_does_not_resurrect_the_flow() {
        let id = "cancelled-mid-poll";
        FLOWS
            .lock()
            .get_or_insert_with(HashMap::new)
            .insert(id.into(), flow(id));
        let mut in_flight = get(id).unwrap();
        discard(id);
        in_flight.status = "approved".into();
        in_flight.token = Some(json!({"refreshToken": "rt"}));
        put(in_flight);
        assert!(get(id).is_none());
    }

    #[test]
    fn a_poll_result_updates_a_flow_that_is_still_registered() {
        let id = "still-registered";
        FLOWS
            .lock()
            .get_or_insert_with(HashMap::new)
            .insert(id.into(), flow(id));
        let mut in_flight = get(id).unwrap();
        in_flight.status = "approved".into();
        put(in_flight);
        assert_eq!(get(id).unwrap().status, "approved");
        discard(id);
    }
}
