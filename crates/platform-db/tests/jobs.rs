//! Job queue tests against a real PostgreSQL 18.
//!
//! These tests need `DATABASE_URL` and a role that can create databases.
//! Each test gets a new database with all migrations applied.
//! Run them with:
//!   cargo nextest run -p platform-db --run-ignored only

use std::time::Duration;

use platform_db::PgPool;
use platform_db::jobs::{self, FailOutcome, NewJob};
use serde_json::json;

const LEASE: Duration = Duration::from_secs(60);

#[sqlx::test]
#[ignore = "needs DATABASE_URL"]
async fn claim_then_complete(pool: PgPool) {
    let id = jobs::enqueue(&pool, &NewJob::new("t.ok", json!({"n": 1})))
        .await
        .unwrap()
        .unwrap();

    let job = jobs::claim(&pool, &["t.ok"], "w1", LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.id, id);
    assert_eq!(job.attempts, 1);
    assert_eq!(job.payload, json!({"n": 1}));

    // A claimed job is not claimed again.
    assert!(
        jobs::claim(&pool, &["t.ok"], "w2", LEASE)
            .await
            .unwrap()
            .is_none()
    );
    // Only the lease holder can complete it.
    assert!(!jobs::complete(&pool, id, "w2").await.unwrap());
    assert!(jobs::complete(&pool, id, "w1").await.unwrap());
}

#[sqlx::test]
#[ignore = "needs DATABASE_URL"]
async fn claim_skips_unknown_kinds_and_future_jobs(pool: PgPool) {
    jobs::enqueue(&pool, &NewJob::new("t.other", json!({})))
        .await
        .unwrap();
    let mut later = NewJob::new("t.ok", json!({}));
    later.delay = Duration::from_secs(3600);
    jobs::enqueue(&pool, &later).await.unwrap();

    assert!(
        jobs::claim(&pool, &["t.ok"], "w1", LEASE)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        jobs::claim(&pool, &[], "w1", LEASE)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test]
#[ignore = "needs DATABASE_URL"]
async fn dedupe_key_allows_one_live_job(pool: PgPool) {
    let mut job = NewJob::new("t.sync", json!({}));
    job.dedupe_key = Some("conn-1");

    let first = jobs::enqueue(&pool, &job).await.unwrap();
    assert!(first.is_some());
    assert!(jobs::enqueue(&pool, &job).await.unwrap().is_none());

    // After the job finishes, the same key is allowed again.
    let claimed = jobs::claim(&pool, &["t.sync"], "w1", LEASE)
        .await
        .unwrap()
        .unwrap();
    jobs::complete(&pool, claimed.id, "w1").await.unwrap();
    assert!(jobs::enqueue(&pool, &job).await.unwrap().is_some());
}

#[sqlx::test]
#[ignore = "needs DATABASE_URL"]
async fn fail_retries_with_backoff_then_dead(pool: PgPool) {
    jobs::enqueue(&pool, &NewJob::new("t.fail", json!({})))
        .await
        .unwrap();
    sqlx::query("UPDATE app.jobs SET max_attempts = 2")
        .execute(&pool)
        .await
        .unwrap();

    let job = jobs::claim(&pool, &["t.fail"], "w1", LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        jobs::fail(&pool, job.id, "w1", "boom").await.unwrap(),
        FailOutcome::Retrying
    );

    // The backoff for attempt 1 is 2.5 s to 5 s.
    let delay: f64 =
        sqlx::query_scalar("SELECT extract(epoch FROM run_after - now())::float8 FROM app.jobs")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!((2.0..=5.0).contains(&delay), "delay {delay}");

    // Make it due now, then fail the last attempt.
    sqlx::query("UPDATE app.jobs SET run_after = now()")
        .execute(&pool)
        .await
        .unwrap();
    let job = jobs::claim(&pool, &["t.fail"], "w1", LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.attempts, 2);
    assert_eq!(
        jobs::fail(&pool, job.id, "w1", "boom").await.unwrap(),
        FailOutcome::Dead
    );
    assert_eq!(
        jobs::fail(&pool, job.id, "w1", "boom").await.unwrap(),
        FailOutcome::LeaseLost
    );
}

#[sqlx::test]
#[ignore = "needs DATABASE_URL"]
async fn expired_lease_is_requeued(pool: PgPool) {
    jobs::enqueue(&pool, &NewJob::new("t.lease", json!({})))
        .await
        .unwrap();
    let job = jobs::claim(&pool, &["t.lease"], "w1", LEASE)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE app.jobs SET locked_until = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(jobs::requeue_expired(&pool).await.unwrap(), 1);
    // The old lease holder cannot complete it now.
    assert!(!jobs::complete(&pool, job.id, "w1").await.unwrap());
    let again = jobs::claim(&pool, &["t.lease"], "w2", LEASE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.id, job.id);
    assert_eq!(again.attempts, 2);
}

#[sqlx::test]
#[ignore = "needs DATABASE_URL"]
async fn runtime_role_can_use_tables_but_not_change_schema(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE savespace_runtime")
        .execute(&mut *tx)
        .await
        .unwrap();
    jobs::enqueue(&mut *tx, &NewJob::new("t.role", json!({})))
        .await
        .unwrap();
    let denied = sqlx::query("DROP TABLE app.jobs").execute(&mut *tx).await;
    assert!(denied.is_err(), "runtime role must not drop tables");
}
