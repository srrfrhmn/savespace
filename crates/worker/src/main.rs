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

/// Enqueues the periodic jobs. The dedupe key keeps one live job per kind.
async fn schedule(pool: &PgPool) {
    let mut cleanup = NewJob::new(handlers::CLEANUP, serde_json::json!({}));
    cleanup.dedupe_key = Some("periodic");
    if let Err(e) = jobs::enqueue(pool, &cleanup).await {
        tracing::warn!(error = %e, "cannot enqueue periodic jobs");
    }
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
        Command::Drain => drain(&pool, &Registry::new(), &worker_id()).await,
        Command::Run => run(&pool, &Registry::new(), &worker_id()).await,
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

/// Claims and runs one job. Returns `false` if no job was due.
async fn run_one(pool: &PgPool, registry: &Registry, worker_id: &str) -> anyhow::Result<bool> {
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
    match registry.run(pool, job).await {
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
