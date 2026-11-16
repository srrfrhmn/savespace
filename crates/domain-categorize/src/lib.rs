//! `domain-categorize`: merchant normalization, categorization rules, and the
//! category model (design doc §10.2 Enrich, §10.3, D14).
//!
//! - [`normalize_merchant`] turns a bank description into a merchant key.
//! - [`provider_category`] maps the provider category to a seeded category.
//! - [`CategorizeService::enrich_household`] runs after each sync (the
//!   `transactions.changed` outbox event). It sets the merchant and the
//!   automatic category of each new or changed transaction. Order: the
//!   person's rules, then the provider mapping. A person's edit
//!   (`app.user_overrides`) wins over both and is never changed here.
//! - A new or deleted rule enriches the household again, so rules apply to
//!   past and future transactions.

mod mapping;
mod merchant;
mod rules;

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

pub use mapping::{mapped_keys, provider_category};
pub use merchant::{Merchant, known_prefix, normalize_merchant};
pub use rules::{CompiledRule, Decision, Direction, Facts, RuleMatch, RuleMatchError, decide};

/// The job kind that enriches one household's new transactions.
pub const ENRICH_JOB: &str = "categorize.household";

/// The outbox topic after transactions got a merchant or category.
pub const TRANSACTIONS_CATEGORIZED: &str = "transactions.categorized";

/// A household can have this many rules.
pub const MAX_RULES: i64 = 500;

/// Transactions per enrich batch.
const BATCH: i64 = 500;

/// A stop for the batch loop: 500 × 400 = 200,000 transactions.
const MAX_BATCHES: usize = 400;

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum CategorizeError {
    /// The rule, category, or merchant does not exist in this household.
    #[error("not found")]
    NotFound,
    /// The rule is not valid.
    #[error(transparent)]
    InvalidRule(#[from] RuleMatchError),
    /// The household has [`MAX_RULES`] rules.
    #[error("too many rules")]
    TooManyRules,
    /// A database error.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

type Result<T, E = CategorizeError> = std::result::Result<T, E>;

/// A category.
#[derive(Debug, Clone, Serialize)]
pub struct Category {
    /// The category ID.
    pub id: Uuid,
    /// The seed key, for example `groceries`. `None` for the person's own.
    pub key: Option<String>,
    /// The name.
    pub name: String,
    /// `fixed`, `flex`, `non_monthly`, `income`, or `transfer`.
    pub kind: String,
    /// True for health, giving, and similar.
    pub sensitive: bool,
}

/// A rule.
#[derive(Debug, Clone, Serialize)]
pub struct Rule {
    /// The rule ID.
    pub id: Uuid,
    /// Higher wins.
    pub priority: i32,
    /// What it matches.
    pub matcher: RuleMatch,
    /// The category it sets.
    pub category_id: Uuid,
    /// The merchant name, when the rule matches a merchant.
    pub merchant_name: Option<String>,
    /// How many transactions it categorizes now. A person's own category
    /// on a transaction wins, so those do not count.
    pub transactions: i64,
    /// When it was made.
    pub created_at: jiff::Timestamp,
}

/// Categories, merchants, and rules.
#[derive(Debug, Clone)]
pub struct CategorizeService {
    pool: PgPool,
}

type RuleRow = (
    Uuid,
    i32,
    sqlx::types::Json<RuleMatch>,
    Uuid,
    Option<String>,
    i64,
    jiff_sqlx::Timestamp,
);

type EnrichRow = (
    Uuid,
    i32,
    String,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
);

impl CategorizeService {
    /// Creates the service.
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Lists categories in display order.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn list_categories(&self, household_id: Uuid) -> Result<Vec<Category>> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rows: Vec<(Uuid, Option<String>, String, String, bool)> = sqlx::query_as(
            "SELECT id, key, name, kind, sensitive FROM app.categories \
             ORDER BY position, name",
        )
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(|(id, key, name, kind, sensitive)| Category {
                id,
                key,
                name,
                kind,
                sensitive,
            })
            .collect())
    }

    /// Lists rules, highest priority first.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn list_rules(&self, household_id: Uuid) -> Result<Vec<Rule>> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rules = Self::rules(&mut tx, None).await?;
        tx.commit().await?;
        Ok(rules)
    }

    async fn rules(conn: &mut sqlx::PgConnection, only: Option<Uuid>) -> Result<Vec<Rule>> {
        let rows: Vec<RuleRow> = sqlx::query_as(
            "SELECT r.id, r.priority, r.match, r.category_id, m.name, \
               (SELECT count(*) FROM app.transactions t WHERE t.category_rule_id = r.id \
                  AND NOT EXISTS (SELECT 1 FROM app.user_overrides o \
                                  WHERE o.transaction_id = t.id AND o.category_id IS NOT NULL)), \
               r.created_at \
             FROM app.rules r \
             LEFT JOIN app.merchants m ON m.id = (r.match->>'merchant_id')::uuid \
             WHERE $1::uuid IS NULL OR r.id = $1 \
             ORDER BY r.priority DESC, r.id DESC",
        )
        .bind(only)
        .fetch_all(conn)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| Rule {
                id: r.0,
                priority: r.1,
                matcher: r.2.0,
                category_id: r.3,
                merchant_name: r.4,
                transactions: r.5,
                created_at: r.6.to_jiff(),
            })
            .collect())
    }

    /// Creates a rule with the highest priority, then applies the rules to
    /// all transactions again.
    ///
    /// # Errors
    ///
    /// [`CategorizeError::InvalidRule`], [`CategorizeError::NotFound`] for a
    /// category or merchant of another household, [`CategorizeError::TooManyRules`],
    /// or a database error.
    pub async fn create_rule(
        &self,
        household_id: Uuid,
        matcher: RuleMatch,
        category_id: Uuid,
    ) -> Result<Rule> {
        let matcher = matcher.validated()?;
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        // Foreign keys do not apply row-level security, so check the
        // household here (§12.2).
        let category: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM app.categories WHERE id = $1")
                .bind(category_id)
                .fetch_optional(&mut *tx)
                .await?;
        if category.is_none() {
            return Err(CategorizeError::NotFound);
        }
        if let Some(merchant_id) = matcher.merchant_id {
            let merchant: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM app.merchants WHERE id = $1")
                    .bind(merchant_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if merchant.is_none() {
                return Err(CategorizeError::NotFound);
            }
        }
        // One rule per target: a new rule for the same merchant or text
        // replaces the old one.
        sqlx::query("DELETE FROM app.rules WHERE match = $1")
            .bind(sqlx::types::Json(&matcher))
            .execute(&mut *tx)
            .await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM app.rules")
            .fetch_one(&mut *tx)
            .await?;
        if count >= MAX_RULES {
            return Err(CategorizeError::TooManyRules);
        }
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO app.rules (household_id, priority, match, category_id) \
             SELECT $1, coalesce(max(priority), 0) + 1, $2, $3 FROM app.rules \
             RETURNING id",
        )
        .bind(household_id)
        .bind(sqlx::types::Json(&matcher))
        .bind(category_id)
        .fetch_one(&mut *tx)
        .await?;
        Self::reset(&mut tx).await?;
        tx.commit().await?;

        self.enrich_household(household_id).await?;
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rule = Self::rules(&mut tx, Some(id))
            .await?
            .pop()
            .ok_or(CategorizeError::NotFound)?;
        tx.commit().await?;
        Ok(rule)
    }

    /// Deletes a rule, then applies the other rules to all transactions again.
    ///
    /// # Errors
    ///
    /// [`CategorizeError::NotFound`] or a database error.
    pub async fn delete_rule(&self, household_id: Uuid, rule_id: Uuid) -> Result<()> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let deleted = sqlx::query("DELETE FROM app.rules WHERE id = $1")
            .bind(rule_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if deleted == 0 {
            return Err(CategorizeError::NotFound);
        }
        Self::reset(&mut tx).await?;
        tx.commit().await?;
        self.enrich_household(household_id).await?;
        Ok(())
    }

    /// Marks every transaction for enrichment.
    async fn reset(conn: &mut sqlx::PgConnection) -> Result<()> {
        sqlx::query("UPDATE app.transactions SET enriched_version = NULL")
            .execute(conn)
            .await?;
        Ok(())
    }

    /// Sets the merchant and automatic category of each transaction that
    /// needs it. Idempotent. Returns how many transactions it updated.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn enrich_household(&self, household_id: Uuid) -> Result<u64> {
        let mut updated = 0;
        for _ in 0..MAX_BATCHES {
            let (selected, n) = self.enrich_batch(household_id).await?;
            updated += n;
            if selected < BATCH {
                return Ok(updated);
            }
        }
        tracing::warn!(%household_id, "enrich stopped after the batch limit");
        Ok(updated)
    }

    async fn enrich_batch(&self, household_id: Uuid) -> Result<(i64, u64)> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rows: Vec<EnrichRow> = sqlx::query_as(
            "SELECT id, raw_version, name, merchant_name, amount_minor, \
               provider_category_primary, provider_category_detailed \
             FROM app.transactions WHERE enriched_version IS NULL \
             ORDER BY id LIMIT $1",
        )
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        if rows.is_empty() {
            tx.commit().await?;
            return Ok((0, 0));
        }
        let selected = i64::try_from(rows.len()).unwrap_or(i64::MAX);

        let categories: HashMap<String, Uuid> = sqlx::query_as::<_, (String, Uuid)>(
            "SELECT key, id FROM app.categories WHERE key IS NOT NULL",
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
        let rules = Self::compiled_rules(&mut tx).await?;

        let found = Self::resolve_merchants(&mut tx, &rows).await?;
        let merchant_ids = Self::upsert_merchants(&mut tx, household_id, &found).await?;

        let changes: Vec<Value> = rows
            .iter()
            .zip(&found)
            .map(|(r, merchant)| {
                let merchant_id = merchant
                    .as_ref()
                    .and_then(|m| merchant_ids.get(&m.key))
                    .copied();
                let facts = Facts {
                    merchant_id,
                    name: &r.2,
                    merchant_name: r
                        .3
                        .as_deref()
                        .or(merchant.as_ref().map(|m| m.name.as_str())),
                    amount_minor: r.4,
                };
                let provider = provider_category(r.5.as_deref(), r.6.as_deref())
                    .and_then(|key| categories.get(key))
                    .copied();
                let (category_id, source, rule_id) = match decide(&facts, &rules, provider) {
                    Decision::Rule {
                        category_id,
                        rule_id,
                    } => (Some(category_id), Some("rule"), Some(rule_id)),
                    Decision::Provider { category_id } => {
                        (Some(category_id), Some("provider"), None)
                    }
                    Decision::None => (None, None, None),
                };
                json!({
                    "id": r.0,
                    "version": r.1,
                    "merchant_id": merchant_id,
                    "category_id": category_id,
                    "source": source,
                    "rule_id": rule_id,
                })
            })
            .collect();
        // The version check skips a row that a sync changed meanwhile. The
        // sync cleared its enriched_version, so the next batch takes it.
        let updated = sqlx::query(
            "UPDATE app.transactions t SET merchant_id = r.merchant_id, \
               category_id = r.category_id, category_source = r.source, \
               category_rule_id = r.rule_id, enriched_version = r.version \
             FROM jsonb_to_recordset($1) AS r(id uuid, version integer, merchant_id uuid, \
               category_id uuid, source text, rule_id uuid) \
             WHERE t.id = r.id AND t.raw_version = r.version",
        )
        .bind(Value::from(changes))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated > 0 {
            sqlx::query(
                "INSERT INTO app.outbox (topic, household_id, payload) VALUES ($1, $2, $3)",
            )
            .bind(TRANSACTIONS_CATEGORIZED)
            .bind(household_id)
            .bind(json!({ "updated": updated }))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok((selected, updated))
    }

    /// The household's rules, highest priority first.
    async fn compiled_rules(conn: &mut sqlx::PgConnection) -> Result<Vec<CompiledRule>> {
        let rows: Vec<(Uuid, sqlx::types::Json<RuleMatch>, Uuid)> = sqlx::query_as(
            "SELECT id, match, category_id FROM app.rules ORDER BY priority DESC, id DESC",
        )
        .fetch_all(conn)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, matcher, category_id)| CompiledRule {
                id,
                matcher: matcher.0,
                category_id,
            })
            .collect())
    }

    /// Normalizes the merchant of each row. A description that starts with a
    /// provider merchant name (stored, or in this batch) joins that merchant.
    async fn resolve_merchants(
        conn: &mut sqlx::PgConnection,
        rows: &[EnrichRow],
    ) -> Result<Vec<Option<Merchant>>> {
        let mut found: Vec<Option<Merchant>> = rows
            .iter()
            .map(|r| normalize_merchant(&r.2, r.3.as_deref()))
            .collect();
        let mut known: BTreeMap<String, String> = sqlx::query_as::<_, (String, String)>(
            "SELECT key, name FROM app.merchants WHERE from_provider",
        )
        .fetch_all(conn)
        .await?
        .into_iter()
        .collect();
        for m in found.iter().flatten().filter(|m| m.from_provider) {
            known.entry(m.key.clone()).or_insert_with(|| m.name.clone());
        }
        for m in found.iter_mut().flatten().filter(|m| !m.from_provider) {
            if let Some(key) = known_prefix(&m.key, known.keys().map(String::as_str)) {
                m.name.clone_from(&known[key]);
                m.key = key.to_owned();
            }
        }
        Ok(found)
    }

    /// Upserts the merchants of one batch. Returns the ID for each key.
    async fn upsert_merchants(
        conn: &mut sqlx::PgConnection,
        household_id: Uuid,
        found: &[Option<Merchant>],
    ) -> Result<HashMap<String, Uuid>> {
        // A key appears once: one upsert cannot change a row twice. A
        // provider name wins over a name made from a description.
        let mut unique: BTreeMap<&str, (&str, bool)> = BTreeMap::new();
        for m in found.iter().flatten() {
            let entry = unique
                .entry(m.key.as_str())
                .or_insert((m.name.as_str(), m.from_provider));
            if m.from_provider && !entry.1 {
                *entry = (m.name.as_str(), true);
            }
        }
        if unique.is_empty() {
            return Ok(HashMap::new());
        }
        let input: Vec<Value> = unique
            .iter()
            .map(|(key, (name, from_provider))| {
                json!({ "key": key, "name": name, "from_provider": from_provider })
            })
            .collect();
        Ok(sqlx::query_as::<_, (String, Uuid)>(
            "INSERT INTO app.merchants AS m (household_id, key, name, from_provider) \
             SELECT $1, r.key, r.name, r.from_provider \
             FROM jsonb_to_recordset($2) AS r(key text, name text, from_provider boolean) \
             ON CONFLICT (household_id, key) DO UPDATE SET \
               name = CASE WHEN EXCLUDED.from_provider AND NOT m.from_provider \
                           THEN EXCLUDED.name ELSE m.name END, \
               from_provider = m.from_provider OR EXCLUDED.from_provider \
             RETURNING m.key, m.id",
        )
        .bind(household_id)
        .bind(Value::from(input))
        .fetch_all(conn)
        .await?
        .into_iter()
        .collect())
    }
}
