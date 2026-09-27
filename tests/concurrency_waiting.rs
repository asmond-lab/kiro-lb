use kiro_lb::settings::{self, TunableKey};
use kiro_lb::upstream::http::{concurrency_slot, concurrency_status, reset_concurrency};
use serde_json::json;
use std::sync::Once;
use std::time::Duration;

static INIT: Once = Once::new();
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn init() {
    INIT.call_once(|| {
        let dir =
            std::env::temp_dir().join(format!("kirolb-waiting-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("DASHBOARD_DATA_DIR", &dir);
        kiro_lb::store::initialize().unwrap();
        settings::load_tunables();
    });
}

fn configure(global: i64, account: i64) {
    settings::set_tunable(&TunableKey::MaxConcurrency, &json!(global))
        .unwrap()
        .unwrap();
    settings::set_tunable(&TunableKey::MaxAccountConcurrency, &json!(account))
        .unwrap()
        .unwrap();
    reset_concurrency();
}

async fn abandoned_waiter_leaves_no_queue(account: &str) {
    let held = concurrency_slot(account).await.unwrap();
    let owned = account.to_owned();
    let waiter = tokio::spawn(async move { concurrency_slot(&owned).await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let label = kiro_lb::pool::account_label(account);
    assert_eq!(concurrency_status()["accounts"][&label]["waiting"], 1);
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let status = concurrency_status();
    drop(held);
    assert_eq!(status["accounts"][&label]["waiting"], 0, "{status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_waiter_on_the_global_gate_is_not_counted() {
    let _serial = SERIAL.lock().await;
    init();
    configure(1, 2);
    abandoned_waiter_leaves_no_queue("acct-global").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_waiter_on_the_account_gate_is_not_counted() {
    let _serial = SERIAL.lock().await;
    init();
    configure(0, 1);
    abandoned_waiter_leaves_no_queue("acct-account").await;
}
