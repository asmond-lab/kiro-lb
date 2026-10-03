use axum::http::HeaderMap;

#[tokio::test]
async fn dashboard_opens_without_a_session_when_auth_is_off() {
    let dir =
        std::env::temp_dir().join(format!("kirolb-authoff-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    std::env::set_var("DASHBOARD_AUTH", "false");
    std::env::set_var("DASHBOARD_PASSWORD", "unused-secret");
    kiro_lb::store::initialize().unwrap();
    assert!(!kiro_lb::config::get().dashboard_auth);

    let keys = kiro_lb::routes_dashboard::list_keys(HeaderMap::new()).await;
    assert_eq!(keys.status(), 200);

    let login =
        kiro_lb::routes_dashboard::login(HeaderMap::new(), bytes::Bytes::from_static(b"{}")).await;
    assert_eq!(login.status(), 200);
    assert!(login.headers().get("set-cookie").is_none());

    let headers = |pairs: &[(&'static str, &'static str)]| {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    };
    let same = headers(&[
        ("host", "127.0.0.1:8000"),
        ("origin", "http://127.0.0.1:8000"),
        ("sec-fetch-site", "same-origin"),
    ]);
    assert_eq!(
        kiro_lb::routes_dashboard::list_keys(same).await.status(),
        200
    );
    let other_origin = headers(&[
        ("host", "127.0.0.1:8000"),
        ("origin", "https://evil.example"),
    ]);
    assert_eq!(
        kiro_lb::routes_dashboard::list_keys(other_origin)
            .await
            .status(),
        403
    );
    let rewritten_host = headers(&[
        ("host", "127.0.0.1:8101"),
        ("origin", "http://127.0.0.1:5173"),
        ("sec-fetch-site", "same-origin"),
    ]);
    assert_eq!(
        kiro_lb::routes_dashboard::list_keys(rewritten_host)
            .await
            .status(),
        200,
        "a dev or reverse proxy that rewrites Host keeps the SPA working"
    );
    let cross_site = headers(&[("host", "127.0.0.1:8000"), ("sec-fetch-site", "cross-site")]);
    assert_eq!(
        kiro_lb::routes_dashboard::list_keys(cross_site)
            .await
            .status(),
        403
    );
    let _ = std::fs::remove_dir_all(&dir);
}
