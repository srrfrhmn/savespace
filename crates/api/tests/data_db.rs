//! HTTP tests for settings, push, export, and account deletion, with a
//! real PostgreSQL 18. Run with:
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

/// The api with a token key: check-in notes and push are on.
async fn app(admin: &PgPool) -> Router {
    use base64::Engine;
    let kek = platform_crypto::Kek::from_base64(
        &base64::engine::general_purpose::STANDARD.encode([6_u8; 32]),
    )
    .unwrap();
    let state = api::AppState::new(runtime_pool(admin).await, ORIGIN.parse().unwrap(), None)
        .unwrap()
        .with_kek(kek);
    api::build_router(state, None)
}

async fn raw(app: &Router, request: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, body)
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
async fn settings_push_export_and_delete(admin: PgPool) {
    seed(&admin).await;
    let app = app(&admin).await;
    let cookie = sign_in(&app).await;

    // Settings.
    let patch = json!({
        "time_zone": "America/Chicago",
        "quiet_start": "22:30",
        "quiet_end": "07:00",
        "notify_daily_limit": 2,
        "notify_muted_kinds": ["awareness", "awareness"],
        "currency": "CAD"
    });
    let (status, me) = send(
        &app,
        request(Method::PATCH, "/v1/me", &cookie, Some(&patch)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["time_zone"], "America/Chicago");
    assert_eq!(me["notifications"]["quiet_start"], "22:30");
    assert_eq!(me["notifications"]["daily_limit"], 2);
    assert_eq!(me["notifications"]["muted_kinds"], json!(["awareness"]));
    assert_eq!(me["currency"], "CAD");
    let (_, budget) = send(
        &app,
        request(Method::GET, "/v1/budgets/2026-11", &cookie, None),
    )
    .await;
    assert_eq!(
        budget["currency"], "CAD",
        "budgets follow the household currency"
    );
    for (body, code) in [
        (json!({ "currency": "EUR" }), "currency_invalid"),
        (json!({ "time_zone": "Mars/Base" }), "time_zone_invalid"),
        (json!({ "quiet_start": "25:00" }), "time_invalid"),
        (json!({ "notify_daily_limit": 9 }), "limit_invalid"),
        (
            json!({ "notify_muted_kinds": ["reassurance"] }),
            "kinds_invalid",
        ),
    ] {
        let (status, bad) =
            send(&app, request(Method::PATCH, "/v1/me", &cookie, Some(&body))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(bad["error"]["code"], code);
    }

    // Push: the key is derived from the token key; unknown hosts are refused.
    let (status, key) = send(&app, request(Method::GET, "/v1/push/key", &cookie, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["public_key"].as_str().unwrap().len(), 87);
    let sub = json!({ "endpoint": "https://evil.example/push", "keys": {
        "p256dh": "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        "auth": "BTBZMqHH6r4Tts7J_aSIgg" } });
    let (status, bad) = send(
        &app,
        request(Method::POST, "/v1/push/subscriptions", &cookie, Some(&sub)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"]["code"], "subscription_invalid");
    let mut ok = sub.clone();
    ok["endpoint"] = json!("https://fcm.googleapis.com/fcm/send/abc");
    let (status, _) = send(
        &app,
        request(Method::POST, "/v1/push/subscriptions", &cookie, Some(&ok)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // A check-in note, to see it decrypted in the export.
    send(
        &app,
        request(
            Method::POST,
            "/v1/check_ins",
            &cookie,
            Some(&json!({ "score": 4, "note": "fine" })),
        ),
    )
    .await;

    // Export: a zip with data.json and transactions.csv, no secrets.
    let (status, headers, bytes) = raw(
        &app,
        request(Method::POST, "/v1/exports", &cookie, Some(&json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/zip");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut data = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("data.json").unwrap(), &mut data).unwrap();
    let data: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(data["transactions"].as_array().unwrap().len(), 3);
    assert_eq!(data["check_ins"][0]["note"], "fine");
    assert!(data["connections"][0].get("access_token_enc").is_none());
    assert!(data["household"][0].get("dek_wrapped").is_none());
    assert!(data.get("sessions").is_none() && data.get("push_subscriptions").is_none());
    let mut csv = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("transactions.csv").unwrap(), &mut csv).unwrap();
    assert_eq!(csv.lines().count(), 4);
    assert!(csv.starts_with("date,account,amount_minor"));

    // Delete: everything goes, the cookie is cleared, the session ends.
    let (status, headers, _) = raw(&app, request(Method::DELETE, "/v1/me", &cookie, None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        headers["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let (status, _) = send(&app, request(Method::GET, "/v1/me", &cookie, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for table in [
        "households",
        "users",
        "transactions",
        "transactions_raw",
        "check_ins",
        "push_subscriptions",
        "categories",
    ] {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM app.{table}"
        )))
        .fetch_one(&admin)
        .await
        .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    let audit: Vec<String> = sqlx::query_scalar("SELECT action FROM app.audit_log")
        .fetch_all(&admin)
        .await
        .unwrap();
    assert_eq!(audit, ["household.deleted"]);
}
