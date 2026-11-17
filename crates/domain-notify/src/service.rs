//! Notifications: queue from insights, apply the policy at send time, and
//! deliver by Web Push (design doc §10.4, D13).

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::policy::{self, Decision, Settings};
use crate::webpush::{self, Keys, Vapid};

/// The job kind that sends one household's due notifications.
pub const SEND_JOB: &str = "notify.household";

/// The hourly job that queues [`SEND_JOB`] for households with notifications
/// that quiet hours delayed.
pub const DUE_JOB: &str = "notify.due";

/// Insight kinds that can become notifications. Reassurance stays in the app.
pub const NOTIFY_KINDS: &[&str] = &["concern", "anticipation", "awareness"];

/// A subscription is removed after this many failed sends in a row.
const MAX_FAILURES: i32 = 5;

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    /// The endpoint is not on a known push service, or the keys are bad.
    #[error("invalid subscription")]
    InvalidSubscription,
    /// The endpoint belongs to another household.
    #[error("subscription conflict")]
    Conflict,
    /// Push is off on this server (no VAPID key).
    #[error("push is not available")]
    Unavailable,
    /// A database error.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

type Result<T, E = NotifyError> = std::result::Result<T, E>;

/// The counts from one send run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Sent to at least one browser.
    pub sent: usize,
    /// Waiting for quiet hours to end.
    pub delayed: usize,
    /// Skipped, expired, or failed.
    pub dropped: usize,
}

/// What a push service answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    Ok,
    /// 404 or 410: the subscription is gone.
    Gone,
    Failed,
}

type SettingsRow = (
    Uuid,
    String,
    jiff_sqlx::Time,
    jiff_sqlx::Time,
    i16,
    Vec<String>,
    bool,
);
type DueRow = (
    Uuid,
    String,
    Option<String>,
    Option<String>,
    jiff_sqlx::Timestamp,
);
type SubscriptionRow = (Uuid, String, Vec<u8>, Vec<u8>);

/// Notifications and push subscriptions.
#[derive(Debug, Clone)]
pub struct NotifyService {
    pool: PgPool,
    http: Option<reqwest::Client>,
    vapid: Option<Vapid>,
    /// Tests only: allow any `http(s)` endpoint (a mock push service).
    any_endpoint: bool,
}

impl NotifyService {
    /// Creates the service. Without a VAPID key, push is off: notifications
    /// are still queued and decided, and stay in the app.
    pub fn new(pool: PgPool, vapid: Option<Vapid>) -> Self {
        // reqwest uses rustls without a built-in provider (see aggregator-plaid).
        let _ = rustls_provider();
        let http = reqwest::Client::builder()
            .user_agent("savespace")
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok();
        Self {
            pool,
            http,
            vapid,
            any_endpoint: false,
        }
    }

    /// Tests only: accept any endpoint, so a test can run a mock push
    /// service. Never use it in the api or the worker.
    #[must_use]
    pub const fn with_any_endpoint_for_tests(mut self) -> Self {
        self.any_endpoint = true;
        self
    }

    fn endpoint_allowed(&self, url: &url::Url) -> bool {
        self.any_endpoint || webpush::allowed_endpoint(url)
    }

    /// The VAPID public key for `pushManager.subscribe`, if push is on.
    pub fn public_key(&self) -> Option<&str> {
        self.vapid.as_ref().map(|v| v.public_key.as_str())
    }

    /// Saves a browser's subscription.
    ///
    /// # Errors
    ///
    /// [`NotifyError::InvalidSubscription`], [`NotifyError::Conflict`], or a
    /// database error.
    pub async fn subscribe(
        &self,
        household_id: Uuid,
        user_id: Uuid,
        endpoint: &str,
        keys: &Keys,
    ) -> Result<()> {
        let url: url::Url = endpoint
            .parse()
            .map_err(|_| NotifyError::InvalidSubscription)?;
        if endpoint.len() > 1000 || !self.endpoint_allowed(&url) {
            return Err(NotifyError::InvalidSubscription);
        }
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        sqlx::query("DELETE FROM app.push_subscriptions WHERE endpoint = $1")
            .bind(endpoint)
            .execute(&mut *tx)
            .await?;
        let inserted = sqlx::query(
            "INSERT INTO app.push_subscriptions (household_id, user_id, endpoint, p256dh, auth) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(household_id)
        .bind(user_id)
        .bind(endpoint)
        .bind(&keys.p256dh)
        .bind(&keys.auth)
        .execute(&mut *tx)
        .await;
        match inserted {
            Ok(_) => {}
            // The endpoint is stored for another household (RLS hid it).
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(NotifyError::Conflict);
            }
            Err(e) => return Err(e.into()),
        }
        tx.commit().await?;
        Ok(())
    }

    /// Removes a browser's subscription.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn unsubscribe(&self, household_id: Uuid, endpoint: &str) -> Result<()> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        sqlx::query("DELETE FROM app.push_subscriptions WHERE endpoint = $1")
            .bind(endpoint)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Queues a notification for each new insight that can notify. Idempotent:
    /// one notification per insight.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn queue_from_insights(&self, household_id: Uuid) -> Result<u64> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let queued = sqlx::query(
            "INSERT INTO app.notifications (household_id, user_id, insight_id, kind) \
             SELECT i.household_id, m.user_id, i.id, i.kind FROM app.insights i \
             JOIN app.household_members m ON m.household_id = i.household_id \
             WHERE i.status = 'open' AND i.transaction_id IS NULL AND i.kind = ANY($1) \
               AND i.created_at > now() - interval '24 hours' \
             ON CONFLICT (insight_id) DO NOTHING",
        )
        .bind(NOTIFY_KINDS)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(queued)
    }

    /// Applies the policy to due notifications and sends the ones it allows.
    ///
    /// # Errors
    ///
    /// A database error. A failed push is recorded, not returned.
    pub async fn send_due(&self, household_id: Uuid) -> Result<Outcome> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rows: Vec<SettingsRow> = sqlx::query_as(
            "SELECT u.id, u.time_zone, u.quiet_start, u.quiet_end, u.notify_daily_limit, \
               u.notify_muted_kinds, u.calm_mode FROM app.users u \
             JOIN app.household_members m ON m.user_id = u.id",
        )
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut outcome = Outcome::default();
        for (user_id, zone, quiet_start, quiet_end, limit, muted, calm) in rows {
            let settings = Settings {
                quiet_start: quiet_start.to_jiff(),
                quiet_end: quiet_end.to_jiff(),
                daily_limit: u8::try_from(limit).unwrap_or(1),
                muted_kinds: muted,
                calm_mode: calm,
            };
            let tz = jiff::tz::TimeZone::get(&zone).unwrap_or(jiff::tz::TimeZone::UTC);
            let one = self
                .send_for_user(household_id, user_id, &settings, &tz)
                .await?;
            outcome.sent += one.sent;
            outcome.delayed += one.delayed;
            outcome.dropped += one.dropped;
        }
        Ok(outcome)
    }

    async fn send_for_user(
        &self,
        household_id: Uuid,
        user_id: Uuid,
        settings: &Settings,
        tz: &jiff::tz::TimeZone,
    ) -> Result<Outcome> {
        let now = jiff::Timestamp::now();
        let local = now.to_zoned_datetime(tz.clone());
        let midnight = local
            .date()
            .to_zoned_datetime(tz.clone())
            .map_or(now, |z| z.timestamp());

        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let due: Vec<DueRow> = sqlx::query_as(
            "SELECT n.id, n.kind, i.text, i.status, n.created_at FROM app.notifications n \
             LEFT JOIN app.insights i ON i.id = n.insight_id \
             WHERE n.user_id = $1 AND n.status = 'queued' AND n.scheduled_for <= now() \
             ORDER BY (n.kind = 'concern') DESC, n.created_at",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        let mut sent_today: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM app.notifications WHERE user_id = $1 AND status = 'sent' \
               AND sent_at >= $2",
        )
        .bind(user_id)
        .bind(jiff_sqlx::Timestamp::from(midnight))
        .fetch_one(&mut *tx)
        .await?;
        let subscriptions: Vec<SubscriptionRow> = sqlx::query_as(
            "SELECT id, endpoint, p256dh, auth FROM app.push_subscriptions WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;

        let mut outcome = Outcome::default();
        let mut to_send = Vec::new();
        for (id, kind, text, insight_status, created_at) in due {
            let age = now.duration_since(created_at.to_jiff()).as_hours();
            // The insight was resolved, dismissed, or marked wrong meanwhile.
            let decision = if insight_status.as_deref() != Some("open") {
                Decision::Skip("resolved")
            } else if subscriptions.is_empty() || self.vapid.is_none() {
                Decision::Skip("no_subscription")
            } else {
                policy::decide(
                    settings,
                    &kind,
                    local.datetime(),
                    age,
                    u32::try_from(sent_today).unwrap_or(u32::MAX),
                )
            };
            match decision {
                Decision::Send => {
                    sent_today += 1;
                    let body =
                        policy::body(text.as_deref().unwrap_or(""), &kind, settings.calm_mode);
                    to_send.push((id, kind, body));
                }
                Decision::Delay(until) => {
                    let at = until
                        .to_zoned_datetime(tz.clone())
                        .map_or(now, |z| z.timestamp());
                    sqlx::query("UPDATE app.notifications SET scheduled_for = $2 WHERE id = $1")
                        .bind(id)
                        .bind(jiff_sqlx::Timestamp::from(at))
                        .execute(&mut *tx)
                        .await?;
                    outcome.delayed += 1;
                }
                Decision::Skip(reason) => {
                    set_status(&mut tx, id, "skipped", Some(reason)).await?;
                    outcome.dropped += 1;
                }
                Decision::Expire => {
                    set_status(&mut tx, id, "expired", None).await?;
                    outcome.dropped += 1;
                }
            }
        }
        tx.commit().await?;

        // Network calls run outside the database transaction.
        for (id, kind, body) in to_send {
            let payload = json!({ "title": "SaveSpace", "body": body, "url": "/" }).to_string();
            let urgency = if kind == "concern" { "high" } else { "normal" };
            let delivered = self
                .deliver(household_id, &subscriptions, &payload, urgency)
                .await?;
            let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
            if delivered > 0 {
                sqlx::query(
                    "UPDATE app.notifications SET status = 'sent', sent_at = now() WHERE id = $1",
                )
                .bind(id)
                .execute(&mut *tx)
                .await?;
                outcome.sent += 1;
            } else {
                set_status(&mut tx, id, "failed", Some("push_failed")).await?;
                outcome.dropped += 1;
            }
            tx.commit().await?;
        }
        Ok(outcome)
    }

    /// Sends a test notification to the person's browsers. Returns how many
    /// accepted it.
    ///
    /// # Errors
    ///
    /// [`NotifyError::Unavailable`] without a VAPID key, or a database error.
    pub async fn send_test(&self, household_id: Uuid, user_id: Uuid) -> Result<usize> {
        if self.vapid.is_none() {
            return Err(NotifyError::Unavailable);
        }
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let subscriptions: Vec<SubscriptionRow> = sqlx::query_as(
            "SELECT id, endpoint, p256dh, auth FROM app.push_subscriptions WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let payload = json!({
            "title": "SaveSpace",
            "body": "Notifications are on. SaveSpace sends at most one a day, outside quiet hours.",
            "url": "/settings",
        })
        .to_string();
        self.deliver(household_id, &subscriptions, &payload, "normal")
            .await
    }

    /// Pushes one payload to each subscription. Removes gone subscriptions.
    async fn deliver(
        &self,
        household_id: Uuid,
        subscriptions: &[SubscriptionRow],
        payload: &str,
        urgency: &str,
    ) -> Result<usize> {
        let mut ok = 0;
        for (id, endpoint, p256dh, auth) in subscriptions {
            let keys = Keys {
                p256dh: p256dh.clone(),
                auth: auth.clone(),
            };
            let result = self.push(endpoint, &keys, payload, urgency).await;
            let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
            match result {
                Delivery::Ok => {
                    ok += 1;
                    sqlx::query(
                        "UPDATE app.push_subscriptions SET failures = 0, last_sent_at = now() \
                         WHERE id = $1",
                    )
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                }
                Delivery::Gone => {
                    sqlx::query("DELETE FROM app.push_subscriptions WHERE id = $1")
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                }
                Delivery::Failed => {
                    sqlx::query(
                        "WITH f AS (UPDATE app.push_subscriptions SET failures = failures + 1 \
                           WHERE id = $1 RETURNING id, failures) \
                         DELETE FROM app.push_subscriptions s USING f \
                         WHERE s.id = f.id AND f.failures >= $2",
                    )
                    .bind(id)
                    .bind(MAX_FAILURES)
                    .execute(&mut *tx)
                    .await?;
                }
            }
            tx.commit().await?;
        }
        Ok(ok)
    }

    async fn push(&self, endpoint: &str, keys: &Keys, payload: &str, urgency: &str) -> Delivery {
        let (Some(http), Some(vapid)) = (&self.http, &self.vapid) else {
            return Delivery::Failed;
        };
        let Ok(url) = endpoint.parse::<url::Url>() else {
            return Delivery::Gone;
        };
        if !self.endpoint_allowed(&url) {
            return Delivery::Gone;
        }
        let Ok(body) = webpush::encrypt(keys, payload.as_bytes()) else {
            return Delivery::Gone;
        };
        let response = http
            .post(url.clone())
            .header("TTL", "86400")
            .header("Urgency", urgency)
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header(
                "Authorization",
                vapid.authorization(&url, jiff::Timestamp::now()),
            )
            .body(body)
            .send()
            .await;
        match response.map(|r| r.status().as_u16()) {
            Ok(200..=299) => Delivery::Ok,
            Ok(404 | 410) => Delivery::Gone,
            Ok(status) => {
                tracing::warn!(status, host = url.host_str(), "push rejected");
                Delivery::Failed
            }
            Err(e) => {
                tracing::warn!(error = %e, host = url.host_str(), "push failed");
                Delivery::Failed
            }
        }
    }

    /// Households with notifications due now.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn households_due(&self) -> Result<Vec<Uuid>> {
        Ok(
            sqlx::query_scalar("SELECT household_id FROM app.households_with_due_notifications()")
                .fetch_all(&self.pool)
                .await?,
        )
    }
}

async fn set_status(
    conn: &mut sqlx::PgConnection,
    id: Uuid,
    status: &str,
    reason: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE app.notifications SET status = $2, reason = $3 WHERE id = $1")
        .bind(id)
        .bind(status)
        .bind(reason)
        .execute(conn)
        .await?;
    Ok(())
}

/// Installs the ring TLS provider once. A second install is not an error.
fn rustls_provider() -> bool {
    rustls::crypto::ring::default_provider()
        .install_default()
        .is_ok()
}

/// The base64url form of subscription keys, for tests and exports.
pub fn encode_key(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}
