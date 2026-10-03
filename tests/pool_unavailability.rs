mod common;

use kiro_lb::pool::{AccountManager, Unavailable};
use serde_json::json;

fn source(id: &str) -> serde_json::Value {
    json!({
        "type": "internal",
        "id": id,
        "credential": {
            "refreshToken": format!("refresh-{id}"),
            "accessToken": format!("access-{id}"),
            "expiresAt": "2999-01-01T00:00:00Z",
            "profileArn": format!("arn:aws:codewhisperer:us-east-1:123456789012:profile/{id}"),
            "region": "us-east-1"
        }
    })
}

#[tokio::test]
async fn unavailability_separates_lasting_states_from_temporary_ones() {
    let dir =
        std::env::temp_dir().join(format!("kirolb-unavail-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    kiro_lb::store::initialize().unwrap();
    let sources = vec![source("a"), source("b")];
    kiro_lb::store::with(|c| kiro_lb::store::replace_account_sources(c, &sources, true)).unwrap();

    let pool = AccountManager::new(common::upstream().await.http);
    pool.load_credentials();
    pool.load_state();
    for id in ["a", "b"] {
        assert!(pool.initialize_account(id).await, "initialize {id}");
        pool.get(id)
            .unwrap()
            .models
            .update(vec![json!({"modelId": "m"})]);
    }
    let a = pool.get("a").unwrap();
    let b = pool.get("b").unwrap();
    let now = kiro_lb::store::now_f64();

    assert_eq!(pool.unavailability("m"), Unavailable::Temporary);

    a.models.record_unsupported("x");
    b.models.record_unsupported("x");
    assert_eq!(pool.unavailability("x"), Unavailable::Model);

    a.state.lock().quota_exhausted_until = now + 3600.0;
    b.state.lock().rate_limited_until = now + 10.0;
    assert_eq!(
        pool.unavailability("m"),
        Unavailable::Temporary,
        "a burst limit is worth a retry"
    );

    b.state.lock().rate_limited_until = 0.0;
    b.state.lock().suspended_until = now + 3600.0;
    match pool.unavailability("m") {
        Unavailable::Quota { resets_in } => assert!(resets_in > 3500.0 && resets_in <= 3600.0),
        other => panic!("expected Quota, got {other:?}"),
    }

    a.state.lock().quota_exhausted_until = 0.0;
    a.state.lock().suspended_until = now + 3600.0;
    assert_eq!(pool.unavailability("m"), Unavailable::Accounts);

    a.state.lock().suspended_until = 0.0;
    b.state.lock().suspended_until = 0.0;
    for x in [&a, &b] {
        let mut st = x.state.lock();
        st.quota_headroom = Some(0.0);
        st.quota_overage_enabled = Some(false);
        st.quota_resets_at = now + 7200.0;
        st.quota_observed_at = now;
    }
    match pool.unavailability("m") {
        Unavailable::Quota { resets_in } => assert!(resets_in > 7100.0 && resets_in <= 7200.0),
        other => panic!("spent usage must read as quota, got {other:?}"),
    }
}
