use sha2::{Digest, Sha256};

pub const IDE_VERSION: &str = "1.1.70";
pub const KAS_VERSION: &str = "0.66.7";
const IDE_PLATFORM: &str = "ua/2.1 os/win32#10.0.26200 lang/js md/nodejs#24.18.0";
pub const RUNTIME_API: &str = "kiroruntime";
pub const CONTROL_PLANE_API: &str = "kirocontrolplanebearer";
pub const CODEWHISPERER_API: &str = "codewhispererruntime";

pub fn account_machine_id(account_key: &str) -> String {
    hex::encode(Sha256::digest(
        format!("kiro-lb-machine-id\0{account_key}").as_bytes(),
    ))
}

fn ide_build(api_label: &str, machine_id: &str) -> String {
    if api_label == CODEWHISPERER_API {
        format!("KiroIDE-{IDE_VERSION}-{machine_id}")
    } else {
        format!("KiroIDE-{IDE_VERSION}-{machine_id}-KAS/{KAS_VERSION}")
    }
}

pub fn ide_short_user_agent(api_label: &str, machine_id: &str) -> String {
    format!("aws-sdk-js/1.0.0 {}", ide_build(api_label, machine_id))
}

pub fn ide_user_agent(api_label: &str, machine_id: &str) -> String {
    let features = if api_label == RUNTIME_API {
        "m/N"
    } else {
        "m/N,E"
    };
    format!(
        "aws-sdk-js/1.0.0 {IDE_PLATFORM} api/{api_label}#1.0.0 {features} {}",
        ide_build(api_label, machine_id)
    )
}

pub fn refresh_user_agent(machine_id: &str) -> String {
    format!("KiroIDE-{IDE_VERSION}-{machine_id}")
}

pub fn kiro_headers(token: &str, machine_id: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Authorization", format!("Bearer {token}")),
        ("Content-Type", "application/x-amz-json-1.0".into()),
        (
            "x-amz-target",
            "KiroRuntimeService.GenerateAssistantResponse".into(),
        ),
        ("x-amzn-kiro-client-attribution", "kiro-ide".into()),
        ("User-Agent", ide_user_agent(RUNTIME_API, machine_id)),
        (
            "x-amz-user-agent",
            ide_short_user_agent(RUNTIME_API, machine_id),
        ),
        ("x-amzn-codewhisperer-optout", "true".into()),
        ("x-kiro-attempt", "1;max=3".into()),
        ("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string()),
        ("amz-sdk-request", "attempt=1; max=3".into()),
    ]
}

pub fn management_headers(
    token: &str,
    target: Option<&str>,
    api_label: &str,
    machine_id: &str,
) -> Vec<(&'static str, String)> {
    let mut out = vec![("Authorization", format!("Bearer {token}"))];
    if let Some(t) = target {
        out.push(("Content-Type", "application/x-amz-json-1.0".into()));
        out.push(("x-amz-target", t.to_owned()));
    }
    out.push(("User-Agent", ide_user_agent(api_label, machine_id)));
    out.push((
        "x-amz-user-agent",
        ide_short_user_agent(api_label, machine_id),
    ));
    out.push(("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string()));
    out.push(("amz-sdk-request", "attempt=1; max=3".into()));
    out
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
