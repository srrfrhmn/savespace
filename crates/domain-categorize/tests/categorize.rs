//! Categorization tests against a real PostgreSQL 18.
//!
//! The service connects as `savespace_runtime`, so RLS applies. Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use domain_categorize::{CategorizeError, CategorizeService, Direction, RuleMatch, mapped_keys};
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

/// A household with one connection and one account.
async fn household(admin: &PgPool) -> (Uuid, Uuid, Uuid) {
    let household = Uuid::now_v7();
    sqlx::query("INSERT INTO app.households (id) VALUES ($1)")
        .bind(household)
        .execute(admin)
        .await
        .unwrap();
    let connection: Uuid = sqlx::query_scalar(
        "INSERT INTO app.connections (household_id, provider, provider_item_id, \
           access_token_enc) VALUES ($1, 'plaid', $2, '\\x00') RETURNING id",
    )
    .bind(household)
    .bind(household.to_string())
    .fetch_one(admin)
    .await
    .unwrap();
    let account: Uuid = sqlx::query_scalar(
        "INSERT INTO app.accounts (household_id, connection_id, provider_account_id, name, \
           type, currency) VALUES ($1, $2, 'a1', 'Checking', 'depository', 'USD') RETURNING id",
    )
    .bind(household)
    .bind(connection)
    .fetch_one(admin)
    .await
    .unwrap();
    (household, connection, account)
}

async fn add_txn(
    admin: &PgPool,
    (household, connection, account): (Uuid, Uuid, Uuid),
    name: &str,
    merchant: Option<&str>,
    amount: i64,
    category: (Option<&str>, Option<&str>),
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO app.transactions (household_id, connection_id, account_id, \
           provider_transaction_id, raw_version, amount_minor, currency, date, name, \
           merchant_name, pending, provider_category_primary, provider_category_detailed) \
         VALUES ($1, $2, $3, $4, 1, $5, 'USD', '2026-11-10', $6, $7, false, $8, $9) \
         RETURNING id",
    )
    .bind(household)
    .bind(connection)
    .bind(account)
    .bind(Uuid::now_v7().to_string())
    .bind(amount)
    .bind(name)
    .bind(merchant)
    .bind(category.0)
    .bind(category.1)
    .fetch_one(admin)
    .await
    .unwrap()
}

/// (merchant name, category key, source) of a transaction.
async fn result(admin: &PgPool, id: Uuid) -> (Option<String>, Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT m.name, c.key, t.category_source FROM app.transactions t \
         LEFT JOIN app.merchants m ON m.id = t.merchant_id \
         LEFT JOIN app.categories c ON c.id = t.category_id WHERE t.id = $1",
    )
    .bind(id)
    .fetch_one(admin)
    .await
    .unwrap()
}

async fn category(svc: &CategorizeService, household: Uuid, key: &str) -> Uuid {
    svc.list_categories(household)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.key.as_deref() == Some(key))
        .unwrap()
        .id
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn new_household_gets_every_mapped_category(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let (household, ..) = household(&admin).await;
    let svc = CategorizeService::new(pool);
    let categories = svc.list_categories(household).await.unwrap();
    assert_eq!(categories.len(), 23);
    assert_eq!(categories[0].key.as_deref(), Some("income"));
    for key in mapped_keys() {
        assert!(
            categories.iter().any(|c| c.key.as_deref() == Some(key)),
            "{key} is not seeded"
        );
    }
    assert!(
        categories
            .iter()
            .any(|c| c.key.as_deref() == Some("health") && c.sensitive)
    );
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn enrich_sets_merchant_and_provider_category(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let h = household(&admin).await;
    let svc = CategorizeService::new(pool);

    let coffee = add_txn(
        &admin,
        h,
        "SQ *BLUE BOTTLE #123",
        None,
        -450,
        (Some("FOOD_AND_DRINK"), Some("FOOD_AND_DRINK_COFFEE")),
    )
    .await;
    let coffee2 = add_txn(&admin, h, "sq*Blue Bottle #77", None, -500, (None, None)).await;
    let rent = add_txn(
        &admin,
        h,
        "ACH RENT",
        Some("Oak Apartments"),
        -180_000,
        (Some("RENT_AND_UTILITIES"), Some("RENT_AND_UTILITIES_RENT")),
    )
    .await;

    assert_eq!(svc.enrich_household(h.0).await.unwrap(), 3);
    assert_eq!(
        result(&admin, coffee).await,
        (
            Some("Blue Bottle".into()),
            Some("dining".into()),
            Some("provider".into())
        )
    );
    assert_eq!(
        result(&admin, coffee2).await,
        (Some("Blue Bottle".into()), None, None)
    );
    assert_eq!(
        result(&admin, rent).await,
        (
            Some("Oak Apartments".into()),
            Some("housing".into()),
            Some("provider".into())
        )
    );
    let merchants: i64 = sqlx::query_scalar("SELECT count(*) FROM app.merchants")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(merchants, 2);
    // Idempotent: nothing left to do.
    assert_eq!(svc.enrich_household(h.0).await.unwrap(), 0);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn rules_apply_to_past_transactions_and_win_over_the_provider(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let h = household(&admin).await;
    let svc = CategorizeService::new(pool);
    let a = add_txn(
        &admin,
        h,
        "SQ *BLUE BOTTLE #123",
        None,
        -450,
        (Some("FOOD_AND_DRINK"), None),
    )
    .await;
    let b = add_txn(&admin, h, "VENMO CASHOUT", None, 5000, (None, None)).await;
    let c = add_txn(&admin, h, "VENMO PAYMENT", None, -2000, (None, None)).await;
    svc.enrich_household(h.0).await.unwrap();

    let merchant: Uuid =
        sqlx::query_scalar("SELECT merchant_id FROM app.transactions WHERE id = $1")
            .bind(a)
            .fetch_one(&admin)
            .await
            .unwrap();
    let groceries = category(&svc, h.0, "groceries").await;
    let rule = svc
        .create_rule(
            h.0,
            RuleMatch {
                merchant_id: Some(merchant),
                ..RuleMatch::default()
            },
            groceries,
        )
        .await
        .unwrap();
    assert_eq!(rule.transactions, 1);
    assert_eq!(rule.merchant_name.as_deref(), Some("Blue Bottle"));
    assert_eq!(
        result(&admin, a).await.1.as_deref(),
        Some("groceries"),
        "the rule wins over the provider"
    );

    let income = category(&svc, h.0, "income").await;
    let venmo = svc
        .create_rule(
            h.0,
            RuleMatch {
                name_contains: Some("venmo".into()),
                direction: Some(Direction::In),
                ..RuleMatch::default()
            },
            income,
        )
        .await
        .unwrap();
    assert_eq!(venmo.transactions, 1);
    assert_eq!(result(&admin, b).await.2.as_deref(), Some("rule"));
    assert_eq!(result(&admin, c).await.1, None);
    let rules = svc.list_rules(h.0).await.unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(
        rules[0].id, venmo.id,
        "the newest rule has the highest priority"
    );

    // The same target again replaces the rule.
    let dining = category(&svc, h.0, "dining").await;
    svc.create_rule(
        h.0,
        RuleMatch {
            merchant_id: Some(merchant),
            ..RuleMatch::default()
        },
        dining,
    )
    .await
    .unwrap();
    assert_eq!(svc.list_rules(h.0).await.unwrap().len(), 2);
    assert_eq!(result(&admin, a).await.1.as_deref(), Some("dining"));

    // Deleting the rule gives the provider category back.
    let id = svc.list_rules(h.0).await.unwrap()[0].id;
    svc.delete_rule(h.0, id).await.unwrap();
    assert_eq!(result(&admin, a).await.2.as_deref(), Some("provider"));
    assert!(matches!(
        svc.delete_rule(h.0, id).await,
        Err(CategorizeError::NotFound)
    ));
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn rules_stay_in_their_household(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let a = household(&admin).await;
    let b = household(&admin).await;
    let svc = CategorizeService::new(pool);
    let t = add_txn(&admin, a, "SQ *BLUE BOTTLE", None, -450, (None, None)).await;
    svc.enrich_household(a.0).await.unwrap();
    let merchant_a: Uuid =
        sqlx::query_scalar("SELECT merchant_id FROM app.transactions WHERE id = $1")
            .bind(t)
            .fetch_one(&admin)
            .await
            .unwrap();
    let groceries_a = category(&svc, a.0, "groceries").await;
    let groceries_b = category(&svc, b.0, "groceries").await;

    // B cannot use A's category or A's merchant.
    let by_text = RuleMatch {
        name_contains: Some("bottle".into()),
        ..RuleMatch::default()
    };
    assert!(matches!(
        svc.create_rule(b.0, by_text.clone(), groceries_a).await,
        Err(CategorizeError::NotFound)
    ));
    assert!(matches!(
        svc.create_rule(
            b.0,
            RuleMatch {
                merchant_id: Some(merchant_a),
                ..RuleMatch::default()
            },
            groceries_b
        )
        .await,
        Err(CategorizeError::NotFound)
    ));

    // A rule in B does not touch A's transactions, and B cannot see or
    // delete A's rules.
    svc.create_rule(b.0, by_text.clone(), groceries_b)
        .await
        .unwrap();
    assert_eq!(result(&admin, t).await.1, None);
    let rule_a = svc.create_rule(a.0, by_text, groceries_a).await.unwrap();
    assert_eq!(svc.list_rules(b.0).await.unwrap().len(), 1);
    assert!(matches!(
        svc.delete_rule(b.0, rule_a.id).await,
        Err(CategorizeError::NotFound)
    ));
    assert_eq!(svc.list_categories(b.0).await.unwrap().len(), 23);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn a_description_joins_the_provider_merchant(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let h = household(&admin).await;
    let svc = CategorizeService::new(pool);
    // The provider names the merchant on one ride only. The cleaned
    // descriptions are "uber sf", which starts with the provider key "uber".
    let named = add_txn(
        &admin,
        h,
        "Uber 063015 SF**POOL**",
        Some("Uber"),
        -540,
        (None, None),
    )
    .await;
    let plain = add_txn(
        &admin,
        h,
        "Uber 072515 SF**POOL**",
        None,
        -650,
        (None, None),
    )
    .await;
    svc.enrich_household(h.0).await.unwrap();
    assert_eq!(result(&admin, named).await.0.as_deref(), Some("Uber"));
    assert_eq!(result(&admin, plain).await.0.as_deref(), Some("Uber"));

    // In a later batch too: the stored provider merchant claims it.
    let later = add_txn(
        &admin,
        h,
        "UBER 081115 SF**POOL**",
        None,
        -700,
        (None, None),
    )
    .await;
    svc.enrich_household(h.0).await.unwrap();
    assert_eq!(result(&admin, later).await.0.as_deref(), Some("Uber"));
    let merchants: Vec<(String, bool)> =
        sqlx::query_as("SELECT key, from_provider FROM app.merchants")
            .fetch_all(&admin)
            .await
            .unwrap();
    assert_eq!(merchants, [("uber".to_owned(), true)]);
}
