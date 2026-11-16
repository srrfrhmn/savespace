//! Router tests. They run without a network socket or a database.

#![expect(clippy::unwrap_used, reason = "test helpers")]

use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// A pool that points at a closed port. Queries fail fast.
fn state() -> api::AppState {
    let mut config = platform_db::PoolConfig::new("api-test");
    config.acquire_timeout = std::time::Duration::from_millis(300);
    let db =
        platform_db::connect_lazy("postgres://nobody:nothing@127.0.0.1:1/none", &config).unwrap();
    let origin = url::Url::parse("http://localhost:8080").unwrap();
    api::AppState::new(db, origin, None).unwrap()
}

async fn get(router: Router, uri: &str) -> axum::http::Response<Body> {
    router
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn json(response: axum::http::Response<Body>) -> serde_json::Value {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

/// A fake Vite build in a new temporary directory.
fn web_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("savespace-web-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><title>SaveSpace</title>",
    )
    .unwrap();
    std::fs::write(dir.join("assets/app-abc123.js"), "console.log(1)").unwrap();
    dir
}

#[tokio::test]
async fn health_returns_ok_with_request_id_and_headers() {
    let response = get(api::build_router(state(), None), "/health").await;
    assert_eq!(response.status(), StatusCode::OK);

    let headers = response.headers();
    let request_id = headers[api::REQUEST_ID_HEADER].to_str().unwrap();
    assert!(request_id.starts_with("req_"), "{request_id}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );

    let body = json(response).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["core_version"], savespace_core::VERSION);
}

#[tokio::test]
async fn ready_is_503_when_database_is_down() {
    let response = get(api::build_router(state(), None), "/ready").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json(response).await["status"], "unavailable");
}

#[tokio::test]
async fn unknown_api_route_is_json_404() {
    let response = get(api::build_router(state(), Some(&web_dir())), "/v1/nope").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["error"]["type"], "not_found_error");
    assert!(
        body["error"]["request_id"]
            .as_str()
            .unwrap()
            .starts_with("req_")
    );
}

#[tokio::test]
async fn unknown_route_is_404_without_web_app() {
    let response = get(api::build_router(state(), None), "/nope").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn web_app_serves_index_for_client_routes() {
    let dir = web_dir();
    let response = get(api::build_router(state(), Some(&dir)), "/budget/2026-11").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("<title>SaveSpace</title>"));
}

#[tokio::test]
async fn hashed_assets_are_immutable() {
    let dir = web_dir();
    let response = get(
        api::build_router(state(), Some(&dir)),
        "/assets/app-abc123.js",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
}
