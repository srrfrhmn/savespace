//! HTTP tests with a real PostgreSQL 18: sessions, origin checks, step-up,
//! idempotency, and rate limits.
//!
//! The API connects as `savespace_runtime`, like production. Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use domain_identity::token;
use http_body_util::BodyExt;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use tower::ServiceExt;
use uuid::Uuid;

const ORIGIN: &str = "https://app.example.test";
const CODE: &str = "AAAA-BBBB-CCCC";

async fn runtime_pool(admin: &PgPool) -> PgPool {
    PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _| {
            Box::pin(async move {
                conn.execute("SET ROLE savespace_runtime").await?;
                Ok(())
            })
        })
        .connect_with((*admin.connect_options()).clone())
        .await
        .unwrap()
}

async fn app(admin: &PgPool) -> Router {
    let pool = runtime_pool(admin).await;
    let state = api::AppState::new(pool, ORIGIN.parse().unwrap(), None).unwrap();
    api::build_router(state, None)
}

async fn seed_owner(admin: &PgPool) {
    let household = Uuid::now_v7();
    let user = Uuid::now_v7();
    sqlx::query("SELECT app.create_owner($1, $2, 'Sam')")
        .bind(user)
        .bind(household)
        .execute(admin)
        .await
        .unwrap();
    let normalized = token::normalize_recovery_code(CODE).unwrap();
    sqlx::query(
        "INSERT INTO app.recovery_codes (household_id, user_id, code_hash) VALUES ($1, $2, $3)",
    )
    .bind(household)
    .bind(user)
    .bind(token::hash_recovery_code(&normalized))
    .execute(admin)
    .await
    .unwrap();
}

fn post(uri: &str) -> axum::http::request::Builder {
    Request::post(uri)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
}

async fn send(app: &Router, request: Request<Body>) -> Response<Body> {
    app.clone().oneshot(request).await.unwrap()
}

async fn json(response: Response<Body>) -> serde_json::Value {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null)
}

/// Signs in with the recovery code and returns the `Cookie` header value.
async fn sign_in(app: &Router) -> String {
    let body = serde_json::json!({ "code": CODE });
    let response = send(
        app,
        post("/v1/auth/recover")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly"));
    set_cookie.split(';').next().unwrap().to_owned()
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn sign_in_me_and_sign_out(admin: PgPool) {
    seed_owner(&admin).await;
    let app = app(&admin).await;

    let response = send(&app, Request::get("/v1/me").body(Body::empty()).unwrap()).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = json(response).await;
    assert_eq!(body["error"]["code"], "session_required");
    assert!(
        body["error"]["request_id"]
            .as_str()
            .unwrap()
            .starts_with("req_")
    );

    let cookie = sign_in(&app).await;
    let response = send(
        &app,
        Request::get("/v1/me")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let me = json(response).await;
    assert_eq!(me["display_name"], "Sam");
    assert!(me["id"].as_str().unwrap().starts_with("usr_"));
    assert!(me["household_id"].as_str().unwrap().starts_with("hh_"));
    assert_eq!(me["recovery_codes_left"], 0);

    let response = send(
        &app,
        post("/v1/auth/logout")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );

    let response = send(
        &app,
        Request::get("/v1/me")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn state_changes_need_the_app_origin(admin: PgPool) {
    seed_owner(&admin).await;
    let app = app(&admin).await;
    let body = serde_json::json!({ "code": CODE }).to_string();

    for origin in [
        None,
        Some("https://evil.example"),
        Some("http://app.example.test"),
    ] {
        let mut request =
            Request::post("/v1/auth/recover").header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let response = send(&app, request.body(Body::from(body.clone())).unwrap()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{origin:?}");
        assert_eq!(json(response).await["error"]["code"], "origin_invalid");
    }

    // A JSON body needs the JSON content type.
    let response = send(
        &app,
        Request::post("/v1/auth/recover")
            .header(header::ORIGIN, ORIGIN)
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn step_up_and_idempotency(admin: PgPool) {
    seed_owner(&admin).await;
    let app = app(&admin).await;
    let cookie = sign_in(&app).await;

    let regenerate = |key: Option<&str>| {
        let mut request = post("/v1/me/recovery_codes").header(header::COOKIE, &cookie);
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        request.body(Body::empty()).unwrap()
    };

    // A recovery code sign-in counts as a fresh check.
    let first = send(&app, regenerate(Some("k1"))).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first = json(first).await;
    assert_eq!(first["recovery_codes"].as_array().unwrap().len(), 10);

    // Same key, same request: the stored response.
    let replay = send(&app, regenerate(Some("k1"))).await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(replay.headers()["idempotent-replayed"], "true");
    assert_eq!(json(replay).await, first);

    // Same key, other request: 409.
    let other = send(
        &app,
        post("/v1/auth/verify/start")
            .header(header::COOKIE, &cookie)
            .header("idempotency-key", "k1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(other.status(), StatusCode::CONFLICT);
    assert_eq!(json(other).await["error"]["code"], "idempotency_key_reused");

    // Six minutes later, step-up actions need a new check.
    sqlx::query("UPDATE app.sessions SET verified_at = now() - interval '6 minutes'")
        .execute(&admin)
        .await
        .unwrap();
    let late = send(&app, regenerate(None)).await;
    assert_eq!(late.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(late).await["error"]["code"], "step_up_required");

    // An error response is stored too, so a retry gets the same answer.
    let late_keyed = send(&app, regenerate(Some("k2"))).await;
    assert_eq!(late_keyed.status(), StatusCode::FORBIDDEN);
    let again = send(&app, regenerate(Some("k2"))).await;
    assert_eq!(again.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(again).await["error"]["code"], "step_up_required");
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn sign_in_attempts_are_rate_limited(admin: PgPool) {
    let app = app(&admin).await;
    let body = serde_json::json!({ "code": "ZZZZ-ZZZZ-ZZZZ" }).to_string();
    let mut statuses = Vec::new();
    for _ in 0..21 {
        let response = send(
            &app,
            post("/v1/auth/recover")
                .header("x-forwarded-for", "203.0.113.7")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await;
        statuses.push(response.status());
    }
    assert!(
        statuses[..20]
            .iter()
            .all(|s| *s == StatusCode::UNAUTHORIZED)
    );
    assert_eq!(statuses[20], StatusCode::TOO_MANY_REQUESTS);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn other_host_names_redirect_to_the_public_origin(admin: PgPool) {
    let app = app(&admin).await;
    let response = send(
        &app,
        Request::get("/settings?tab=security")
            .header(header::HOST, "savespace-api-123.us-east1.run.app")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(
        response.headers()[header::LOCATION],
        "https://app.example.test/settings?tab=security"
    );

    let response = send(
        &app,
        Request::get("/health")
            .header(header::HOST, "savespace-api-123.us-east1.run.app")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}
