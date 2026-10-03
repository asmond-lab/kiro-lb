use kiro_lb::auth::{KiroAuth, Source};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

async fn failing_proxy() -> (reqwest::Client, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = socket
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });
    (
        reqwest::Client::builder().proxy(proxy).build().unwrap(),
        hits,
    )
}

fn credentials(dir: &std::path::Path, name: &str, expires_in: i64) -> String {
    let expires = chrono_like(expires_in);
    let path = dir.join(name);
    std::fs::write(
        &path,
        json!({
            "accessToken": "still-valid",
            "refreshToken": "refresh",
            "expiresAt": expires,
            "profileArn": "arn:aws:codewhisperer:us-east-1:123456789012:profile/x",
            "region": "us-east-1"
        })
        .to_string(),
    )
    .unwrap();
    path.to_string_lossy().into_owned()
}

fn chrono_like(offset: i64) -> String {
    kiro_lb::auth::iso_from_epoch(kiro_lb::store::now_f64() + offset as f64)
}

#[tokio::test]
async fn a_transient_refresh_failure_keeps_serving_the_valid_token() {
    let dir =
        std::env::temp_dir().join(format!("kirolb-refresh-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    kiro_lb::store::initialize().unwrap();
    let (http, hits) = failing_proxy().await;

    let near = credentials(&dir, "near.json", 300);
    let auth = KiroAuth::new(Source::File(near), "us-east-1", None, http.clone()).unwrap();
    assert_eq!(auth.access_token().await.unwrap(), "still-valid");
    let after_first = hits.load(Ordering::SeqCst);
    assert!(after_first >= 1, "the refresh was attempted");
    assert_eq!(auth.access_token().await.unwrap(), "still-valid");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        after_first,
        "no new refresh attempt inside the backoff window"
    );

    let gone = credentials(&dir, "gone.json", -10);
    let auth = KiroAuth::new(Source::File(gone), "us-east-1", None, http).unwrap();
    let before_expired = hits.load(Ordering::SeqCst);
    assert!(
        auth.access_token().await.is_err(),
        "an expired token is never served"
    );
    let after_expired = hits.load(Ordering::SeqCst);
    assert!(
        after_expired > before_expired,
        "the expired token was refreshed once"
    );
    for _ in 0..3 {
        assert!(auth.access_token().await.is_err());
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        after_expired,
        "an expired token waits out the backoff instead of hammering a failing auth host"
    );
}
