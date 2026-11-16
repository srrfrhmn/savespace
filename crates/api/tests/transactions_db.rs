//! HTTP tests for transactions, categories, and rules, with a real
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
async fn edit_split_and_rule_flow(admin: PgPool) {
    seed(&admin).await;
    let app = app(&admin).await;
    let (status, _) = send(&app, request(Method::GET, "/v1/transactions", "", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let cookie = sign_in(&app).await;

    let (status, categories) =
        send(&app, request(Method::GET, "/v1/categories", &cookie, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(categories["data"].as_array().unwrap().len(), 23);
    let groceries = category_id(&categories, "Groceries");
    let home = category_id(&categories, "Home");
    let dining = category_id(&categories, "Restaurants and coffee");

    let (status, list) = send(
        &app,
        request(Method::GET, "/v1/transactions", &cookie, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = list["data"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let (t1, t3) = (
        rows[0]["id"].as_str().unwrap(),
        rows[2]["id"].as_str().unwrap(),
    );
    assert_eq!(rows[0]["merchant_name"], "Blue Bottle");
    assert_eq!(rows[0]["merchant_id"], rows[1]["merchant_id"]);
    assert_eq!(rows[0]["category_id"], dining.as_str());
    assert_eq!(rows[0]["category_source"], "provider");
    assert_eq!(rows[2]["category_id"], Value::Null);
    let merchant = rows[0]["merchant_id"].as_str().unwrap().to_owned();
    assert!(merchant.starts_with("mer_"));

    // Filters.
    let (_, none) = send(
        &app,
        request(Method::GET, "/v1/transactions?category=none", &cookie, None),
    )
    .await;
    assert_eq!(none["data"].as_array().unwrap().len(), 1);
    let (_, found) = send(
        &app,
        request(
            Method::GET,
            "/v1/transactions?q=target&limit=1",
            &cookie,
            None,
        ),
    )
    .await;
    assert_eq!(found["data"][0]["id"], t3);
    assert_eq!(found["has_more"], false);
    let (status, bad) = send(
        &app,
        request(
            Method::GET,
            "/v1/transactions?from=11/01/2026",
            &cookie,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"]["code"], "date_invalid");

    // The person's edit wins. Unknown fields are an error.
    let uri = format!("/v1/transactions/{t1}");
    let (status, t) = send(
        &app,
        request(
            Method::PATCH,
            &uri,
            &cookie,
            Some(&json!({ "category_id": groceries, "note": "beans" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["category_source"], "person");
    assert_eq!(t["note"], "beans");
    let (status, bad) = send(
        &app,
        request(
            Method::PATCH,
            &uri,
            &cookie,
            Some(&json!({ "category": groceries })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    let (status, bad) = send(
        &app,
        request(
            Method::PATCH,
            &uri,
            &cookie,
            Some(&json!({ "category_id": "cat_00000000000000000000000000000000" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"]["code"], "category_invalid");

    // A rule for the merchant: both Blue Bottle purchases, but t1 keeps the
    // person's category.
    let (status, rule) = send(
        &app,
        request(
            Method::POST,
            "/v1/rules",
            &cookie,
            Some(&json!({ "match": { "merchant_id": merchant }, "category_id": home })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{rule}");
    assert_eq!(rule["transactions"], 1, "t1 keeps the person's category");
    assert_eq!(rule["merchant_name"], "Blue Bottle");
    assert!(rule["id"].as_str().unwrap().starts_with("rule_"));
    let (_, t) = send(&app, request(Method::GET, &uri, &cookie, None)).await;
    assert_eq!(t["category_id"], groceries.as_str());
    let (_, list) = send(
        &app,
        request(Method::GET, "/v1/transactions", &cookie, None),
    )
    .await;
    assert_eq!(list["data"][1]["category_source"], "rule");
    let (status, bad) = send(
        &app,
        request(
            Method::POST,
            "/v1/rules",
            &cookie,
            Some(&json!({ "match": {}, "category_id": home })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"]["code"], "rule_invalid");

    // Splits.
    let splits = format!("/v1/transactions/{t3}/splits");
    let body = |a: i64, b: i64| {
        json!({ "splits": [
            { "category_id": groceries, "amount_minor": a },
            { "category_id": home, "amount_minor": b }
        ]})
    };
    let (status, bad) = send(
        &app,
        request(Method::PUT, &splits, &cookie, Some(&body(-6000, -3000))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(bad["error"]["code"], "splits_total");
    let (status, t) = send(
        &app,
        request(Method::PUT, &splits, &cookie, Some(&body(-6000, -4000))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["splits"].as_array().unwrap().len(), 2);
    assert!(t["splits"][0]["id"].as_str().unwrap().starts_with("spl_"));

    // Rules list and delete.
    let (_, rules) = send(&app, request(Method::GET, "/v1/rules", &cookie, None)).await;
    let rule_id = rules["data"][0]["id"].as_str().unwrap().to_owned();
    let uri = format!("/v1/rules/{rule_id}");
    let (status, _) = send(&app, request(Method::DELETE, &uri, &cookie, None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, request(Method::DELETE, &uri, &cookie, None)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = send(
        &app,
        request(Method::GET, "/v1/transactions", &cookie, None),
    )
    .await;
    assert_eq!(list["data"][1]["category_source"], "provider");

    // A wrong ID prefix is a 404.
    let (status, _) = send(
        &app,
        request(
            Method::GET,
            "/v1/transactions/acct_00000000000000000000000000000000",
            &cookie,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
