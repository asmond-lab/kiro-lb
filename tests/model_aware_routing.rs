use kiro_lb::errors::ErrorType;
use kiro_lb::pool::AccountManager;
use kiro_lb::settings;
use kiro_lb::upstream::http::{account_concurrency_load, concurrency_slot, reset_concurrency};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

fn profile_arn(id: &str) -> String {
    format!("arn:aws:codewhisperer:us-east-1:123456789012:profile/{id}")
}

fn source(id: &str) -> serde_json::Value {
    json!({
        "type": "internal",
        "id": id,
        "credential": {
            "refreshToken": format!("refresh-{id}"),
            "accessToken": format!("access-{id}"),
            "expiresAt": "2999-01-01T00:00:00Z",
            "profileArn": profile_arn(id),
            "region": "us-east-1"
        }
    })
}

async fn rejecting_client() -> reqwest::Client {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = socket
                    .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                    .await;
            });
        }
    });
    reqwest::Client::builder().proxy(proxy).build().unwrap()
}

fn excluded(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

#[tokio::test]
async fn selection_preserves_affinity_and_uses_capability_health_quota_and_capacity() {
    let dir = std::env::temp_dir().join(format!(
        "kirolb-model-routing-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    kiro_lb::store::initialize().unwrap();
    let sources = [
        source("unsupported"),
        source("known-a"),
        source("known-b"),
        source("unknown"),
    ];
    kiro_lb::store::with(|c| kiro_lb::store::replace_account_sources(c, &sources, true)).unwrap();
    kiro_lb::store::save_setting("load_balancing", &json!("session")).unwrap();
    kiro_lb::store::save_setting("max_account_concurrency", &json!(1)).unwrap();
    settings::load_tunables();
    reset_concurrency();

    let pool = AccountManager::new(rejecting_client().await);
    pool.load_credentials();
    for id in ["unsupported", "known-a", "known-b", "unknown"] {
        assert!(pool.initialize_account(id).await, "initialize {id}");
    }
    pool.get("unsupported")
        .unwrap()
        .models
        .update(vec![json!({"modelId": "other-model"})]);
    pool.get("known-a").unwrap().models.update(vec![
        json!({"modelId": "target-model"}),
        json!({"modelId": "other-model"}),
    ]);
    pool.get("known-b")
        .unwrap()
        .models
        .update(vec![json!({"modelId": "target-model"})]);
    pool.get("unknown").unwrap().models.seed_fallback();

    let selected = pool
        .next_account("target-model", &excluded(&["known-b"]), None)
        .await
        .unwrap();
    assert_eq!(selected.id, "known-a", "known support must beat fallbacks");

    let selected = pool
        .next_account("target-model", &excluded(&["known-a", "known-b"]), None)
        .await
        .unwrap();
    assert_eq!(selected.id, "unknown", "unknown evidence remains eligible");

    let selected = pool
        .next_account(
            "target-model",
            &excluded(&["known-a", "known-b", "unknown"]),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        selected.id, "unsupported",
        "negative evidence is a preference, not an exclusion"
    );

    let session = Some(41);
    pool.pin_session(session, "unknown");
    let selected = pool
        .next_account("target-model", &HashSet::new(), session)
        .await
        .unwrap();
    assert_eq!(
        selected.id, "unknown",
        "suitable affinity must remain sticky"
    );

    pool.pin_session(session, "unsupported");
    let selected = pool
        .next_account("target-model", &excluded(&["known-b"]), session)
        .await
        .unwrap();
    assert_eq!(
        selected.id, "known-a",
        "known-negative affinity must fail over"
    );

    pool.pin_session(session, "known-a");
    pool.report_failure(
        "known-a",
        "target-model",
        ErrorType::Recoverable,
        429,
        Some("USER_REQUEST_RATE_EXCEEDED"),
        None,
    );
    let selected = pool
        .next_account("target-model", &excluded(&["known-b"]), session)
        .await
        .unwrap();
    assert_eq!(selected.id, "unknown", "unhealthy affinity must fail over");

    {
        let known_a = pool.get("known-a").unwrap();
        let mut state = known_a.state.lock();
        state.rate_limited_until = 0.0;
        state.quota_headroom = Some(0.0);
        state.quota_overage_enabled = Some(false);
    }
    let selected = pool
        .next_account(
            "target-model",
            &excluded(&["known-b", "unknown", "unsupported"]),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        selected.id, "known-a",
        "quota-depleted account remains the last resort"
    );
    pool.get("known-a").unwrap().state.lock().quota_headroom = Some(1.0);

    let known_a_arn = profile_arn("known-a");
    let held = concurrency_slot(&known_a_arn).await.unwrap();
    assert_eq!(account_concurrency_load(&known_a_arn), Some((1, 1)));
    let selected = pool
        .next_account("target-model", &excluded(&["unknown", "unsupported"]), None)
        .await
        .unwrap();
    assert_eq!(
        selected.id, "known-b",
        "a saturated account must not receive new work"
    );

    let waiter_arn = known_a_arn.clone();
    let waiter = tokio::spawn(async move { concurrency_slot(&waiter_arn).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    waiter.abort();
    drop(held);
    let reacquired = tokio::time::timeout(Duration::from_secs(1), concurrency_slot(&known_a_arn))
        .await
        .expect("a cancelled waiter must not retain capacity")
        .unwrap();
    drop(reacquired);
    assert_eq!(account_concurrency_load(&known_a_arn), Some((0, 1)));

    pool.report_failure(
        "known-a",
        "target-model",
        ErrorType::Recoverable,
        400,
        Some("INVALID_MODEL_ID"),
        None,
    );
    assert_eq!(
        pool.get("known-a").unwrap().models.support("target-model"),
        kiro_lb::model_resolver::ModelSupport::Unsupported
    );
    assert_eq!(
        pool.get("known-a").unwrap().models.support("other-model"),
        kiro_lb::model_resolver::ModelSupport::Supported,
        "invalid-model evidence must stay scoped to the requested model"
    );

    let replaced = pool.get("known-b").unwrap();
    pool.remove_account("known-b");
    replaced
        .models
        .update(vec![json!({"modelId": "target-model"})]);
    pool.load_credentials();
    let replacement = pool.get("known-b").unwrap();
    assert!(pool.initialize_account("known-b").await);
    replacement
        .models
        .update(vec![json!({"modelId": "target-model"})]);
    let selected = pool
        .next_account(
            "target-model",
            &excluded(&["known-a", "unknown", "unsupported"]),
            None,
        )
        .await
        .unwrap();
    assert!(
        Arc::ptr_eq(&selected, &replacement) && !Arc::ptr_eq(&selected, &replaced),
        "selection must return the live replacement, not a refreshed removed account"
    );

    reset_concurrency();
    let _ = std::fs::remove_dir_all(dir);
}
