//! Recurring detection tests against a real PostgreSQL 18.
//!
//! The service connects as `savespace_runtime`, so RLS applies. Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use domain_recurring::RecurringService;
use jiff::ToSpan;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use uuid::Uuid;

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

#[derive(Clone, Copy)]
struct Home {
    household: Uuid,
    connection: Uuid,
    account: Uuid,
}

async fn household(admin: &PgPool) -> Home {
    let household = Uuid::now_v7();
    sqlx::query("INSERT INTO app.households (id) VALUES ($1)")
        .bind(household)
        .execute(admin)
        .await
        .unwrap();
    let (connection, account): (Uuid, Uuid) = sqlx::query_as(
        "WITH c AS (INSERT INTO app.connections (household_id, provider, provider_item_id, \
           access_token_enc) VALUES ($1, 'plaid', $2, '\\x00') RETURNING id) \
         INSERT INTO app.accounts (household_id, connection_id, provider_account_id, name, \
           type, currency) SELECT $1, id, 'a1', 'Checking', 'depository', 'USD' FROM c \
         RETURNING connection_id, id",
    )
    .bind(household)
    .bind(household.to_string())
    .fetch_one(admin)
    .await
    .unwrap();
    Home {
        household,
        connection,
        account,
    }
}

async fn merchant(admin: &PgPool, home: Home, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO app.merchants (household_id, key, name, from_provider) \
         VALUES ($1, lower($2), $2, true) RETURNING id",
    )
    .bind(home.household)
    .bind(name)
    .fetch_one(admin)
    .await
    .unwrap()
}

/// Adds a posted transaction `days_ago` days before today.
async fn txn(
    admin: &PgPool,
    home: Home,
    merchant: Uuid,
    days_ago: i32,
    amount: i64,
    category: Option<&str>,
) -> Uuid {
    let date = jiff::ZonedDateTime::now()
        .date()
        .checked_sub(days_ago.days())
        .unwrap();
    sqlx::query_scalar(
        "INSERT INTO app.transactions (household_id, connection_id, account_id, \
           provider_transaction_id, raw_version, amount_minor, currency, date, name, pending, \
           merchant_id, category_id, category_source, enriched_version) \
         VALUES ($1, $2, $3, $4, 1, $5, 'USD', $6::date, 'x', false, $7, \
           (SELECT id FROM app.categories WHERE household_id = $1 AND key = $8), \
           'provider', 1) RETURNING id",
    )
    .bind(home.household)
    .bind(home.connection)
    .bind(home.account)
    .bind(Uuid::now_v7().to_string())
    .bind(amount)
    .bind(date.to_string())
    .bind(merchant)
    .bind(category)
    .fetch_one(admin)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn finds_monthly_bills_and_paychecks_not_shopping(admin: PgPool) {
    let svc = RecurringService::new(runtime_pool(&admin).await);
    let h = household(&admin).await;
    let netflix = merchant(&admin, h, "Netflix").await;
    let employer = merchant(&admin, h, "Acme Payroll").await;
    let shop = merchant(&admin, h, "Corner Shop").await;
    let card = merchant(&admin, h, "Card Payment").await;
    let mut netflix_ids = Vec::new();
    for i in 0..6 {
        netflix_ids.push(
            txn(
                &admin,
                h,
                netflix,
                10 + 30 * i,
                -1549,
                Some("subscriptions"),
            )
            .await,
        );
        txn(&admin, h, card, 5 + 30 * i, -50_000, Some("transfer")).await;
    }
    for i in 0..8 {
        txn(&admin, h, employer, 3 + 14 * i, 250_000, Some("income")).await;
    }
    for (i, d) in [1, 4, 9, 22, 23, 40].into_iter().enumerate() {
        txn(
            &admin,
            h,
            shop,
            d,
            -1000 - 137 * i64::try_from(i).unwrap(),
            None,
        )
        .await;
    }
    // A hidden charge does not count.
    let hidden = txn(&admin, h, netflix, 2, -1549, Some("subscriptions")).await;
    sqlx::query("INSERT INTO app.user_overrides (transaction_id, household_id, hidden) VALUES ($1, $2, true)")
        .bind(hidden)
        .bind(h.household)
        .execute(&admin)
        .await
        .unwrap();

    assert_eq!(svc.detect_household(h.household).await.unwrap(), 2);
    let list = svc.list(h.household).await.unwrap();
    let by_name = |n: &str| list.iter().find(|s| s.merchant_name == n);
    let n = by_name("Netflix").unwrap();
    assert_eq!(
        (n.cadence.as_str(), n.direction.as_str(), n.amount_minor),
        ("monthly", "out", -1549)
    );
    assert!(n.expected && n.active);
    assert_eq!(n.occurrences, 6, "the hidden charge does not count");
    assert!(n.next_date >= jiff::ZonedDateTime::now().date());
    let p = by_name("Acme Payroll").unwrap();
    assert_eq!(
        (p.cadence.as_str(), p.direction.as_str()),
        ("biweekly", "in")
    );
    assert!(by_name("Corner Shop").is_none());
    assert!(
        by_name("Card Payment").is_none(),
        "transfers are not recurring charges"
    );

    let linked: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app.transactions WHERE recurring_series_id = $1")
            .bind(n.id)
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(linked, 6);

    // Run again: same series IDs. Remove Netflix: its series goes away.
    assert_eq!(svc.detect_household(h.household).await.unwrap(), 2);
    assert_eq!(
        svc.list(h.household)
            .await
            .unwrap()
            .iter()
            .find(|s| s.merchant_name == "Netflix")
            .unwrap()
            .id,
        n.id
    );
    sqlx::query("DELETE FROM app.transactions WHERE merchant_id = $1")
        .bind(netflix)
        .execute(&admin)
        .await
        .unwrap();
    assert_eq!(svc.detect_household(h.household).await.unwrap(), 1);

    // Another household sees nothing.
    let other = household(&admin).await;
    assert!(svc.list(other.household).await.unwrap().is_empty());
    assert_eq!(svc.detect_household(other.household).await.unwrap(), 0);
    assert_eq!(svc.list(h.household).await.unwrap().len(), 1);
}
