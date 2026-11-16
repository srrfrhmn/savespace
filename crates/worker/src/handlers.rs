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
use domain_insights::InsightService;
use domain_insights::facts::MonthInput;
use domain_ledger::LedgerService;
use domain_notify::NotifyService;
use domain_recurring::RecurringService;
use domain_sync::{SyncError, SyncJob, SyncService};
use platform_db::PgPool;
use platform_db::jobs::{self, Job, NewJob};
use uuid::Uuid;

/// What handlers need.
pub(crate) struct Ctx {
    pub(crate) pool: PgPool,
    /// `None` if Plaid is not configured. Sync jobs then fail and retry.
    pub(crate) sync: Option<SyncService>,
    pub(crate) categorize: CategorizeService,
    pub(crate) recurring: RecurringService,
    pub(crate) ledger: LedgerService,
    pub(crate) insights: InsightService,
    pub(crate) notify: NotifyService,
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
        handlers.insert(domain_recurring::DETECT_JOB, |ctx, job| {
            Box::pin(detect_recurring(ctx, job))
        });
        handlers.insert(domain_insights::GENERATE_JOB, |ctx, job| {
            Box::pin(generate_insights(ctx, job))
        });
        handlers.insert(domain_insights::DAILY_JOB, |ctx, _| {
            Box::pin(daily_insights(ctx))
        });
        handlers.insert(domain_notify::service::SEND_JOB, |ctx, job| {
            Box::pin(send_notifications(ctx, job))
        });
        handlers.insert(domain_notify::service::DUE_JOB, |ctx, _| {
            Box::pin(due_notifications(ctx))
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

/// `recurring.household`: finds recurring series after enrichment or an edit.
async fn detect_recurring(ctx: Arc<Ctx>, job: Job) -> anyhow::Result<()> {
    let household_id = household_of(&job)?;
    let series = ctx.recurring.detect_household(household_id).await?;
    tracing::info!(series, "recurring series found");
    Ok(())
}

/// `insights.household`: runs the insight pipeline (design doc §7.2). The
/// flexible plan numbers come from the budget, so insights match the Budget
/// page.
async fn generate_insights(ctx: Arc<Ctx>, job: Job) -> anyhow::Result<()> {
    let household_id = household_of(&job)?;
    let month = ctx.insights.local_month(household_id).await?;
    let budget = ctx.ledger.budget_month(household_id, month).await?;
    let planned: Vec<_> = budget
        .categories
        .iter()
        .filter(|c| c.kind == "flex" && c.plan.is_some())
        .collect();
    let input = MonthInput {
        plan: planned
            .iter()
            .map(|c| c.plan.unwrap_or(0) + c.carried)
            .sum(),
        left: budget.flex.left,
    };
    let outcome = ctx
        .insights
        .generate(household_id, (!planned.is_empty()).then_some(input))
        .await?;
    tracing::info!(
        created = outcome.created,
        refreshed = outcome.refreshed,
        expired = outcome.expired,
        "insights generated"
    );
    // New insights can become notifications (design doc §10.4).
    let queued = ctx.notify.queue_from_insights(household_id).await?;
    let sent = ctx.notify.send_due(household_id).await?;
    tracing::info!(
        queued,
        sent = sent.sent,
        delayed = sent.delayed,
        "notifications"
    );
    Ok(())
}

/// `notify.household`: sends one household's due notifications.
async fn send_notifications(ctx: Arc<Ctx>, job: Job) -> anyhow::Result<()> {
    let household_id = household_of(&job)?;
    let sent = ctx.notify.send_due(household_id).await?;
    tracing::info!(
        sent = sent.sent,
        delayed = sent.delayed,
        dropped = sent.dropped,
        "notifications"
    );
    Ok(())
}

/// `notify.due`: queues sends for notifications that quiet hours delayed.
async fn due_notifications(ctx: Arc<Ctx>) -> anyhow::Result<()> {
    let due = ctx.notify.households_due().await?;
    for household_id in &due {
        let key = household_id.to_string();
        let mut job = NewJob::new(
            domain_notify::service::SEND_JOB,
            serde_json::json!({ "household_id": household_id }),
        );
        job.dedupe_key = Some(&key);
        jobs::enqueue(&ctx.pool, &job).await?;
    }
    Ok(())
}

/// `insights.daily`: queues the 06:00 local run for each household that is
/// due (design doc §7.2 triggers).
async fn daily_insights(ctx: Arc<Ctx>) -> anyhow::Result<()> {
    let due = ctx.insights.due_for_daily_run().await?;
    for household_id in &due {
        let key = household_id.to_string();
        let mut job = NewJob::new(
            domain_insights::GENERATE_JOB,
            serde_json::json!({ "household_id": household_id }),
        );
        job.dedupe_key = Some(&key);
        jobs::enqueue(&ctx.pool, &job).await?;
    }
    tracing::info!(queued = due.len(), "daily insight runs queued");
    Ok(())
}

/// `system.ping`: does nothing and succeeds. Operators enqueue it to check the
/// queue and the scheduled worker from end to end.
#[expect(clippy::unnecessary_wraps, reason = "handlers return Result")]
fn ping(job: &Job) -> anyhow::Result<()> {
    tracing::info!(payload = %job.payload, "ping");
    Ok(())
}
