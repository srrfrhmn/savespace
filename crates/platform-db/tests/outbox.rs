//! Outbox relay tests against a real PostgreSQL 18.
//!   cargo nextest run -p platform-db --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use platform_db::PgPool;
use platform_db::outbox::{self, Subscription};
use uuid::Uuid;

const SUBS: &[Subscription] = &[
    Subscription {
        topic: "t.changed",
        job_kind: "t.enrich",
    },
    Subscription {
        topic: "t.changed",
        job_kind: "t.insights",
    },
];

async fn event(pool: &PgPool, topic: &str, household: Uuid) {
    sqlx::query("INSERT INTO app.outbox (topic, household_id, payload) VALUES ($1, $2, '{}')")
        .bind(topic)
        .bind(household)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn relays_events_to_one_job_per_household_and_kind(pool: PgPool) {
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    event(&pool, "t.changed", a).await;
    event(&pool, "t.changed", a).await;
    event(&pool, "t.changed", b).await;
    event(&pool, "t.other", a).await;

    assert_eq!(outbox::relay(&pool, SUBS).await.unwrap(), 4);
    let jobs: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, payload->>'household_id' FROM app.jobs ORDER BY kind, 2")
            .fetch_all(&pool)
            .await
            .unwrap();
    let mut expected = vec![
        ("t.enrich".to_owned(), a.to_string()),
        ("t.enrich".to_owned(), b.to_string()),
        ("t.insights".to_owned(), a.to_string()),
        ("t.insights".to_owned(), b.to_string()),
    ];
    expected.sort();
    assert_eq!(jobs, expected);

    // Published events are not relayed again.
    assert_eq!(outbox::relay(&pool, SUBS).await.unwrap(), 0);
    let unpublished: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app.outbox WHERE published_at IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(unpublished, 0);
}
