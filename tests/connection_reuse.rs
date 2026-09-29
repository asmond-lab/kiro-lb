use axum::extract::ConnectInfo;
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn idle_connections_survive_between_prompts_and_are_pinged() {
    let peers: Arc<Mutex<Vec<SocketAddr>>> = Arc::default();
    let seen = peers.clone();
    let app = Router::new().route(
        "/",
        get(move |ConnectInfo(peer): ConnectInfo<SocketAddr>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(peer);
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let client = kiro_lb::upstream::http::upstream_builder(None)
        .http2_prior_knowledge()
        .no_proxy()
        .build()
        .unwrap();
    let url = format!("http://{addr}/");
    for pause in [0, 3] {
        tokio::time::sleep(Duration::from_secs(pause)).await;
        let res = client.get(&url).send().await.unwrap();
        assert_eq!(res.version(), reqwest::Version::HTTP_2);
        assert_eq!(res.text().await.unwrap(), "ok");
    }

    let peers = peers.lock().unwrap().clone();
    assert_eq!(peers.len(), 2);
    assert_eq!(
        peers[0], peers[1],
        "the second request should reuse the idle HTTP/2 connection"
    );
}
