use kiro_lb::utils::{
    account_machine_id, ide_short_user_agent, ide_user_agent, kiro_headers, management_headers,
    refresh_user_agent, CODEWHISPERER_API, CONTROL_PLANE_API, RUNTIME_API,
};
use serde_json::json;

const ID: &str = "ae519a9ef0cd28742105b7c4d9a2078ea665eef3dcd2585d6e3132855df31e6c";

fn header<'a>(headers: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[test]
fn user_agents_match_the_captured_kiro_ide() {
    assert_eq!(
        ide_user_agent(RUNTIME_API, ID),
        format!("aws-sdk-js/1.0.0 ua/2.1 os/win32#10.0.26200 lang/js md/nodejs#24.18.0 api/kiroruntime#1.0.0 m/N KiroIDE-1.1.70-{ID}-KAS/0.66.7")
    );
    assert_eq!(
        ide_short_user_agent(RUNTIME_API, ID),
        format!("aws-sdk-js/1.0.0 KiroIDE-1.1.70-{ID}-KAS/0.66.7")
    );
    assert_eq!(
        ide_user_agent(CONTROL_PLANE_API, ID),
        format!("aws-sdk-js/1.0.0 ua/2.1 os/win32#10.0.26200 lang/js md/nodejs#24.18.0 api/kirocontrolplanebearer#1.0.0 m/N,E KiroIDE-1.1.70-{ID}-KAS/0.66.7")
    );
    assert_eq!(
        ide_user_agent(CODEWHISPERER_API, ID),
        format!("aws-sdk-js/1.0.0 ua/2.1 os/win32#10.0.26200 lang/js md/nodejs#24.18.0 api/codewhispererruntime#1.0.0 m/N,E KiroIDE-1.1.70-{ID}")
    );
    assert_eq!(
        ide_short_user_agent(CODEWHISPERER_API, ID),
        format!("aws-sdk-js/1.0.0 KiroIDE-1.1.70-{ID}")
    );
    assert_eq!(refresh_user_agent(ID), format!("KiroIDE-1.1.70-{ID}"));
}

#[test]
fn no_user_agent_carries_the_cli_marker() {
    for label in [RUNTIME_API, CONTROL_PLANE_API, CODEWHISPERER_API] {
        let ua = ide_user_agent(label, ID);
        assert!(!ua.contains("exec-env"), "{ua}");
        assert!(!ua.contains("AmazonQ-For-CLI"), "{ua}");
    }
}

#[test]
fn every_account_gets_its_own_stable_machine_id() {
    let a = account_machine_id("device-github-a#lineage:1111");
    let b = account_machine_id("device-github-b#lineage:2222");
    assert_ne!(a, b);
    assert_eq!(a, account_machine_id("device-github-a#lineage:1111"));
    assert_ne!(a, account_machine_id("device-github-a#lineage:3333"));
    for id in [&a, &b] {
        assert_eq!(id.len(), 64);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}

#[test]
fn generation_headers_carry_the_account_machine_id() {
    let id = account_machine_id("acct#lineage:x");
    let headers = kiro_headers("tok", &id);
    assert!(header(&headers, "User-Agent")
        .unwrap()
        .contains(&format!("KiroIDE-1.1.70-{id}-KAS/0.66.7")));
    assert!(header(&headers, "x-amz-user-agent")
        .unwrap()
        .ends_with(&format!("KiroIDE-1.1.70-{id}-KAS/0.66.7")));
    assert_eq!(
        header(&headers, "x-amzn-kiro-client-attribution"),
        Some("kiro-ide")
    );
    assert_eq!(
        header(&headers, "x-amz-target"),
        Some("KiroRuntimeService.GenerateAssistantResponse")
    );
}

#[test]
fn management_headers_match_the_ide_control_plane_calls() {
    let json_call = management_headers(
        "tok",
        Some("KiroControlPlaneBearerService.ListAvailableModels"),
        CONTROL_PLANE_API,
        ID,
    );
    let names: Vec<String> = json_call
        .iter()
        .map(|(k, _)| k.to_ascii_lowercase())
        .collect();
    assert_eq!(
        names,
        [
            "authorization",
            "content-type",
            "x-amz-target",
            "user-agent",
            "x-amz-user-agent",
            "amz-sdk-invocation-id",
            "amz-sdk-request"
        ]
    );
    let get_call = management_headers("tok", None, CODEWHISPERER_API, ID);
    assert!(header(&get_call, "x-amz-target").is_none());
    assert!(header(&get_call, "content-type").is_none());
    for h in [&json_call, &get_call] {
        for absent in [
            "x-amzn-kiro-client-attribution",
            "x-amzn-codewhisperer-optout",
            "x-kiro-attempt",
        ] {
            assert!(header(h, absent).is_none(), "{absent}");
        }
    }
}

#[test]
fn tool_schemas_keep_additional_properties() {
    for value in [json!(false), json!(true), json!({"type": "string"})] {
        let schema = json!({
            "type": "object",
            "properties": {"tags": {"type": "object", "additionalProperties": value}},
            "additionalProperties": false
        });
        let out = kiro_lb::convert_core::sanitize_json_schema(Some(&schema));
        assert_eq!(out["additionalProperties"], json!(false));
        assert_eq!(out["properties"]["tags"]["additionalProperties"], value);
    }
}
