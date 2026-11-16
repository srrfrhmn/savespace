//! HTTP tests for connections and webhooks, with a real PostgreSQL 18 and
//! the real Plaid adapter against a mock Plaid server (wiremock).
//!
//! Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use std::sync::Arc;

use aggregator_plaid::{PlaidClient, PlaidConfig, PlaidEnv};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use domain_identity::token;
use domain_sync::SyncService;
use http_body_util::BodyExt;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use platform_crypto::{Kek, Keyring};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

async fn app(admin: &PgPool, plaid_server: &MockServer) -> Router {
    let pool = runtime_pool(admin).await;
    let plaid = PlaidClient::new(PlaidConfig {
        env: PlaidEnv::Sandbox,
        client_id: "client".into(),
        secret: "secret".into(),
        base_url: Some(plaid_server.uri()),
    })
    .unwrap();
    let kek = Kek::from_base64(&STANDARD.encode([5_u8; 32])).unwrap();
    let sync = SyncService::new(pool.clone(), Keyring::new(kek), Arc::new(plaid.clone()));
    let state = api::AppState::new(pool, ORIGIN.parse().unwrap(), None)
        .unwrap()
        .with_sync(sync, plaid);
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

async fn send(app: &Router, request: Request<Body>) -> Response<Body> {
    app.clone().oneshot(request).await.unwrap()
}

async fn json_body(response: Response<Body>) -> Value {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or(Value::Null)
}

fn post(uri: &str, cookie: &str, body: &Value) -> Request<Body> {
    Request::post(uri)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str, cookie: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

async fn sign_in(app: &Router) -> String {
    let response = send(app, post("/v1/auth/recover", "", &json!({ "code": CODE }))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    set_cookie.split(';').next().unwrap().to_owned()
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn connect_flow(admin: PgPool) {
    seed_owner(&admin).await;
    let plaid = MockServer::start().await;
    Mock::given(path("/link/token/create"))
        .and(body_partial_json(json!({
            "products": ["transactions"],
            "webhook": "https://app.example.test/v1/webhooks/plaid"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "link_token": "link-sandbox-1", "expiration": "2026-11-17T00:00:00Z"
        })))
        .mount(&plaid)
        .await;
    Mock::given(path("/item/public_token/exchange"))
        .and(body_partial_json(
            json!({ "public_token": "public-sandbox-1" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "access-sandbox-1", "item_id": "item-1"
        })))
        .mount(&plaid)
        .await;
    let app = app(&admin, &plaid).await;
    let cookie = sign_in(&app).await;

    // The CSP allows Plaid Link and the sandbox API.
    let response = send(&app, get("/health", "")).await;
    let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        csp.contains("https://cdn.plaid.com/link/v2/stable/link-initialize.js"),
        "{csp}"
    );
    assert!(csp.contains("frame-src https://cdn.plaid.com/"), "{csp}");
    assert!(
        csp.contains("connect-src 'self' https://sandbox.plaid.com/"),
        "{csp}"
    );

    // A recovery-code sign-in counts as step-up, so a link token works now.
    let response = send(&app, post("/v1/link_tokens", &cookie, &json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["link_token"], "link-sandbox-1");

    // Connect.
    let response = send(
        &app,
        post(
            "/v1/connections",
            &cookie,
            &json!({
                "public_token": "public-sandbox-1",
                "institution": { "id": "ins_109508", "name": "First Platypus Bank" }
            }),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let connection = json_body(response).await;
    let id = connection["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("conn_"));
    assert_eq!(connection["status"], "syncing");
    assert_eq!(connection["institution_name"], "First Platypus Bank");

    let response = send(&app, get("/v1/connections", &cookie)).await;
    assert_eq!(json_body(response).await["data"][0]["id"], id.as_str());
    let jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app.jobs WHERE kind = 'sync.connection'")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(jobs, 1);

    // Lists work before the first sync. Paging is checked.
    let response = send(&app, get("/v1/accounts", &cookie)).await;
    assert_eq!(json_body(response).await["data"], json!([]));
    let response = send(&app, get("/v1/transactions?limit=500", &cookie)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = send(&app, get("/v1/transactions?starting_after=acct_x", &cookie)).await;
    assert_eq!(json_body(response).await["error"]["code"], "cursor_invalid");

    // Step-up actions need a recent check.
    sqlx::query("UPDATE app.sessions SET verified_at = now() - interval '10 minutes'")
        .execute(&admin)
        .await
        .unwrap();
    let response = send(&app, post("/v1/link_tokens", &cookie, &json!({}))).await;
    assert_eq!(
        json_body(response).await["error"]["code"],
        "step_up_required"
    );
    let response = send(
        &app,
        Request::delete(format!("/v1/connections/{id}"))
            .header(header::ORIGIN, ORIGIN)
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // A wrong ID type is a 404.
    let response = send(
        &app,
        post("/v1/connections/acct_0/sync", &cookie, &json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = send(
        &app,
        post(&format!("/v1/connections/{id}/sync"), &cookie, &json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
}

fn signing_key() -> SigningKey {
    SigningKey::from_slice(&[3_u8; 32]).unwrap()
}

fn jwt(body: &[u8]) -> String {
    let header =
        URL_SAFE_NO_PAD.encode(json!({ "alg": "ES256", "kid": "k1", "typ": "JWT" }).to_string());
    let claims = URL_SAFE_NO_PAD.encode(
        json!({
            "iat": jiff::Timestamp::now().as_second(),
            "request_body_sha256": hex::encode(Sha256::digest(body))
        })
        .to_string(),
    );
    let input = format!("{header}.{claims}");
    let signature: Signature = signing_key().sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn webhooks_need_a_valid_signature_but_no_session_or_origin(admin: PgPool) {
    let plaid = MockServer::start().await;
    let point = signing_key().verifying_key().to_sec1_point(false);
    let bytes = point.as_bytes();
    Mock::given(path("/webhook_verification_key/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "key": {
                "alg": "ES256", "crv": "P-256", "kid": "k1", "kty": "EC", "use": "sig",
                "x": URL_SAFE_NO_PAD.encode(&bytes[1..33]),
                "y": URL_SAFE_NO_PAD.encode(&bytes[33..65]),
                "created_at": 1, "expired_at": null
            }
        })))
        .mount(&plaid)
        .await;
    let app = app(&admin, &plaid).await;
    let body = json!({
        "webhook_type": "TRANSACTIONS",
        "webhook_code": "SYNC_UPDATES_AVAILABLE",
        "item_id": "item-1",
        "initial_update_complete": true
    })
    .to_string();

    let unsigned = Request::post("/v1/webhooks/plaid")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.clone()))
        .unwrap();
    let response = send(&app, unsigned).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "webhook_invalid"
    );

    let wrong_body = Request::post("/v1/webhooks/plaid")
        .header("plaid-verification", jwt(b"{}"))
        .body(Body::from(body.clone()))
        .unwrap();
    assert_eq!(
        send(&app, wrong_body).await.status(),
        StatusCode::UNAUTHORIZED
    );

    let signed = Request::post("/v1/webhooks/plaid")
        .header(header::CONTENT_TYPE, "application/json")
        .header("plaid-verification", jwt(body.as_bytes()))
        .body(Body::from(body.clone()))
        .unwrap();
    let response = send(&app, signed).await;
    assert_eq!(response.status(), StatusCode::OK);

    let stored: (Option<String>, String) =
        sqlx::query_as("SELECT item_id, webhook_code FROM app.webhook_events")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(
        stored,
        (Some("item-1".into()), "SYNC_UPDATES_AVAILABLE".into())
    );
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM app.jobs WHERE kind = 'sync.webhook'")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
}
