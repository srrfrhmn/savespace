//! Job handlers, by job kind.
//!
//! Each handler calls a domain crate. Handlers must be idempotent: a job can
//! run more than once (design doc §8.6).

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use platform_db::PgPool;
use platform_db::jobs::Job;

type HandlerFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
type Handler = fn(PgPool, Job) -> HandlerFuture;

/// The job kinds this worker can run.
pub(crate) struct Registry {
    handlers: BTreeMap<&'static str, Handler>,
}

impl Registry {
    /// All handlers in this release. Phase 1 adds `sync.transactions`,
    /// `insights.generate`, and others.
    pub(crate) fn new() -> Self {
        let mut handlers: BTreeMap<&'static str, Handler> = BTreeMap::new();
        handlers.insert("system.ping", |_, job| {
            Box::pin(std::future::ready(ping(&job)))
        });
        Self { handlers }
    }

    /// The kinds to claim.
    pub(crate) fn kinds(&self) -> Vec<&'static str> {
        self.handlers.keys().copied().collect()
    }

    /// Runs the handler for `job.kind`.
    pub(crate) async fn run(&self, pool: &PgPool, job: Job) -> anyhow::Result<()> {
        let Some(handler) = self.handlers.get(job.kind.as_str()) else {
            anyhow::bail!("no handler for kind {:?}", job.kind);
        };
        handler(pool.clone(), job).await
    }
}

/// `system.ping`: does nothing and succeeds. Operators enqueue it to check the
/// queue and the scheduled worker from end to end.
#[expect(clippy::unnecessary_wraps, reason = "handlers return Result")]
fn ping(job: &Job) -> anyhow::Result<()> {
    tracing::info!(payload = %job.payload, "ping");
    Ok(())
}
