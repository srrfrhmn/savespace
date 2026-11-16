//! Storing, listing, and feedback for insights (design doc §7.2 steps 2–4
//! and 9, §7.7).

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::facts::{self, MonthInput};
use crate::model::{Candidate, Kind};
use crate::policy::{self, Shown};
use crate::rules::registry;
use crate::validator;

/// Three "Not helpful" for one rule in 30 days mute it (§7.7).
pub const MUTE_AFTER: i64 = 3;

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum InsightError {
    /// The insight does not exist in this household.
    #[error("not found")]
    NotFound,
    /// A database error.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// An encryption error.
    #[error(transparent)]
    Crypto(#[from] platform_crypto::CryptoError),
    /// Check-in notes need the token key, and it is not set.
    #[error("notes are not available")]
    NotesUnavailable,
}

pub(crate) type Result<T, E = InsightError> = std::result::Result<T, E>;

/// A stored insight.
#[derive(Debug, Clone, Serialize)]
pub struct StoredInsight {
    /// The insight ID.
    pub id: Uuid,
    /// The rule.
    pub rule_id: String,
    /// The kind.
    pub kind: Kind,
    /// The scope.
    pub scope: String,
    /// The text.
    pub text: String,
    /// The facts, for "Why am I seeing this?".
    pub facts: Value,
    /// 0 to 10,000.
    pub relevance_bp: i32,
    /// The transaction, for insights about one transaction.
    pub transaction_id: Option<Uuid>,
    /// When it was made.
    pub created_at: jiff::Timestamp,
}

impl Shown for StoredInsight {
    fn kind(&self) -> Kind {
        self.kind
    }
    fn scope(&self) -> &str {
        &self.scope
    }
    fn relevance_bp(&self) -> i32 {
        self.relevance_bp
    }
}

/// Which insights to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The home feed: not about one transaction. At most 3 (§7.1).
    Home,
    /// About one transaction.
    Transaction(Uuid),
}

/// The counts from one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Outcome {
    /// New insights.
    pub created: usize,
    /// Open insights with new facts.
    pub refreshed: usize,
    /// Open insights whose condition no longer holds.
    pub expired: u64,
}

/// "This is wrong" or "Not helpful" (§7.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    /// Hide at once and add to the review queue, with a reason.
    Wrong(Reason),
    /// Lower relevance. Three in 30 days mute the rule.
    NotHelpful,
}

/// Why an insight is wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Wrong amount.
    Amount,
    /// Wrong merchant.
    Merchant,
    /// Wrong category.
    Category,
    /// Wrong date.
    Date,
    /// Something else.
    Other,
}

impl Reason {
    /// The stable name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Amount => "amount",
            Self::Merchant => "merchant",
            Self::Category => "category",
            Self::Date => "date",
            Self::Other => "other",
        }
    }

    /// Parses [`Reason::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Amount,
            Self::Merchant,
            Self::Category,
            Self::Date,
            Self::Other,
        ]
        .into_iter()
        .find(|r| r.as_str() == value)
    }
}

type InsightRow = (
    Uuid,
    String,
    String,
    String,
    String,
    Value,
    i32,
    Option<Uuid>,
    jiff_sqlx::Timestamp,
);

fn facts_json(c: &Candidate) -> Value {
    Value::from(
        c.facts
            .iter()
            .map(|f| json!({ "key": f.key, "label": f.label, "value": f.display }))
            .collect::<Vec<_>>(),
    )
}

/// The candidates from every rule that is on and not muted, with texts that
/// pass the copy checks.
async fn candidates(
    conn: &mut sqlx::PgConnection,
    facts: &crate::rules::FactSet,
) -> Result<Vec<Candidate>> {
    let off: HashSet<String> = sqlx::query_scalar(
        "SELECT rule_id FROM app.insight_rule_flags WHERE NOT enabled \
             UNION SELECT rule_id FROM app.insight_mutes",
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .collect();
    Ok(registry()
        .iter()
        .filter(|r| !off.contains(r.id()))
        .flat_map(|r| r.evaluate(facts))
        .filter(|c| {
            let problems = validator::check(&c.text, c.kind);
            if !problems.is_empty() {
                tracing::warn!(
                    rule = c.rule_id,
                    ?problems,
                    "insight text failed the checks"
                );
            }
            problems.is_empty()
        })
        .collect())
}

/// Insights.
#[derive(Debug, Clone)]
pub struct InsightService {
    pool: PgPool,
}

impl InsightService {
    /// Creates the service.
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Runs the pipeline for one household (§7.2). `month` comes from the
    /// budget, so the numbers match the Budget page.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn generate(&self, household_id: Uuid, month: Option<MonthInput>) -> Result<Outcome> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let facts = facts::build(&mut tx, month).await?;
        let candidates = candidates(&mut tx, &facts).await?;

        let open: HashMap<(String, String), Uuid> = sqlx::query_as::<_, (String, String, Uuid)>(
            "SELECT rule_id, scope, id FROM app.insights WHERE status = 'open'",
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(r, s, id)| ((r, s), id))
        .collect();
        let mut outcome = Outcome::default();
        let mut current: Vec<Uuid> = Vec::new();
        for c in &candidates {
            let key = (c.rule_id.to_owned(), c.scope.clone());
            if let Some(id) = open.get(&key) {
                sqlx::query(
                    "UPDATE app.insights SET text = $2, facts = $3, rule_version = $4, \
                       confidence_bp = $5, relevance_bp = $6, updated_at = now() WHERE id = $1",
                )
                .bind(id)
                .bind(&c.text)
                .bind(facts_json(c))
                .bind(i32::from(c.rule_version))
                .bind(c.confidence_bp)
                .bind(c.relevance_bp)
                .execute(&mut *tx)
                .await?;
                current.push(*id);
                outcome.refreshed += 1;
                continue;
            }
            // Dedupe (§7.2 step 4): one rule and scope at most once in 7 days.
            let recent: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM app.insights WHERE rule_id = $1 AND scope = $2 \
                   AND created_at > now() - interval '7 days')",
            )
            .bind(c.rule_id)
            .bind(&c.scope)
            .fetch_one(&mut *tx)
            .await?;
            if recent {
                continue;
            }
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO app.insights (household_id, rule_id, rule_version, kind, scope, \
                   text, facts, confidence_bp, relevance_bp, transaction_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
            )
            .bind(household_id)
            .bind(c.rule_id)
            .bind(i32::from(c.rule_version))
            .bind(c.kind.as_str())
            .bind(&c.scope)
            .bind(&c.text)
            .bind(facts_json(c))
            .bind(c.confidence_bp)
            .bind(c.relevance_bp)
            .bind(c.transaction_id)
            .fetch_one(&mut *tx)
            .await?;
            current.push(id);
            outcome.created += 1;
        }
        // The condition no longer holds, or the rule is off or muted.
        outcome.expired = sqlx::query(
            "UPDATE app.insights SET status = 'expired', updated_at = now() \
             WHERE status = 'open' AND NOT (id = ANY($1))",
        )
        .bind(&current)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        sqlx::query("UPDATE app.households SET insights_run_on = $1 WHERE id = $2")
            .bind(jiff_sqlx::Date::from(facts::date_of(facts.today)))
            .bind(household_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(outcome)
    }

    /// The first day of the current month in the owner's time zone.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn local_month(&self, household_id: Uuid) -> Result<jiff::civil::Date> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let today = facts::today(&mut tx).await?;
        tx.commit().await?;
        Ok(facts::date_of(today.month_start()))
    }

    /// The weekly reflection for the week that contains `date` (§6.1). The
    /// cards come from the facts of that week at the time of the request.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn reflection(
        &self,
        household_id: Uuid,
        date: jiff::civil::Date,
    ) -> Result<(jiff::civil::Date, bool, Vec<crate::reflection::Card>)> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let monday = facts::week_start(facts::day_of(date));
        let summary = facts::week_summary(&mut tx, monday).await?;
        tx.commit().await?;
        Ok((
            facts::date_of(monday),
            summary.partial,
            crate::reflection::reflection(&summary),
        ))
    }

    /// The insights for a view, after the policy (§7.1).
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn list(&self, household_id: Uuid, view: View) -> Result<Vec<StoredInsight>> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let transaction = match view {
            View::Home => None,
            View::Transaction(id) => Some(id),
        };
        let rows: Vec<InsightRow> = sqlx::query_as(
            "SELECT id, rule_id, kind, scope, text, facts, relevance_bp, transaction_id, \
               created_at FROM app.insights \
             WHERE status = 'open' AND transaction_id IS NOT DISTINCT FROM $1",
        )
        .bind(transaction)
        .fetch_all(&mut *tx)
        .await?;
        let not_helpful: HashMap<String, i64> = sqlx::query_as::<_, (String, i64)>(
            "SELECT rule_id, count(*) FROM app.insight_feedback \
             WHERE kind = 'not_helpful' AND created_at > now() - interval '30 days' \
             GROUP BY rule_id",
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        tx.commit().await?;
        let items: Vec<StoredInsight> = rows
            .into_iter()
            .filter_map(|r| {
                // "Not helpful" lowers the relevance of the rule (§7.7).
                let penalty = not_helpful.get(&r.1).copied().unwrap_or(0) * 1500;
                Some(StoredInsight {
                    id: r.0,
                    kind: Kind::parse(&r.2)?,
                    relevance_bp: (i64::from(r.6) - penalty)
                        .clamp(0, 10_000)
                        .try_into()
                        .ok()?,
                    rule_id: r.1,
                    scope: r.3,
                    text: r.4,
                    facts: r.5,
                    transaction_id: r.7,
                    created_at: r.8.to_jiff(),
                })
            })
            .collect();
        Ok(policy::select(items))
    }

    /// Records feedback. Returns `true` if the rule is now muted.
    ///
    /// # Errors
    ///
    /// [`InsightError::NotFound`] or a database error.
    pub async fn feedback(
        &self,
        household_id: Uuid,
        insight_id: Uuid,
        feedback: Feedback,
    ) -> Result<bool> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let found: Option<(String, i32)> = sqlx::query_as(
            "SELECT rule_id, rule_version FROM app.insights WHERE id = $1 FOR UPDATE",
        )
        .bind(insight_id)
        .fetch_optional(&mut *tx)
        .await?;
        let (rule_id, version) = found.ok_or(InsightError::NotFound)?;
        let (kind, reason, status) = match feedback {
            Feedback::Wrong(r) => ("wrong", Some(r.as_str()), "wrong"),
            Feedback::NotHelpful => ("not_helpful", None, "dismissed"),
        };
        sqlx::query(
            "INSERT INTO app.insight_feedback (household_id, insight_id, rule_id, rule_version, \
               kind, reason) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(household_id)
        .bind(insight_id)
        .bind(&rule_id)
        .bind(version)
        .bind(kind)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE app.insights SET status = $2, updated_at = now() WHERE id = $1")
            .bind(insight_id)
            .bind(status)
            .execute(&mut *tx)
            .await?;
        let mut muted = false;
        if feedback == Feedback::NotHelpful {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM app.insight_feedback WHERE rule_id = $1 \
                   AND kind = 'not_helpful' AND created_at > now() - interval '30 days'",
            )
            .bind(&rule_id)
            .fetch_one(&mut *tx)
            .await?;
            if count >= MUTE_AFTER {
                sqlx::query(
                    "INSERT INTO app.insight_mutes (household_id, rule_id) VALUES ($1, $2) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(household_id)
                .bind(&rule_id)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE app.insights SET status = 'expired', updated_at = now() \
                     WHERE status = 'open' AND rule_id = $1",
                )
                .bind(&rule_id)
                .execute(&mut *tx)
                .await?;
                muted = true;
            }
        }
        tx.commit().await?;
        Ok(muted)
    }

    /// The muted rules.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn mutes(&self, household_id: Uuid) -> Result<Vec<String>> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rules = sqlx::query_scalar("SELECT rule_id FROM app.insight_mutes ORDER BY rule_id")
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(rules)
    }

    /// Unmutes a rule. Its "Not helpful" count starts again.
    ///
    /// # Errors
    ///
    /// [`InsightError::NotFound`] or a database error.
    pub async fn unmute(&self, household_id: Uuid, rule_id: &str) -> Result<()> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let deleted = sqlx::query("DELETE FROM app.insight_mutes WHERE rule_id = $1")
            .bind(rule_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if deleted == 0 {
            return Err(InsightError::NotFound);
        }
        sqlx::query("DELETE FROM app.insight_feedback WHERE rule_id = $1 AND kind = 'not_helpful'")
            .bind(rule_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Households whose daily run is due: 06:00 or later in the owner's time
    /// zone, and no run yet today (§7.2 triggers).
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn due_for_daily_run(&self) -> Result<Vec<Uuid>> {
        let rows: Vec<(Uuid, String, Option<jiff_sqlx::Date>)> = sqlx::query_as(
            "SELECT id, time_zone, insights_run_on FROM app.households_for_daily_run()",
        )
        .fetch_all(&self.pool)
        .await?;
        let now = jiff::Timestamp::now();
        Ok(rows
            .into_iter()
            .filter(|(_, zone, last)| {
                let local = now.to_zoned_datetime(facts::zone(Some(zone)));
                local.hour() >= 6 && last.is_none_or(|d| d.to_jiff() < local.date())
            })
            .map(|(id, _, _)| id)
            .collect())
    }
}
