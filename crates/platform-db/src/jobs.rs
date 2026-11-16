//! The PostgreSQL job queue (design doc §8.6).
//!
//! - A worker claims one job with `FOR UPDATE SKIP LOCKED` and holds a lease.
//! - A failed job goes back to the queue with exponential backoff and jitter.
//!   After `max_attempts`, it goes to the `dead` state.
//! - A job whose lease expires (for example, the worker stopped) goes back to
//!   the queue. See [`requeue_expired`].
//!
//! Every job handler must be idempotent. A job can run more than once.

use std::time::Duration;

use sqlx::PgExecutor;
use uuid::Uuid;

/// A claimed job.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Job {
    /// The job ID.
    pub id: Uuid,
    /// The job kind, for example `sync.transactions`.
    pub kind: String,
    /// Handler input.
    pub payload: serde_json::Value,
    /// The number of claims so far, including this one.
    pub attempts: i32,
    /// The attempt limit.
    pub max_attempts: i32,
}

/// A job to add to the queue.
#[derive(Debug, Clone)]
pub struct NewJob<'a> {
    /// The job kind.
    pub kind: &'a str,
    /// Handler input.
    pub payload: serde_json::Value,
    /// If set, the queue keeps at most one queued or running job with this
    /// kind and key.
    pub dedupe_key: Option<&'a str>,
    /// The job does not run before this delay.
    pub delay: Duration,
}

impl<'a> NewJob<'a> {
    /// A job with no dedupe key and no delay.
    pub fn new(kind: &'a str, payload: serde_json::Value) -> Self {
        Self {
            kind,
            payload,
            dedupe_key: None,
            delay: Duration::ZERO,
        }
    }
}

/// The result of [`fail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOutcome {
    /// The job is queued again, after a backoff.
    Retrying,
    /// The job used all attempts. It is in the `dead` state.
    Dead,
    /// This worker no longer holds the lease. Nothing changed.
    LeaseLost,
    /// A job with the same kind and dedupe key is already queued, so this one
    /// does not retry. It is in the `superseded` state.
    Superseded,
}

fn secs(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

/// Adds a job. Run it in the same transaction as the domain write that needs it.
///
/// Returns `None` if a queued job has the same kind and dedupe key. A running
/// job does not block a new one, so work that arrives during a run is not lost.
///
/// # Errors
///
/// Returns an error if the query fails.
pub async fn enqueue<'e, E: PgExecutor<'e>>(
    executor: E,
    job: &NewJob<'_>,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO app.jobs (kind, payload, dedupe_key, run_after)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4::double precision))
         ON CONFLICT (kind, dedupe_key)
           WHERE dedupe_key IS NOT NULL AND status = 'queued'
           DO NOTHING
         RETURNING id",
    )
    .bind(job.kind)
    .bind(&job.payload)
    .bind(job.dedupe_key)
    .bind(secs(job.delay))
    .fetch_optional(executor)
    .await
}

/// Claims the next due job of one of `kinds`, and holds it for `lease`.
///
/// A worker claims only the kinds it can run. This makes rolling deploys safe
/// when a new release adds a job kind.
///
/// # Errors
///
/// Returns an error if the query fails.
pub async fn claim<'e, E: PgExecutor<'e>>(
    executor: E,
    kinds: &[&str],
    worker_id: &str,
    lease: Duration,
) -> Result<Option<Job>, sqlx::Error> {
    sqlx::query_as(
        "UPDATE app.jobs
         SET status = 'running',
             attempts = attempts + 1,
             locked_by = $2,
             locked_until = now() + make_interval(secs => $3::double precision),
             updated_at = now()
         WHERE id = (
             SELECT id FROM app.jobs
             WHERE status = 'queued' AND run_after <= now() AND kind = ANY($1)
             ORDER BY run_after
             LIMIT 1
             FOR UPDATE SKIP LOCKED
         )
         RETURNING id, kind, payload, attempts, max_attempts",
    )
    .bind(kinds)
    .bind(worker_id)
    .bind(secs(lease))
    .fetch_optional(executor)
    .await
}

/// Marks a claimed job as done.
///
/// Returns `false` if this worker no longer holds the lease.
///
/// # Errors
///
/// Returns an error if the query fails.
pub async fn complete<'e, E: PgExecutor<'e>>(
    executor: E,
    id: Uuid,
    worker_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE app.jobs
         SET status = 'succeeded', locked_by = NULL, locked_until = NULL, updated_at = now()
         WHERE id = $1 AND locked_by = $2 AND status = 'running'",
    )
    .bind(id)
    .bind(worker_id)
    .execute(executor)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Records a failed attempt.
///
/// The backoff is `min(5 s × 2^(attempts − 1), 1 h)`, multiplied by a random
/// factor from 0.5 to 1.0 (jitter). `error` is cut to 2,000 characters. It must
/// not contain personal data or secrets (doc §12.4).
///
/// # Errors
///
/// Returns an error if the query fails.
pub async fn fail<'e, E: PgExecutor<'e>>(
    executor: E,
    id: Uuid,
    worker_id: &str,
    error: &str,
) -> Result<FailOutcome, sqlx::Error> {
    let status: Option<String> = sqlx::query_scalar(
        "UPDATE app.jobs
         SET status = CASE
                 WHEN attempts >= max_attempts THEN 'dead'
                 WHEN dedupe_key IS NOT NULL AND EXISTS (
                     SELECT 1 FROM app.jobs q
                     WHERE q.kind = jobs.kind AND q.dedupe_key = jobs.dedupe_key
                       AND q.status = 'queued' AND q.id <> jobs.id) THEN 'superseded'
                 ELSE 'queued'
             END,
             run_after = CASE
                 WHEN attempts >= max_attempts THEN run_after
                 ELSE now() + make_interval(
                     secs => least(5 * power(2, attempts - 1), 3600) * (0.5 + random() * 0.5))
             END,
             locked_by = NULL,
             locked_until = NULL,
             last_error = left($3, 2000),
             updated_at = now()
         WHERE id = $1 AND locked_by = $2 AND status = 'running'
         RETURNING status",
    )
    .bind(id)
    .bind(worker_id)
    .bind(error)
    .fetch_optional(executor)
    .await?;
    Ok(match status.as_deref() {
        None => FailOutcome::LeaseLost,
        Some("dead") => FailOutcome::Dead,
        Some("superseded") => FailOutcome::Superseded,
        Some(_) => FailOutcome::Retrying,
    })
}

/// Puts running jobs with an expired lease back in the queue. A job that used
/// all attempts goes to `dead`.
///
/// Returns the number of jobs changed.
///
/// # Errors
///
/// Returns an error if the query fails.
pub async fn requeue_expired<'e, E: PgExecutor<'e>>(executor: E) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE app.jobs
         SET status = CASE
                 WHEN attempts >= max_attempts THEN 'dead'
                 WHEN dedupe_key IS NOT NULL AND EXISTS (
                     SELECT 1 FROM app.jobs q
                     WHERE q.kind = jobs.kind AND q.dedupe_key = jobs.dedupe_key
                       AND q.status = 'queued' AND q.id <> jobs.id) THEN 'superseded'
                 ELSE 'queued'
             END,
             locked_by = NULL,
             locked_until = NULL,
             last_error = coalesce(last_error, 'lease expired'),
             updated_at = now()
         WHERE status = 'running' AND locked_until < now()",
    )
    .execute(executor)
    .await?;
    Ok(result.rows_affected())
}
