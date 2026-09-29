use kiro_lb::auth::{KiroAuth, Source};
use kiro_lb::store;
use serde_json::json;
use sha2::{Digest, Sha256};

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

#[tokio::test]
async fn social_logins_sharing_a_profile_arn_keep_separate_identities() {
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
        for entry in &entries[2..] {
            let refresh = entry["credential"]["refreshToken"].as_str().unwrap();
            let fingerprint = format!("source:{}", hex::encode(Sha256::digest(refresh)));
            c.execute(
                "UPDATE account_sources SET login_identity = ?1, source_fingerprint = ?2 WHERE account_id = ?3",
                rusqlite::params![LEGACY, fingerprint, entry["id"].as_str().unwrap()],
            )?;
        }
        Ok(())
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

    let http = reqwest::Client::new();
    for (index, id) in ids.iter().enumerate() {
        let auth = KiroAuth::new(
            Source::Internal((*id).into()),
            "us-east-1",
            None,
            http.clone(),
        )
        .unwrap();
        assert!(
            auth.is_current_login(),
            "migrated login must remain usable: {id}"
        );
        assert_eq!(
            auth.access_token().await.unwrap(),
            entries[index]["credential"]["accessToken"]
                .as_str()
                .unwrap()
        );
    }

    for (index, id) in ids.iter().enumerate() {
        let auth = KiroAuth::new(
            Source::Internal((*id).into()),
            "us-east-1",
            None,
            http.clone(),
        )
        .unwrap();
        // Replacing a source with the same profile and legacy marker must not
        // let the old auth instance serve or overwrite the replacement login.
        store::save_internal_credential(id, &social(&format!("replacement-{index}"), true))
            .unwrap();
        assert!(!auth.is_current_login());
        assert!(auth.access_token().await.is_err());
        let replacement = KiroAuth::new(
            Source::Internal((*id).into()),
            "us-east-1",
            None,
            http.clone(),
        )
        .unwrap();
        assert_ne!(replacement.login_identity(), auth.login_identity());
        assert_eq!(
            replacement.access_token().await.unwrap(),
            format!("access-replacement-{index}")
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
