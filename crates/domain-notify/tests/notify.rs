//! Notification tests against a real PostgreSQL 18 and a mock push service.
//!
//! The service connects as `savespace_runtime`, so RLS applies. Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use domain_notify::webpush::{Keys, Vapid};
use domain_notify::{NotifyError, NotifyService};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use uuid::Uuid;
use wiremock::matchers::{header, header_exists, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

/// An owner whose quiet hours never match now (00:00–00:00 is off).
async fn household(admin: &PgPool) -> (Uuid, Uuid) {
    let (household, user) = (Uuid::now_v7(), Uuid::now_v7());
    sqlx::query(
        "WITH h AS (INSERT INTO app.households (id) VALUES ($1) RETURNING id), \
         u AS (INSERT INTO app.users (id, display_name, quiet_start, quiet_end) \
               VALUES ($2, 'Sam', '00:00', '00:00') RETURNING id) \
         INSERT INTO app.household_members (household_id, user_id, role) \
         SELECT h.id, u.id, 'owner' FROM h, u",
    )
    .bind(household)
    .bind(user)
    .execute(admin)
    .await
    .unwrap();
    (household, user)
}

async fn insight(admin: &PgPool, household: Uuid, kind: &str, text: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO app.insights (household_id, rule_id, rule_version, kind, scope, text, \
           facts, confidence_bp, relevance_bp) \
         VALUES ($1, 'test.rule', 1, $2, $3, $4, '[]', 9000, 9000) RETURNING id",
    )
    .bind(household)
    .bind(kind)
    .bind(Uuid::now_v7().to_string())
    .bind(text)
    .fetch_one(admin)
    .await
    .unwrap()
}

/// Browser keys for a subscription. Only the public half is needed.
fn browser_keys() -> Keys {
    Keys::from_base64url(
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        "BTBZMqHH6r4Tts7J_aSIgg",
    )
    .unwrap()
}

async fn statuses(admin: &PgPool) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as("SELECT kind, status, reason FROM app.notifications ORDER BY kind")
        .fetch_all(admin)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn queues_applies_the_policy_and_pushes(admin: PgPool) {
    let push = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("Content-Encoding", "aes128gcm"))
        .and(header("TTL", "86400"))
        .and(header_exists("Authorization"))
        .respond_with(ResponseTemplate::new(201))
        .expect(2)
        .mount(&push)
        .await;
    let vapid = Vapid::derive(&[4_u8; 32], "https://app.example.test");
    let svc =
        NotifyService::new(runtime_pool(&admin).await, Some(vapid)).with_any_endpoint_for_tests();
    let (h, user) = household(&admin).await;

    // Without a subscription: skipped, not lost silently.
    insight(
        &admin,
        h,
        "anticipation",
        "Rent ($1,800) is expected on Dec 1. Your cash covers it.",
    )
    .await;
    assert_eq!(svc.queue_from_insights(h).await.unwrap(), 1);
    assert_eq!(
        svc.queue_from_insights(h).await.unwrap(),
        0,
        "one notification per insight"
    );
    let out = svc.send_due(h).await.unwrap();
    assert_eq!((out.sent, out.dropped), (0, 1));
    assert_eq!(
        statuses(&admin).await[0].2.as_deref(),
        Some("no_subscription")
    );

    // Subscribe. Then a concern, an awareness, and a reassurance. The earlier
    // anticipation is queued again too. A reassurance never notifies.
    let endpoint = format!("{}/push/abc", push.uri());
    svc.subscribe(h, user, &endpoint, &browser_keys())
        .await
        .unwrap();
    sqlx::query("DELETE FROM app.notifications")
        .execute(&admin)
        .await
        .unwrap();
    insight(
        &admin,
        h,
        "concern",
        "Your cash may not cover Rent ($1,800) on Dec 1.",
    )
    .await;
    insight(
        &admin,
        h,
        "awareness",
        "This Netflix charge is $7.50 higher than usual.",
    )
    .await;
    insight(&admin, h, "reassurance", "You are on track.").await;
    assert_eq!(
        svc.queue_from_insights(h).await.unwrap(),
        3,
        "reassurance is not queued"
    );
    // The concern always sends and uses the day's one notification. The
    // awareness and the earlier anticipation (queued again) wait for another
    // day.
    let out = svc.send_due(h).await.unwrap();
    assert_eq!(out.sent, 1);
    let s = statuses(&admin).await;
    assert!(s.iter().any(|r| r.0 == "concern" && r.1 == "sent"));
    assert_eq!(
        s.iter()
            .filter(|r| r.2.as_deref() == Some("daily_limit"))
            .count(),
        2
    );

    // With a higher limit, a new non-concern insight sends.
    sqlx::query("UPDATE app.users SET notify_daily_limit = 3")
        .execute(&admin)
        .await
        .unwrap();
    insight(
        &admin,
        h,
        "awareness",
        "Flexible spending is higher than usual this week: $300 so far.",
    )
    .await;
    svc.queue_from_insights(h).await.unwrap();
    assert_eq!(svc.send_due(h).await.unwrap().sent, 1);
    push.verify().await;

    // A gone subscription (410) is removed.
    push.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(410))
        .mount(&push)
        .await;
    assert_eq!(svc.send_test(h, user).await.unwrap(), 0);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM app.push_subscriptions")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn quiet_hours_delay_and_subscriptions_stay_in_their_household(admin: PgPool) {
    let vapid = Vapid::derive(&[4_u8; 32], "https://app.example.test");
    let svc = NotifyService::new(runtime_pool(&admin).await, Some(vapid));
    let (h, user) = household(&admin).await;
    let endpoint = "https://fcm.googleapis.com/fcm/send/test-endpoint";
    svc.subscribe(h, user, endpoint, &browser_keys())
        .await
        .unwrap();

    // Quiet all day except one minute that already passed: now is quiet,
    // and the send waits until quiet hours end.
    sqlx::query(
        "UPDATE app.users SET \
           quiet_start = (now() AT TIME ZONE time_zone)::time - interval '1 minute', \
           quiet_end = (now() AT TIME ZONE time_zone)::time - interval '2 minutes'",
    )
    .execute(&admin)
    .await
    .unwrap();
    insight(
        &admin,
        h,
        "anticipation",
        "Rent ($1,800) is expected on Dec 1.",
    )
    .await;
    svc.queue_from_insights(h).await.unwrap();
    let out = svc.send_due(h).await.unwrap();
    assert_eq!((out.sent, out.delayed), (0, 1));
    let later: bool = sqlx::query_scalar(
        "SELECT scheduled_for > now() FROM app.notifications WHERE status = 'queued'",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert!(later);
    assert!(
        svc.households_due().await.unwrap().is_empty(),
        "nothing due until quiet hours end"
    );

    // Unknown push hosts are refused (SSRF).
    assert!(matches!(
        svc.subscribe(h, user, "https://169.254.169.254/x", &browser_keys())
            .await,
        Err(NotifyError::InvalidSubscription)
    ));
    // Another household cannot take over this browser's endpoint.
    let (other, other_user) = household(&admin).await;
    assert!(matches!(
        svc.subscribe(other, other_user, endpoint, &browser_keys())
            .await,
        Err(NotifyError::Conflict)
    ));
    svc.unsubscribe(other, endpoint).await.unwrap();
    let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM app.push_subscriptions")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(kept, 1, "the other household cannot remove it");
}
