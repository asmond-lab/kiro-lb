use bytes::Bytes;
use futures_util::StreamExt;
use kiro_lb::pool::AccountManager;
use kiro_lb::upstream::endpoints::{
    attempt_order, cooldown_remaining, record_failure_backoff, record_success,
};
use kiro_lb::usage_tracking::RequestCtx;
use serde_json::json;

fn data_dir(tag: &str) {
    let dir = std::env::temp_dir().join(format!("kirolb-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    kiro_lb::store::initialize().unwrap();
}

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

async fn pool_with(ids: &[&str]) -> std::sync::Arc<AccountManager> {
    let sources: Vec<_> = ids.iter().map(|id| source(id)).collect();
    kiro_lb::store::with(|c| kiro_lb::store::replace_account_sources(c, &sources, true)).unwrap();
    let pool = AccountManager::new(reqwest::Client::new());
    pool.load_credentials();
    pool.load_state();
    for id in ids {
        assert!(pool.initialize_account(id).await, "initialize {id}");
        pool.get(id)
            .unwrap()
            .models
            .update(vec![json!({"modelId": "m"})]);
    }
    pool
}

#[tokio::test]
async fn a_cut_tool_call_is_recorded_on_the_request() {
    data_dir("cutlog");
    let request = RequestCtx::new(None);
    let body: kiro_lb::stream_core::ByteStream = Box::pin(futures_util::stream::iter(vec![Ok::<Bytes, reqwest::Error>(
        Bytes::from_static(br#"{"name":"Read","toolUseId":"t1","input":"{\"file_path\": \"/a\"}","stop":true}{"name":"Write","toolUseId":"t2","input":"{\"file_path\": \"/b\""}"#),
    )]));
    let events: Vec<_> = kiro_lb::stream_core::parse_kiro_stream_metered(body, 1.0, 1.0, &request)
        .collect()
        .await;
    assert!(events.iter().all(Result::is_ok));
    assert_eq!(request.usage.lock().upstream_cut.as_deref(), Some("Write"));
}

#[test]
fn hidden_models_are_stored_by_their_kiro_id() {
    data_dir("listing");
    kiro_lb::settings::load_unlisted_models();
    let saved =
        kiro_lb::settings::set_unlisted_models(&json!(["claude-opus-4-6", "claude-opus-4.6"]))
            .unwrap()
            .unwrap();
    assert_eq!(saved, vec!["claude-opus-4.6"]);
    assert_eq!(
        kiro_lb::settings::unlisted_models(),
        vec!["claude-opus-4.6"]
    );
    assert!(!kiro_lb::settings::is_listed("claude-opus-4-6"));
}

#[test]
fn fastest_ignores_affinity_and_a_zero_cooldown_disables_backoff() {
    data_dir("affinity");
    let order: Vec<String> = ["runtime", "codewhisperer"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    record_success("acct", "m", "codewhisperer");
    assert_eq!(
        attempt_order(Some(("acct", "m")), &order)[0].key,
        "codewhisperer"
    );
    assert_eq!(attempt_order(None, &order)[0].key, "runtime");

    assert_eq!(record_failure_backoff("runtime", 0.0), 0.0);
    assert_eq!(cooldown_remaining("runtime"), 0.0);
}

#[tokio::test]
async fn a_failed_forced_catalog_read_does_not_postpone_the_next_one() {
    data_dir("catalog");
    let pool = pool_with(&["a"]).await;
    let a = pool.get("a").unwrap();
    a.state.lock().models_cached_at = 1.0;
    assert!(!pool.force_refresh_models_with(&a, |_| async { None }).await);
    assert_eq!(a.state.lock().models_cached_at, 1.0);
}

#[tokio::test]
async fn re_pinning_an_evicted_session_counts_it_once() {
    std::env::set_var("SESSION_AFFINITY_CAPACITY", "1");
    data_dir("pincap");
    kiro_lb::store::save_setting("load_balancing", &json!("session")).unwrap();
    kiro_lb::settings::load_tunables();
    let pool = pool_with(&["a", "b"]).await;
    pool.pin_session(Some(7), "a");
    pool.pin_session(Some(7), "b");
    let counts = pool.session_counts();
    assert_eq!(pool.get("a").unwrap().state.lock().sessions, 0);
    assert_eq!(pool.get("b").unwrap().state.lock().sessions, 1);
    assert_eq!(counts.get("b"), Some(&1));
}
