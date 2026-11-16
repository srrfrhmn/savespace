//! Job handlers, by job kind.
//!
//! Each handler calls a domain crate. Handlers must be idempotent: a job can
//! run more than once (design doc §8.6).

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use domain_categorize::CategorizeService;
use domain_sync::{SyncError, SyncJob, SyncService};
use platform_db::PgPool;
use platform_db::jobs::Job;
use uuid::Uuid;

/// What handlers need.
pub(crate) struct Ctx {
    pub(crate) pool: PgPool,
    /// `None` if Plaid is not configured. Sync jobs then fail and retry.
    pub(crate) sync: Option<SyncService>,
    pub(crate) categorize: CategorizeService,
}

type HandlerFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
type Handler = fn(Arc<Ctx>, Job) -> HandlerFuture;

/// The kind of the hourly cleanup job.
pub(crate) const CLEANUP: &str = "system.cleanup";

/// A connection syncs at least this often without webhooks (§10.2).
const POLL_MAX_AGE: Duration = Duration::from_hours(6);

/// The job kinds this worker can run.
pub(crate) struct Registry {
    ctx: Arc<Ctx>,
    handlers: BTreeMap<&'static str, Handler>,
}

impl Registry {
    /// All handlers in this release.
    pub(crate) fn new(ctx: Ctx) -> Self {
        let mut handlers: BTreeMap<&'static str, Handler> = BTreeMap::new();
        handlers.insert("system.ping", |_, job| {
            Box::pin(std::future::ready(ping(&job)))
        });
        handlers.insert(CLEANUP, |ctx, _| Box::pin(cleanup(ctx)));
        handlers.insert(domain_sync::SYNC_JOB, |ctx, job| Box::pin(sync(ctx, job)));
        handlers.insert(domain_sync::WEBHOOK_JOB, |ctx, job| {
            Box::pin(webhook(ctx, job))
        });
        handlers.insert(domain_sync::POLL_JOB, |ctx, _| Box::pin(poll(ctx)));
        handlers.insert(domain_categorize::ENRICH_JOB, |ctx, job| {
            Box::pin(enrich(ctx, job))
        });
        Self {
            ctx: Arc::new(ctx),
            handlers,
        }
    }

    /// The kinds to claim.
    pub(crate) fn kinds(&self) -> Vec<&'static str> {
        self.handlers.keys().copied().collect()
    }

    /// Runs the handler for `job.kind`.
    pub(crate) async fn run(&self, job: Job) -> anyhow::Result<()> {
        let Some(handler) = self.handlers.get(job.kind.as_str()) else {
            anyhow::bail!("no handler for kind {:?}", job.kind);
        };
        handler(Arc::clone(&self.ctx), job).await
    }
}

fn sync_service(ctx: &Ctx) -> anyhow::Result<&SyncService> {
    ctx.sync
        .as_ref()
        .context("Plaid is not configured on this worker")
}

/// `system.cleanup`: deletes expired sign-in challenges, sessions, and
/// idempotency keys.
async fn cleanup(ctx: Arc<Ctx>) -> anyhow::Result<()> {
    let deleted = domain_identity::delete_expired(&ctx.pool).await?;
    tracing::info!(deleted, "expired rows deleted");
    Ok(())
}

/// `sync.connection`: syncs one connection.
async fn sync(ctx: Arc<Ctx>, job: Job) -> anyhow::Result<()> {
    let payload: SyncJob = serde_json::from_value(job.payload).context("payload")?;
    let service = sync_service(&ctx)?;
    match service
        .sync_connection(payload.household_id, payload.connection_id)
        .await
    {
        Ok(outcome) => {
            tracing::info!(
                accounts = outcome.accounts,
                added = outcome.added,
                modified = outcome.modified,
                removed = outcome.removed,
                "sync finished"
            );
            Ok(())
        }
        // The person must act. A retry cannot help. The status shows it.
        Err(SyncError::NeedsAttention) => {
            tracing::info!("connection needs attention; not retrying");
            Ok(())
        }
        // The connection was deleted after the job was queued.
        Err(SyncError::NotFound) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// `sync.webhook`: applies a stored webhook.
async fn webhook(ctx: Arc<Ctx>, job: Job) -> anyhow::Result<()> {
    let event_id: Uuid = serde_json::from_value(
        job.payload
            .get("event_id")
            .cloned()
            .context("payload has no event_id")?,
    )
    .context("event_id")?;
    let service = sync_service(&ctx)?;
    let (item_id, webhook_type, webhook_code, body) = service.webhook_event(event_id).await?;
    let action = aggregator_plaid::webhook_action(&webhook_type, &webhook_code, &body);
    tracing::info!(%webhook_type, %webhook_code, ?action, "webhook");
    service
        .apply_webhook(event_id, item_id.as_deref(), &action)
        .await?;
    Ok(())
}

/// `sync.poll`: queues syncs for connections that have not synced recently.
async fn poll(ctx: Arc<Ctx>) -> anyhow::Result<()> {
    let Some(service) = ctx.sync.as_ref() else {
        return Ok(());
    };
    let queued = service.enqueue_due_polls(POLL_MAX_AGE).await?;
    tracing::info!(queued, "fallback polls queued");
    Ok(())
}

/// The household of an outbox subscriber job.
fn household_of(job: &Job) -> anyhow::Result<Uuid> {
    serde_json::from_value(
        job.payload
            .get("household_id")
            .cloned()
            .context("payload has no household_id")?,
    )
    .context("household_id")
}

/// `categorize.household`: sets merchants and categories after a sync
/// (the `transactions.changed` event).
async fn enrich(ctx: Arc<Ctx>, job: Job) -> anyhow::Result<()> {
    let household_id = household_of(&job)?;
    let updated = ctx.categorize.enrich_household(household_id).await?;
    tracing::info!(updated, "transactions enriched");
    Ok(())
}

/// `system.ping`: does nothing and succeeds. Operators enqueue it to check the
/// queue and the scheduled worker from end to end.
#[expect(clippy::unnecessary_wraps, reason = "handlers return Result")]
fn ping(job: &Job) -> anyhow::Result<()> {
    tracing::info!(payload = %job.payload, "ping");
    Ok(())
}
