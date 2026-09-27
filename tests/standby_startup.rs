use serde_json::json;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Guard(Child, std::path::PathBuf);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_standby_slot_serves_health_without_waiting_on_account_refresh() {
    let dir = std::env::temp_dir().join(format!("kirolb-standby-startup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    std::env::set_var("KIRO_SLOT", "green");
    kiro_lb::store::initialize().unwrap();
    kiro_lb::store::set_runtime_writer("blue").unwrap();
    let expired = |n: &str| json!({"type": "internal", "id": n, "credential": {"refreshToken": format!("refresh-{n}"), "accessToken": format!("access-{n}"), "expiresAt": "2000-01-01T00:00:00Z", "region": "us-east-1"}});
    kiro_lb::store::with(|c| {
        kiro_lb::store::replace_account_sources(c, &[expired("a1"), expired("a2")], true)
    })
    .unwrap();

    let port = free_port();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kirolb"));
    cmd.current_dir(&dir)
        .env("PROXY_API_KEY", "k")
        .env("DASHBOARD_PASSWORD", "p")
        .env("SERVER_HOST", "127.0.0.1")
        .env("SERVER_PORT", port.to_string())
        .env("DASHBOARD_DATA_DIR", &dir)
        .env("KIRO_SLOT", "green")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _guard = Guard(cmd.spawn().unwrap(), dir.clone());

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}/health");
    let started = Instant::now();
    let mut status = None;
    while started.elapsed() < Duration::from_secs(10) {
        if let Ok(r) = client.get(&url).send().await {
            status = Some(r.status().as_u16());
            if status == Some(200) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(
        status,
        Some(200),
        "no healthy response after {:?}",
        started.elapsed()
    );
}
