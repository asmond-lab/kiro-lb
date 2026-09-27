use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;

async fn ok() -> &'static str {
    "ok"
}

#[tokio::test]
async fn model_routes_answer_cors_preflight() {
    let app = Router::new()
        .route("/v1/models", get(ok).options(kiro_lb::app::cors_preflight))
        .route(
            "/v1/models/{id}",
            get(ok).options(kiro_lb::app::cors_preflight),
        );
    for uri in ["/v1/models", "/v1/models/claude-opus-5.5"] {
        let res = app
            .clone()
            .oneshot(
                Request::options(uri)
                    .header("origin", "http://test.invalid")
                    .header("access-control-request-method", "GET")
                    .header("access-control-request-headers", "authorization")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{uri}");
        assert_eq!(res.headers()["access-control-allow-origin"], "*");
        assert!(res.headers()["access-control-allow-methods"]
            .to_str()
            .unwrap()
            .contains("GET"));
    }
}
