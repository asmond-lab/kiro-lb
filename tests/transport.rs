use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_upstream_cannot_hold_the_header_wait_open() {
    std::env::set_var("STREAMING_READ_TIMEOUT", "1");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        let _ = sock
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .await;
    });
    let client = kiro_lb::upstream::http::build_client(None);
    let started = Instant::now();
    let result = client
        .post(format!("http://{addr}/"))
        .body("{}")
        .send()
        .await;
    let waited = started.elapsed();
    assert!(result.is_err(), "a stalled upstream must fail");
    assert!(result.unwrap_err().is_timeout());
    assert!(waited < Duration::from_secs(5), "waited {waited:?}");
}
