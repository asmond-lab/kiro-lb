use kiro_lb::auth::RefreshLease;

fn leases() -> i64 {
    kiro_lb::store::with(|c| {
        c.query_row("SELECT COUNT(*) FROM credential_refresh_leases", [], |r| {
            r.get(0)
        })
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_refresh_releases_its_lease() {
    let dir = std::env::temp_dir().join(format!("kirolb-lease-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    kiro_lb::store::initialize().unwrap();

    let owner = kiro_lb::store::try_acquire_refresh_lease("acct", 60.0).expect("lease");
    let task = tokio::spawn(async move {
        let _lease = RefreshLease {
            account: "acct".into(),
            owner,
        };
        futures_util::future::pending::<()>().await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(leases(), 1);
    task.abort();
    let _ = task.await;
    assert_eq!(leases(), 0);
    assert!(kiro_lb::store::try_acquire_refresh_lease("acct", 60.0).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}
