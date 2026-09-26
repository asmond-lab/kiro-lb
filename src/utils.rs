use sha2::{Digest, Sha256};
use std::sync::OnceLock;

pub const IDE_BUILD: &str =
    "KiroIDE-1.0.437-ea11196bc54380ef285f87b7040026830a864d2a50bb872ea19a5bbbe732b407-KAS/0.54.0";
pub const IDE_EXEC_ENV: &str = "exec-env/AmazonQ-For-CLI-Version/2.21.1-acp-client/kiro-tui";

pub fn ide_short_user_agent() -> String {
    format!("aws-sdk-js/1.0.0 {IDE_BUILD}")
}

pub fn ide_user_agent(api_label: &str) -> String {
    format!(
        "aws-sdk-js/1.0.0 ua/2.1 os/win32#10.0.26200 lang/js md/nodejs#22.22.0 api/{api_label}#1.0.0 {IDE_EXEC_ENV} m/N {IDE_BUILD}"
    )
}

pub fn machine_fingerprint() -> &'static str {
    static F: OnceLock<String> = OnceLock::new();
    F.get_or_init(|| {
        let host = hostname::get()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_default();
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        if host.is_empty() && user.is_empty() {
            return hex::encode(Sha256::digest(b"default-kiro-lb"));
        }
        hex::encode(Sha256::digest(format!("{host}-{user}-kiro-lb").as_bytes()))
    })
}

pub fn kiro_headers(token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Authorization", format!("Bearer {token}")),
        ("Content-Type", "application/x-amz-json-1.0".into()),
        (
            "x-amz-target",
            "KiroRuntimeService.GenerateAssistantResponse".into(),
        ),
        ("x-amzn-kiro-client-attribution", "kiro-ide".into()),
        ("User-Agent", ide_user_agent("kiroruntime")),
        ("x-amz-user-agent", ide_short_user_agent()),
        ("x-amzn-codewhisperer-optout", "true".into()),
        ("x-kiro-attempt", "1;max=3".into()),
        ("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string()),
        ("amz-sdk-request", "attempt=1; max=3".into()),
    ]
}

pub fn completion_id() -> String {
    format!("chatcmpl-{}", uuid::Uuid::new_v4().simple())
}

pub fn conversation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn tool_call_id() -> String {
    format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

pub fn message_id() -> String {
    format!("msg_{}", &uuid::Uuid::new_v4().simple().to_string()[..24])
}
