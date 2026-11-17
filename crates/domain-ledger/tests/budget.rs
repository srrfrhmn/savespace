//! Budget tests against a real PostgreSQL 18.
//!
//! The service connects as `savespace_runtime`, so RLS applies. Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use domain_ledger::budget::{CategoryBudget, PlanChange};
use domain_ledger::{LedgerError, LedgerService, NewSplit};
use jiff::civil::{Date, date};
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
           type, currency, current_minor) \
         SELECT $1, id, 'a1', 'Checking', 'depository', 'USD', 100000 FROM c \
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

async fn category(admin: &PgPool, home: Home, key: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM app.categories WHERE household_id = $1 AND key = $2")
        .bind(home.household)
        .bind(key)
        .fetch_one(admin)
        .await
        .unwrap()
}

async fn txn(admin: &PgPool, home: Home, on: Date, amount: i64, key: Option<&str>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO app.transactions (household_id, connection_id, account_id, \
           provider_transaction_id, raw_version, amount_minor, currency, date, name, pending, \
           category_id, category_source) \
         VALUES ($1, $2, $3, $4, 1, $5, 'USD', $6::date, 'x', false, \
           (SELECT id FROM app.categories WHERE household_id = $1 AND key = $7), 'provider') \
         RETURNING id",
    )
    .bind(home.household)
    .bind(home.connection)
    .bind(home.account)
    .bind(Uuid::now_v7().to_string())
    .bind(amount)
    .bind(on.to_string())
    .bind(key)
    .fetch_one(admin)
    .await
    .unwrap()
}

fn plan(minor: i64) -> PlanChange {
    PlanChange {
        plan: Some(Some(minor)),
        rollover: None,
    }
}

fn find(list: &[CategoryBudget], id: Uuid) -> &CategoryBudget {
    list.iter().find(|c| c.category_id == id).unwrap()
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn plans_rollover_yearly_costs_and_splits(admin: PgPool) {
    let svc = LedgerService::new(runtime_pool(&admin).await);
    let h = household(&admin).await;
    let groceries = category(&admin, h, "groceries").await;
    let housing = category(&admin, h, "housing").await;
    let travel = category(&admin, h, "travel").await;
    let home = category(&admin, h, "home").await;
    let (march, april, may) = (date(2026, 3, 1), date(2026, 4, 1), date(2026, 5, 1));

    svc.set_plan(
        h.household,
        groceries,
        march,
        PlanChange {
            plan: Some(Some(40_000)),
            rollover: Some(true),
        },
    )
    .await
    .unwrap();
    svc.set_plan(h.household, housing, date(2026, 3, 15), plan(180_000))
        .await
        .unwrap();
    svc.set_plan(h.household, travel, date(2026, 1, 1), plan(120_000))
        .await
        .unwrap();
    svc.set_plan(h.household, groceries, may, plan(45_000))
        .await
        .unwrap();

    txn(&admin, h, date(2026, 3, 10), -30_000, Some("groceries")).await;
    txn(&admin, h, date(2026, 3, 1), -180_000, Some("housing")).await;
    txn(&admin, h, date(2026, 4, 9), -45_000, Some("groceries")).await;
    txn(&admin, h, date(2026, 4, 20), 2000, Some("groceries")).await;
    txn(&admin, h, date(2026, 4, 28), -90_000, Some("travel")).await;
    let split = txn(&admin, h, date(2026, 5, 3), -10_000, Some("shopping")).await;
    svc.set_splits(
        h.household,
        split,
        &[
            NewSplit {
                category_id: groceries,
                amount_minor: -6000,
            },
            NewSplit {
                category_id: home,
                amount_minor: -4000,
            },
        ],
    )
    .await
    .unwrap();
    let hidden = txn(&admin, h, date(2026, 5, 4), -99_999, Some("groceries")).await;
    sqlx::query("INSERT INTO app.user_overrides (transaction_id, household_id, hidden) VALUES ($1, $2, true)")
        .bind(hidden)
        .bind(h.household)
        .execute(&admin)
        .await
        .unwrap();
    txn(&admin, h, date(2026, 5, 15), 250_000, Some("income")).await;
    txn(&admin, h, date(2026, 5, 16), -1234, None).await;

    // March: the plan starts mid-month and counts for all of March.
    let m = svc.budget_month(h.household, march).await.unwrap();
    assert_eq!(m.month, march);
    let g = find(&m.categories, groceries);
    assert_eq!(
        (g.plan, g.carried, g.spent, g.left),
        (Some(40_000), 0, 30_000, Some(10_000))
    );
    assert_eq!(find(&m.categories, housing).left, Some(0));
    assert_eq!((m.fixed.planned, m.fixed.spent), (180_000, 180_000));
    assert!(
        m.calm.is_none(),
        "the calm state is for the current month only"
    );

    // April: 10,000 carried; a refund lowers spending; the yearly travel cost
    // is compared with what was set aside so far (4/12 of 120,000).
    let m = svc.budget_month(h.household, april).await.unwrap();
    let g = find(&m.categories, groceries);
    assert_eq!((g.carried, g.spent, g.left), (10_000, 43_000, Some(7000)));
    let t = find(&m.categories, travel);
    assert_eq!(
        (t.plan, t.spent, t.set_aside, t.spent_this_year, t.left),
        (
            Some(120_000),
            90_000,
            Some(40_000),
            Some(90_000),
            Some(-50_000)
        )
    );
    assert_eq!(
        (m.non_monthly.planned, m.non_monthly.spent),
        (40_000, 90_000)
    );

    // May: the new plan, 7,000 carried, the split part, no hidden spending.
    let m = svc.budget_month(h.household, may).await.unwrap();
    let g = find(&m.categories, groceries);
    assert_eq!(
        (g.plan, g.carried, g.spent, g.left),
        (Some(45_000), 7000, 6000, Some(46_000))
    );
    assert_eq!(find(&m.categories, home).spent, 4000);
    assert_eq!(find(&m.categories, home).left, None, "no plan");
    assert_eq!(m.income, 250_000);
    assert_eq!(m.uncategorized, 1234);
    assert_eq!(
        (m.flex.planned, m.flex.spent, m.flex.left),
        (45_000, 10_000, 46_000)
    );

    // Ending a plan, and rollover off.
    svc.set_plan(
        h.household,
        groceries,
        may,
        PlanChange {
            plan: Some(None),
            rollover: Some(false),
        },
    )
    .await
    .unwrap();
    let m = svc.budget_month(h.household, may).await.unwrap();
    let g = find(&m.categories, groceries);
    assert_eq!(
        (g.plan, g.carried, g.left, g.rollover),
        (None, 0, None, false)
    );

    // Errors.
    let income = category(&admin, h, "income").await;
    assert!(matches!(
        svc.set_plan(h.household, income, may, plan(1)).await,
        Err(LedgerError::Invalid {
            code: "category_not_budgeted",
            ..
        })
    ));
    assert!(matches!(
        svc.set_plan(
            h.household,
            travel,
            may,
            PlanChange {
                plan: None,
                rollover: Some(true)
            }
        )
        .await,
        Err(LedgerError::Invalid {
            code: "rollover_not_available",
            ..
        })
    ));
    let other = household(&admin).await;
    assert!(matches!(
        svc.set_plan(other.household, groceries, may, plan(1)).await,
        Err(LedgerError::NotFound)
    ));
    let theirs = svc.budget_month(other.household, may).await.unwrap();
    assert!(
        theirs
            .categories
            .iter()
            .all(|c| c.spent == 0 && c.plan.is_none())
    );
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn calm_state_for_the_current_month(admin: PgPool) {
    let svc = LedgerService::new(runtime_pool(&admin).await);
    let h = household(&admin).await;
    let groceries = category(&admin, h, "groceries").await;
    let today = jiff::ZonedDateTime::now()
        .with_time_zone(jiff::tz::TimeZone::get("America/New_York").unwrap())
        .date();
    let this_month = today.first_of_month();
    svc.set_plan(h.household, groceries, this_month, plan(40_000))
        .await
        .unwrap();
    let merchant: Uuid = sqlx::query_scalar(
        "INSERT INTO app.merchants (household_id, key, name) VALUES ($1, 'rent', 'Rent') RETURNING id",
    )
    .bind(h.household)
    .fetch_one(&admin)
    .await
    .unwrap();
    // A rent bill due today; a low-confidence series does not count.
    sqlx::query(
        "INSERT INTO app.recurring_series (household_id, merchant_id, direction, currency, \
           cadence, amount_minor, spread_minor, last_date, next_date, occurrences, \
           confidence_bp, active) VALUES \
           ($1, $2, 'out', 'USD', 'monthly', -50000, 0, $3::date - 30, $3::date, 6, 9500, true)",
    )
    .bind(h.household)
    .bind(merchant)
    .bind(today.to_string())
    .execute(&admin)
    .await
    .unwrap();

    let state = |cash: i64| {
        let svc = svc.clone();
        let admin = admin.clone();
        async move {
            sqlx::query("UPDATE app.accounts SET current_minor = $1")
                .bind(cash)
                .execute(&admin)
                .await
                .unwrap();
            svc.budget_month(h.household, this_month)
                .await
                .unwrap()
                .calm
                .unwrap()
        }
    };
    let calm = state(100_000).await;
    assert_eq!(
        (calm.state, calm.cash, calm.bills, calm.flex_left),
        ("covered", 100_000, 50_000, 40_000)
    );
    assert_eq!(calm.until, this_month.last_of_month());
    assert_eq!(state(80_000).await.state, "tight");
    assert_eq!(state(40_000).await.state, "needs_attention");

    // "Available" with a $500 overdraft is not cash; a pending payment that
    // lowers "available" is.
    let with_available = |current: i64, available: Option<i64>| {
        let svc = svc.clone();
        let admin = admin.clone();
        async move {
            sqlx::query("UPDATE app.accounts SET current_minor = $1, available_minor = $2")
                .bind(current)
                .bind(available)
                .execute(&admin)
                .await
                .unwrap();
            svc.budget_month(h.household, this_month)
                .await
                .unwrap()
                .calm
                .unwrap()
                .cash
        }
    };
    assert_eq!(with_available(341_973, Some(391_973)).await, 341_973);
    assert_eq!(with_available(341_973, Some(300_000)).await, 300_000);
    assert_eq!(with_available(341_973, None).await, 341_973);
}
