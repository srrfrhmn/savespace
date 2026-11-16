//! HTTP tests for insights, the reflection, and check-ins, with a real
//! PostgreSQL 18. Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, Response, StatusCode, header};
use domain_categorize::CategorizeService;
use domain_identity::token;
use http_body_util::BodyExt;
use serde_json::{Value, json};
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

/// The owner, with a recovery code to sign in, one account, and three
/// enriched transactions. Returns the household ID.
async fn seed(admin: &PgPool) -> Uuid {
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
    sqlx::query(
        "WITH c AS (INSERT INTO app.connections (household_id, provider, provider_item_id, \
           access_token_enc) VALUES ($1, 'plaid', 'item', '\\x00') RETURNING id), \
         a AS (INSERT INTO app.accounts (household_id, connection_id, provider_account_id, \
           name, type, currency) SELECT $1, id, 'a1', 'Checking', 'depository', 'USD' FROM c \
           RETURNING id, connection_id) \
         INSERT INTO app.transactions (household_id, connection_id, account_id, \
           provider_transaction_id, raw_version, amount_minor, currency, date, name, pending, \
           provider_category_primary) \
         SELECT $1, a.connection_id, a.id, v.id, 1, v.amount, 'USD', v.date::date, v.name, \
           v.pending, v.cat FROM a, (VALUES \
             ('t1', -450, '2026-11-03', 'SQ *BLUE BOTTLE #12', false, 'FOOD_AND_DRINK'), \
             ('t2', -500, '2026-11-02', 'sq*Blue Bottle #77', false, 'FOOD_AND_DRINK'), \
             ('t3', -10000, '2026-11-01', 'TARGET 00012345', false, NULL)) \
           AS v(id, amount, date, name, pending, cat)",
    )
    .bind(household)
    .execute(admin)
    .await
    .unwrap();
    let svc = CategorizeService::new(runtime_pool(admin).await);
    assert_eq!(svc.enrich_household(household).await.unwrap(), 3);
    household
}

async fn app(admin: &PgPool) -> Router {
    let state =
        api::AppState::new(runtime_pool(admin).await, ORIGIN.parse().unwrap(), None).unwrap();
    api::build_router(state, None)
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response: Response<Body> = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn request(method: Method, uri: &str, cookie: &str, body: Option<&Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::ORIGIN, ORIGIN)
        .header(header::COOKIE, cookie);
    match body {
        Some(b) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => builder.body(Body::empty()),
    }
    .unwrap()
}

async fn sign_in(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/auth/recover",
            "",
            Some(&json!({ "code": CODE })),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    set_cookie.split(';').next().unwrap().to_owned()
}

#[allow(dead_code, reason = "shared helper")]
fn category_id(categories: &Value, name: &str) -> String {
    categories["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn insights_reflection_and_check_ins(admin: PgPool) {
    seed(&admin).await;
    let app = app(&admin).await;
    let (status, _) = send(&app, request(Method::GET, "/v1/insights", "", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let cookie = sign_in(&app).await;

    let (status, list) = send(&app, request(Method::GET, "/v1/insights", &cookie, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"], json!([]));
    let (status, bad) = send(
        &app,
        request(Method::GET, "/v1/insights?transaction_id=x", &cookie, None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"]["code"], "transaction_invalid");

    let missing = "/v1/insights/ins_00000000000000000000000000000000/feedback";
    let (status, _) = send(
        &app,
        request(
            Method::POST,
            missing,
            &cookie,
            Some(&json!({ "kind": "not_helpful" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, bad) = send(
        &app,
        request(
            Method::POST,
            missing,
            &cookie,
            Some(&json!({ "kind": "nope" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"]["code"], "feedback_invalid");
    let (_, mutes) = send(
        &app,
        request(Method::GET, "/v1/insights/mutes", &cookie, None),
    )
    .await;
    assert_eq!(mutes["data"], json!([]));

    let (status, r) = send(
        &app,
        request(Method::GET, "/v1/reflections/2026-11-04", &cookie, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["week_start"], "2026-11-02");
    let cards = r["cards"].as_array().unwrap();
    assert!(!cards.is_empty() && cards.len() <= 5);
    assert_ne!(cards.last().unwrap()["tone"], "higher");
    let (status, bad) = send(
        &app,
        request(Method::GET, "/v1/reflections/2026-W45", &cookie, None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"]["code"], "week_invalid");

    // No token key in this test server: a note is refused, a score is saved.
    let (status, bad) = send(
        &app,
        request(
            Method::POST,
            "/v1/check_ins",
            &cookie,
            Some(&json!({ "score": 4, "note": "ok" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(bad["error"]["code"], "notes_unavailable");
    let (status, bad) = send(
        &app,
        request(
            Method::POST,
            "/v1/check_ins",
            &cookie,
            Some(&json!({ "score": 6 })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"]["code"], "score_invalid");
    let (status, saved) = send(
        &app,
        request(
            Method::POST,
            "/v1/check_ins",
            &cookie,
            Some(&json!({ "score": 3, "note": "  " })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{saved}");
    assert!(saved["id"].as_str().unwrap().starts_with("chk_"));
    assert_eq!(saved["note"], Value::Null);
    let (_, list) = send(&app, request(Method::GET, "/v1/check_ins", &cookie, None)).await;
    assert_eq!(list["data"][0]["score"], 3);
}
