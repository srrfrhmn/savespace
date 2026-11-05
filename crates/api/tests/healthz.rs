//! Router tests. They run without a network socket.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn healthz_returns_ok_with_request_id() {
    let response = api::build_router()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let request_id = response
        .headers()
        .get(api::REQUEST_ID_HEADER)
        .expect("request-id header")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(request_id.starts_with("req_"), "{request_id}");

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["core_version"], savespace_core::VERSION);
}

#[tokio::test]
async fn unknown_route_is_404() {
    let response = api::build_router()
        .oneshot(Request::get("/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
