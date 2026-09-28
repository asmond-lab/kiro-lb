use kiro_lb::auth::{KiroAuth, Source};
use kiro_lb::store;
use serde_json::json;

const SHARED_SOCIAL_PROFILE: &str =
    "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK";
const LEGACY: &str = "login:f5949ea88b1e8505000000000000000000000000000000000000000000000000";

fn social(refresh: &str, legacy_marker: bool) -> serde_json::Value {
    let mut c = json!({
        "refreshToken": refresh,
        "accessToken": format!("access-{refresh}"),
        "expiresAt": "2999-01-01T00:00:00Z",
        "region": "us-east-1",
        "profileArn": SHARED_SOCIAL_PROFILE
    });
    if legacy_marker {
        c["_kiroLbLoginIdentity"] = json!(LEGACY);
    }
    c
}

#[test]
fn social_logins_sharing_a_profile_arn_keep_separate_identities() {
    let dir =
        std::env::temp_dir().join(format!("kirolb-identity-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    store::initialize().unwrap();
    let entries = vec![
        json!({"type": "internal", "id": "device-github-a", "credential": social("refresh-a", false)}),
        json!({"type": "internal", "id": "device-google-b", "credential": social("refresh-b", false)}),
        json!({"type": "internal", "id": "device-github-legacy-c", "credential": social("refresh-c", true)}),
        json!({"type": "internal", "id": "device-google-legacy-d", "credential": social("refresh-d", true)}),
    ];
    store::with(|c| store::replace_account_sources(c, &entries, true)).unwrap();
    store::with(|c| {
        c.execute(
            "UPDATE account_sources SET login_identity = ?1 WHERE account_id LIKE '%legacy%'",
            [LEGACY],
        )
        .map(|_| ())
    })
    .unwrap();

    let ids = [
        "device-github-a",
        "device-google-b",
        "device-github-legacy-c",
        "device-google-legacy-d",
    ];
    let identities: Vec<String> = ids
        .iter()
        .map(|id| KiroAuth::bind_source_login(&Source::Internal((*id).into())).expect("identity"))
        .collect();
    let stored: Vec<String> = ids
        .iter()
        .map(|id| store::login_identity(id).unwrap())
        .collect();
    let again: Vec<String> = ids
        .iter()
        .map(|id| KiroAuth::bind_source_login(&Source::Internal((*id).into())).unwrap())
        .collect();
    let _ = std::fs::remove_dir_all(&dir);

    let unique: std::collections::HashSet<&String> = identities.iter().collect();
    assert_eq!(unique.len(), ids.len(), "{identities:?}");
    assert!(
        identities
            .iter()
            .all(|i| !store::is_legacy_profile_identity(i)),
        "{identities:?}"
    );
    assert_eq!(identities, stored);
    assert_eq!(
        identities, again,
        "a lineage must be stable across restarts"
    );
}
