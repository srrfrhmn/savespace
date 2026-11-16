//! `worker`: the SaveSpace job runner.
//!
//! Design doc §8.2 and §8.6. Commands:
//!
//! - `worker run` (default): claim and run jobs until a shutdown signal.
//! - `worker drain`: run due jobs until the queue is empty or a time limit is
//!   reached, then exit. A scheduled Cloud Run Job uses this command.
//! - `worker migrate`: apply database migrations, then exit. This command
//!   needs the migration role, not the runtime role.

mod handlers;

use std::time::{Duration, Instant};

use anyhow::Context;
use platform_db::PgPool;
use platform_db::jobs::{self, FailOutcome, Job, NewJob};
use platform_db::outbox::{self, Subscription};

use crate::handlers::Registry;

/// How long a worker holds a job before another worker can take it.
const LEASE: Duration = Duration::from_mins(5);
/// `drain` stops claiming new jobs after this time.
const DRAIN_LIMIT: Duration = Duration::from_mins(8);
/// `run` waits this long when the queue is empty.
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// `run` enqueues the periodic jobs this often. `drain` runs every hour and
/// enqueues them on each start.
const SCHEDULE_INTERVAL: Duration = Duration::from_hours(1);

/// Outbox topics and the jobs they start (§8.6).
const SUBSCRIPTIONS: &[Subscription] = &[
    Subscription {
        topic: domain_sync::TRANSACTIONS_CHANGED,
        job_kind: domain_categorize::ENRICH_JOB,
    },
    Subscription {
        topic: domain_categorize::TRANSACTIONS_CATEGORIZED,
        job_kind: domain_recurring::DETECT_JOB,
    },
    Subscription {
        topic: domain_ledger::TRANSACTIONS_EDITED,
        job_kind: domain_recurring::DETECT_JOB,
    },
    Subscription {
        topic: domain_recurring::RECURRING_UPDATED,
        job_kind: domain_insights::GENERATE_JOB,
    },
];

/// Enqueues the periodic jobs. The dedupe key keeps one queued job per kind.
async fn schedule(pool: &PgPool) {
    for kind in [
        handlers::CLEANUP,
        domain_sync::POLL_JOB,
        domain_insights::DAILY_JOB,
        domain_notify::service::DUE_JOB,
    ] {
        let mut job = NewJob::new(kind, serde_json::json!({}));
        job.dedupe_key = Some("periodic");
        if let Err(e) = jobs::enqueue(pool, &job).await {
            tracing::warn!(error = %e, kind, "cannot enqueue a periodic job");
        }
    }
}

/// The VAPID key for Web Push, derived from `token-kek` (D13). `None` turns
/// push off.
pub(crate) fn vapid() -> Option<domain_notify::webpush::Vapid> {
    let master = platform_crypto::Kek::try_from_env()?.subkey(b"vapid");
    let subject = std::env::var("SAVESPACE_PUBLIC_ORIGIN")
        .unwrap_or_else(|_| "https://savespace.invalid".to_owned());
    Some(domain_notify::webpush::Vapid::derive(
        master.as_ref(),
        &subject,
    ))
}

/// The handlers, with bank sync if Plaid is configured.
fn registry(pool: &PgPool) -> anyhow::Result<Registry> {
    let sync = aggregator_plaid::sync_service_from_env(pool.clone())
        .map_err(anyhow::Error::msg)?
        .map(|(sync, _)| sync);
    if sync.is_none() {
        tracing::warn!("PLAID_CLIENT_ID or PLAID_SECRET not set; sync jobs will fail");
    }
    Ok(Registry::new(handlers::Ctx {
        pool: pool.clone(),
        sync,
        categorize: domain_categorize::CategorizeService::new(pool.clone()),
        recurring: domain_recurring::RecurringService::new(pool.clone()),
        ledger: domain_ledger::LedgerService::new(pool.clone()),
        insights: domain_insights::InsightService::new(pool.clone()),
        notify: domain_notify::NotifyService::new(pool.clone(), vapid()),
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Run,
    Drain,
    Migrate,
}

fn parse_command(arg: Option<&str>) -> anyhow::Result<Command> {
    match arg {
        None | Some("run") => Ok(Command::Run),
        Some("drain") => Ok(Command::Drain),
        Some("migrate") => Ok(Command::Migrate),
        Some(other) => anyhow::bail!("unknown command {other:?}; use run, drain, or migrate"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    platform_observability::init("worker");
    let arg = std::env::args().nth(1);
    let command = parse_command(arg.as_deref())?;

    let url = std::env::var(platform_db::DATABASE_URL_ENV)
        .with_context(|| format!("{} is not set", platform_db::DATABASE_URL_ENV))?;
    let pool = platform_db::connect_lazy(&url, &platform_db::PoolConfig::new("worker"))
        .context("parse DATABASE_URL")?;

    let result = match command {
        Command::Migrate => migrate(&pool).await,
        Command::Drain => drain(&pool, &registry(&pool)?, &worker_id()).await,
        Command::Run => run(&pool, &registry(&pool)?, &worker_id()).await,
    };
    pool.close().await;
    result
}

async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    tracing::info!("applying migrations");
    platform_db::migrate(pool).await.context("migrate")?;
    tracing::info!("migrations applied");
    Ok(())
}

/// A unique ID for this process. Cloud Run Jobs set the execution name.
fn worker_id() -> String {
    let base = std::env::var("CLOUD_RUN_EXECUTION").unwrap_or_else(|_| "local".to_owned());
    format!("{base}-{}", uuid::Uuid::now_v7().simple())
}

async fn drain(pool: &PgPool, registry: &Registry, worker_id: &str) -> anyhow::Result<()> {
    let started = Instant::now();
    let requeued = jobs::requeue_expired(pool).await.context("requeue")?;
    schedule(pool).await;
    let mut ran = 0_u32;
    while started.elapsed() < DRAIN_LIMIT {
        if !run_one(pool, registry, worker_id).await? {
            break;
        }
        ran += 1;
    }
    tracing::info!(
        ran,
        requeued,
        elapsed_ms = started.elapsed().as_millis(),
        "drain finished"
    );
    Ok(())
}

async fn run(pool: &PgPool, registry: &Registry, worker_id: &str) -> anyhow::Result<()> {
    tracing::info!(worker_id, kinds = ?registry.kinds(), "worker started");
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    let mut scheduled: Option<Instant> = None;
    loop {
        if scheduled.is_none_or(|t| t.elapsed() >= SCHEDULE_INTERVAL) {
            schedule(pool).await;
            scheduled = Some(Instant::now());
        }
        if let Err(e) = jobs::requeue_expired(pool).await {
            tracing::warn!(error = %e, "requeue failed");
        }
        let worked = match run_one(pool, registry, worker_id).await {
            Ok(worked) => worked,
            Err(e) => {
                tracing::warn!(error = %e, "claim failed");
                false
            }
        };
        if !worked {
            tokio::select! {
                () = &mut shutdown => break,
                () = tokio::time::sleep(POLL_INTERVAL) => {}
            }
        }
    }
    tracing::info!("worker stopped");
    Ok(())
}

/// Relays outbox events, then claims and runs one job. Returns `false` if no
/// job was due.
async fn run_one(pool: &PgPool, registry: &Registry, worker_id: &str) -> anyhow::Result<bool> {
    // A job that just finished may have written events. Their jobs run next.
    if let Err(e) = outbox::relay(pool, SUBSCRIPTIONS).await {
        tracing::warn!(error = %e, "outbox relay failed");
    }
    let Some(job) = jobs::claim(pool, &registry.kinds(), worker_id, LEASE)
        .await
        .context("claim")?
    else {
        return Ok(false);
    };
    let Job {
        id, kind, attempts, ..
    } = job.clone();
    let span = tracing::info_span!("job", %id, %kind, attempts);
    let _guard = span.enter();

    let started = Instant::now();
    match registry.run(job).await {
        Ok(()) => {
            if jobs::complete(pool, id, worker_id).await? {
                tracing::info!(elapsed_ms = started.elapsed().as_millis(), "job succeeded");
            } else {
                tracing::warn!("job finished after its lease expired");
            }
        }
        Err(e) => {
            let outcome = jobs::fail(pool, id, worker_id, &format!("{e:#}")).await?;
            match outcome {
                FailOutcome::Retrying => tracing::warn!(error = %e, "job failed; will retry"),
                FailOutcome::Dead => tracing::error!(error = %e, "job failed; no attempts left"),
                FailOutcome::Superseded => {
                    tracing::warn!(error = %e, "job failed; a newer job does the same work");
                }
                FailOutcome::LeaseLost => {
                    tracing::warn!(error = %e, "job failed after lease expired");
                }
            }
        }
    }
    Ok(true)
}

/// Resolves on Ctrl-C, or on SIGTERM on Unix (Cloud Run sends SIGTERM).
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(parse_command(None).unwrap(), Command::Run);
        assert_eq!(parse_command(Some("run")).unwrap(), Command::Run);
        assert_eq!(parse_command(Some("drain")).unwrap(), Command::Drain);
        assert_eq!(parse_command(Some("migrate")).unwrap(), Command::Migrate);
        assert!(parse_command(Some("nope")).is_err());
    }
}
