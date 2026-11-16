//! The outbox relay (design doc §8.6).
//!
//! A domain write and its event commit in one transaction (`app.outbox`). The
//! relay turns each event into jobs for its subscribers, then marks the event
//! published. Subscriber jobs for one household share a dedupe key, so a
//! burst of events gives one queued job.

use serde_json::json;
use uuid::Uuid;

use crate::jobs::{self, NewJob};

/// A job kind that runs for each event of a topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subscription {
    /// The event topic, for example `transactions.changed`.
    pub topic: &'static str,
    /// The job kind to enqueue. The payload is
    /// `{"household_id": …, "event_id": …}`.
    pub job_kind: &'static str,
}

/// Events per relay call.
const BATCH: i64 = 100;

/// Relays unpublished events, oldest first. Returns how many events it
/// published. Safe to run in parallel: each event is locked while it relays.
///
/// # Errors
///
/// Returns an error if a query fails. Nothing is published then.
pub async fn relay(
    pool: &sqlx::PgPool,
    subscriptions: &[Subscription],
) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let events: Vec<(i64, Uuid, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT seq, id, topic, household_id FROM app.outbox \
         WHERE published_at IS NULL ORDER BY seq LIMIT $1 FOR UPDATE SKIP LOCKED",
    )
    .bind(BATCH)
    .fetch_all(&mut *tx)
    .await?;
    if events.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }
    for (_, id, topic, household_id) in &events {
        let dedupe = household_id.map(|h| h.to_string());
        for sub in subscriptions.iter().filter(|s| s.topic == topic) {
            let mut job = NewJob::new(
                sub.job_kind,
                json!({ "household_id": household_id, "event_id": id }),
            );
            job.dedupe_key = dedupe.as_deref();
            jobs::enqueue(&mut *tx, &job).await?;
        }
    }
    let seqs: Vec<i64> = events.iter().map(|e| e.0).collect();
    let published = sqlx::query("UPDATE app.outbox SET published_at = now() WHERE seq = ANY($1)")
        .bind(&seqs)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(published)
}
