//! Insight pipeline tests against a real PostgreSQL 18.
//!
//! The services connect as `savespace_runtime`, so RLS applies. Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use base64::Engine;
use domain_insights::checkin::CheckInService;
use domain_insights::service::{Feedback, Reason, View};
use domain_insights::{InsightError, InsightService, Kind};
use jiff::ToSpan;
use platform_crypto::{Kek, Keyring};
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
    user: Uuid,
    connection: Uuid,
    account: Uuid,
}

/// An owner in New York with a checking account holding `cash`.
async fn household(admin: &PgPool, cash: i64) -> Home {
    let (household, user) = (Uuid::now_v7(), Uuid::now_v7());
    // Like app.create_owner, which allows one owner only.
    sqlx::query(
        "WITH h AS (INSERT INTO app.households (id) VALUES ($1) RETURNING id), \
         u AS (INSERT INTO app.users (id, display_name, time_zone) \
               VALUES ($2, 'Sam', 'America/New_York') RETURNING id) \
         INSERT INTO app.household_members (household_id, user_id, role) \
         SELECT h.id, u.id, 'owner' FROM h, u",
    )
    .bind(household)
    .bind(user)
    .execute(admin)
    .await
    .unwrap();
    let (connection, account): (Uuid, Uuid) = sqlx::query_as(
        "WITH c AS (INSERT INTO app.connections (household_id, provider, provider_item_id, \
           access_token_enc) VALUES ($1, 'plaid', $2, '\\x00') RETURNING id) \
         INSERT INTO app.accounts (household_id, connection_id, provider_account_id, name, \
           type, currency, current_minor) SELECT $1, id, 'a1', 'Checking', 'depository', 'USD', $3 \
         FROM c RETURNING connection_id, id",
    )
    .bind(household)
    .bind(household.to_string())
    .bind(cash)
    .fetch_one(admin)
    .await
    .unwrap();
    Home {
        household,
        user,
        connection,
        account,
    }
}

fn today() -> jiff::civil::Date {
    jiff::ZonedDateTime::now()
        .with_time_zone(jiff::tz::TimeZone::get("America/New_York").unwrap())
        .date()
}

/// A monthly rent series due in 2 days, with its last charge posted today.
async fn rent(admin: &PgPool, h: Home, amount: i64) -> Uuid {
    let merchant: Uuid = sqlx::query_scalar(
        "INSERT INTO app.merchants (household_id, key, name, from_provider) \
         VALUES ($1, 'oak apartments', 'Oak Apartments', true) RETURNING id",
    )
    .bind(h.household)
    .fetch_one(admin)
    .await
    .unwrap();
    let next = today().checked_add(2.days()).unwrap();
    let series: Uuid = sqlx::query_scalar(
        "INSERT INTO app.recurring_series (household_id, merchant_id, direction, currency, \
           cadence, amount_minor, spread_minor, last_date, next_date, occurrences, \
           confidence_bp, active) \
         VALUES ($1, $2, 'out', 'USD', 'monthly', $3, 0, $4::date - 30, $4::date, 6, 9500, true) \
         RETURNING id",
    )
    .bind(h.household)
    .bind(merchant)
    .bind(-amount)
    .bind(next.to_string())
    .fetch_one(admin)
    .await
    .unwrap();
    series
}

async fn insight_count(admin: &PgPool, status: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM app.insights WHERE status = $1")
        .bind(status)
        .fetch_one(admin)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn generates_dedupes_and_takes_feedback(admin: PgPool) {
    let svc = InsightService::new(runtime_pool(&admin).await);
    let h = household(&admin, 300_000).await;
    rent(&admin, h, 180_000).await;

    let out = svc.generate(h.household, None).await.unwrap();
    assert_eq!((out.created, out.refreshed, out.expired), (1, 0, 0));
    let list = svc.list(h.household, View::Home).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].kind, Kind::Anticipation);
    assert!(
        list[0]
            .text
            .starts_with("Oak Apartments ($1,800) is expected on ")
    );
    assert!(list[0].text.ends_with(". Your cash covers it."));
    assert_eq!(list[0].facts[0]["value"], "$1,800");

    // Run again: the same insight is refreshed, not added.
    let out = svc.generate(h.household, None).await.unwrap();
    assert_eq!((out.created, out.refreshed), (0, 1));

    // Cash drops below the bill: a concern appears. An overdraft limit in
    // "available" does not hide it.
    sqlx::query("UPDATE app.accounts SET current_minor = 100000, available_minor = 600000")
        .execute(&admin)
        .await
        .unwrap();
    svc.generate(h.household, None).await.unwrap();
    let list = svc.list(h.household, View::Home).await.unwrap();
    assert_eq!(list[0].kind, Kind::Concern, "concerns come first");
    assert!(
        list[0]
            .text
            .starts_with("Your cash may not cover Oak Apartments ($1,800) on ")
    );
    assert_eq!(list[1].kind, Kind::Anticipation);
    assert!(
        !list[1].text.contains("covers it"),
        "the text was refreshed"
    );

    // "This is wrong" hides the insight at once and records the reason.
    svc.feedback(h.household, list[0].id, Feedback::Wrong(Reason::Amount))
        .await
        .unwrap();
    assert_eq!(svc.list(h.household, View::Home).await.unwrap().len(), 1);
    let reason: String = sqlx::query_scalar("SELECT reason FROM app.insight_feedback")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(reason, "amount");
    // Not shown again within 7 days for the same scope.
    svc.generate(h.household, None).await.unwrap();
    assert_eq!(svc.list(h.household, View::Home).await.unwrap().len(), 1);

    // Another household sees and changes nothing.
    let other = household(&admin, 0).await;
    assert!(
        svc.list(other.household, View::Home)
            .await
            .unwrap()
            .is_empty()
    );
    let mine = svc.list(h.household, View::Home).await.unwrap()[0].id;
    assert!(matches!(
        svc.feedback(other.household, mine, Feedback::NotHelpful)
            .await,
        Err(InsightError::NotFound)
    ));
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn three_not_helpful_mute_a_rule_and_kill_switches_work(admin: PgPool) {
    let svc = InsightService::new(runtime_pool(&admin).await);
    let h = household(&admin, 300_000).await;
    rent(&admin, h, 180_000).await;

    for i in 0..3 {
        svc.generate(h.household, None).await.unwrap();
        let id = svc.list(h.household, View::Home).await.unwrap()[0].id;
        let muted = svc
            .feedback(h.household, id, Feedback::NotHelpful)
            .await
            .unwrap();
        assert_eq!(muted, i == 2);
        // A new scope each time, so the 7-day dedupe does not block it.
        sqlx::query("UPDATE app.insights SET scope = scope || ':old', created_at = created_at - interval '8 days'")
            .execute(&admin)
            .await
            .unwrap();
    }
    assert_eq!(
        svc.mutes(h.household).await.unwrap(),
        ["recurring.upcoming"]
    );
    svc.generate(h.household, None).await.unwrap();
    assert!(
        svc.list(h.household, View::Home).await.unwrap().is_empty(),
        "muted"
    );

    svc.unmute(h.household, "recurring.upcoming").await.unwrap();
    svc.generate(h.household, None).await.unwrap();
    assert_eq!(svc.list(h.household, View::Home).await.unwrap().len(), 1);

    // The kill switch expires open insights of the rule at the next run.
    sqlx::query("INSERT INTO app.insight_rule_flags (rule_id, enabled) VALUES ('recurring.upcoming', false)")
        .execute(&admin)
        .await
        .unwrap();
    let out = svc.generate(h.household, None).await.unwrap();
    assert_eq!(out.expired, 1);
    assert!(svc.list(h.household, View::Home).await.unwrap().is_empty());
    assert!(insight_count(&admin, "expired").await >= 1);

    // The runtime role cannot change the kill switches.
    let pool = runtime_pool(&admin).await;
    assert!(
        sqlx::query("UPDATE app.insight_rule_flags SET enabled = true")
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn transaction_insights_and_the_reflection(admin: PgPool) {
    let svc = InsightService::new(runtime_pool(&admin).await);
    let h = household(&admin, 300_000).await;
    let series = rent(&admin, h, 1549).await;
    // Two Netflix-like charges a month apart, the latest today.
    let mut ids = Vec::new();
    for back in [30, 0] {
        let date = today().checked_sub(back.days()).unwrap();
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO app.transactions (household_id, connection_id, account_id, \
               provider_transaction_id, raw_version, amount_minor, currency, date, name, \
               pending, recurring_series_id, category_id) \
             VALUES ($1, $2, $3, $4, 1, -1549, 'USD', $5::date, 'Rent', false, $6, \
               (SELECT id FROM app.categories WHERE household_id = $1 AND key = 'housing')) \
             RETURNING id",
        )
        .bind(h.household)
        .bind(h.connection)
        .bind(h.account)
        .bind(Uuid::now_v7().to_string())
        .bind(date.to_string())
        .bind(series)
        .fetch_one(&admin)
        .await
        .unwrap();
        ids.push(id);
    }
    svc.generate(h.household, None).await.unwrap();
    let on_txn = svc
        .list(h.household, View::Transaction(ids[1]))
        .await
        .unwrap();
    assert_eq!(on_txn.len(), 1);
    assert_eq!(
        on_txn[0].text,
        "This charge is recurring and expected. Oak Apartments charges about $15.49 each month."
    );
    assert!(
        svc.list(h.household, View::Home)
            .await
            .unwrap()
            .iter()
            .all(|i| i.transaction_id.is_none()),
        "transaction insights stay off the home feed"
    );

    let (start, partial, cards) = svc.reflection(h.household, today()).await.unwrap();
    assert!(partial);
    assert!(start <= today());
    assert!(!cards.is_empty() && cards.len() <= 5);
    assert_ne!(
        cards.last().unwrap().tone,
        domain_insights::reflection::Tone::Higher
    );
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn check_in_notes_are_encrypted(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let kek =
        Kek::from_base64(&base64::engine::general_purpose::STANDARD.encode([3_u8; 32])).unwrap();
    let svc = CheckInService::new(pool.clone(), Some(Keyring::new(kek)));
    let h = household(&admin, 0).await;
    let saved = svc
        .create(h.household, h.user, 4, Some("paid off the card"))
        .await
        .unwrap();
    assert_eq!(saved.date, today());
    svc.create(h.household, h.user, 2, None).await.unwrap();

    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT note_enc FROM app.check_ins WHERE note_enc IS NOT NULL")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("paid off"));
    let list = svc.list(h.household, h.user, 30).await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1].note.as_deref(), Some("paid off the card"));
    assert_eq!((list[0].score, list[0].note.as_deref()), (2, None));

    // Without the key, notes are refused. Another household sees nothing.
    let no_key = CheckInService::new(pool, None);
    assert!(matches!(
        no_key.create(h.household, h.user, 3, Some("x")).await,
        Err(InsightError::NotesUnavailable)
    ));
    let other = household(&admin, 0).await;
    assert!(
        svc.list(other.household, h.user, 30)
            .await
            .unwrap()
            .is_empty()
    );
}
