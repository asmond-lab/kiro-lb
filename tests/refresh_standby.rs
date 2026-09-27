use kiro_lb::auth::{AuthError, KiroAuth, Source};
use serde_json::json;
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread")]
async fn a_standby_slot_never_refreshes_without_the_lease() {
    let dir = std::env::temp_dir().join(format!("kirolb-standby-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    std::env::set_var("KIRO_SLOT", "green");
    std::env::set_var("KIRO_REFRESH_LEASE_WAIT_SECONDS", "0.3");
    kiro_lb::store::initialize().unwrap();
    kiro_lb::store::set_runtime_writer("blue").unwrap();
    assert!(kiro_lb::store::try_acquire_refresh_lease("acct", 60.0).is_none());

    let expired = json!({"refreshToken": "old-refresh", "accessToken": "old-access", "expiresAt": "2000-01-01T00:00:00Z", "region": "us-east-1"});
    kiro_lb::store::with(|c| {
        c.execute(
            "INSERT INTO account_sources(account_id, position, config_json, credential_json) VALUES ('acct', 0, ?1, ?2)",
            [json!({"type": "internal", "id": "acct"}).to_string(), expired.to_string()],
        )
    })
    .unwrap();
    let auth = KiroAuth::new(
        Source::Internal("acct".into()),
        "us-east-1",
        None,
        reqwest::Client::new(),
    );

    let started = Instant::now();
    let result = auth.access_token().await;
    let waited = started.elapsed();
    let stored = kiro_lb::store::load_internal_credential("acct").unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        matches!(result, Err(AuthError::Other(ref m)) if m.contains("owned by another slot")),
        "{result:?}"
    );
    assert!(waited < Duration::from_secs(5), "{waited:?}");
    assert_eq!(stored["refreshToken"], "old-refresh");
}
