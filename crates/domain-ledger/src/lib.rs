//! `domain-ledger`: transactions as the person sees them, and the person's
//! edits (design doc §9.1, §9.2).
//!
//! A transaction shows the provider data plus the enrichment
//! (`domain-categorize`) plus the person's edits (`app.user_overrides`),
//! applied last. Edits never change provider data. Splits divide one posted
//! transaction across categories.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::types::Json;
use uuid::Uuid;

/// The most parts in one split.
pub const MAX_SPLITS: usize = 10;

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    /// The transaction or category does not exist in this household.
    #[error("not found")]
    NotFound,
    /// The request is not valid.
    #[error("{message}")]
    Invalid {
        /// A stable error code (§11.4).
        code: &'static str,
        /// The text for the person.
        message: &'static str,
        /// The request field.
        param: &'static str,
    },
    /// A database error.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

const fn invalid(code: &'static str, message: &'static str, param: &'static str) -> LedgerError {
    LedgerError::Invalid {
        code,
        message,
        param,
    }
}

type Result<T, E = LedgerError> = std::result::Result<T, E>;

/// One part of a split.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Split {
    /// The split ID.
    pub id: Uuid,
    /// The category.
    pub category_id: Uuid,
    /// Same sign as the transaction.
    pub amount_minor: i64,
}

/// A new split part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewSplit {
    /// The category.
    pub category_id: Uuid,
    /// Same sign as the transaction, not zero.
    pub amount_minor: i64,
}

/// A transaction with enrichment and the person's edits.
#[derive(Debug, Clone, Serialize)]
pub struct Transaction {
    /// The transaction ID.
    pub id: Uuid,
    /// The account.
    pub account_id: Uuid,
    /// Negative is money out.
    pub amount_minor: i64,
    /// The currency code.
    pub currency: String,
    /// The bank date.
    pub date: jiff::civil::Date,
    /// The bank description.
    pub name: String,
    /// The normalized merchant.
    pub merchant_id: Option<Uuid>,
    /// The merchant name to show: the person's, the normalized, or the
    /// provider's.
    pub merchant_name: Option<String>,
    /// True while pending.
    pub pending: bool,
    /// The category: the person's, a rule's, or the provider mapping's.
    pub category_id: Option<Uuid>,
    /// `person`, `rule`, or `provider`.
    pub category_source: Option<String>,
    /// The person's note.
    pub note: Option<String>,
    /// Hidden from lists, budgets, and insights.
    pub hidden: bool,
    /// The parts, if split. Empty if not.
    pub splits: Vec<Split>,
}

/// The edit fields: category, merchant name, note, hidden.
type OverrideRow = (Option<Uuid>, Option<String>, Option<String>, bool);

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    account_id: Uuid,
    amount_minor: i64,
    currency: String,
    date: jiff_sqlx::Date,
    name: String,
    merchant_id: Option<Uuid>,
    merchant_name: Option<String>,
    pending: bool,
    category_id: Option<Uuid>,
    category_source: Option<String>,
    note: Option<String>,
    hidden: bool,
    splits: Option<Json<Vec<Split>>>,
}

impl From<Row> for Transaction {
    fn from(r: Row) -> Self {
        Self {
            id: r.id,
            account_id: r.account_id,
            amount_minor: r.amount_minor,
            currency: r.currency,
            date: r.date.to_jiff(),
            name: r.name,
            merchant_id: r.merchant_id,
            merchant_name: r.merchant_name,
            pending: r.pending,
            category_id: r.category_id,
            category_source: r.category_source,
            note: r.note,
            hidden: r.hidden,
            splits: r.splits.map_or_else(Vec::new, |s| s.0),
        }
    }
}

/// Which categories to list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CategoryFilter {
    /// All transactions.
    #[default]
    Any,
    /// Transactions without a category and without splits.
    Uncategorized,
    /// Transactions in this category, or with a split part in it.
    Id(Uuid),
}

/// Filters for the transaction list (§11.6).
#[derive(Debug, Clone, Default)]
pub struct TransactionFilter {
    /// One account.
    pub account_id: Option<Uuid>,
    /// A category.
    pub category: CategoryFilter,
    /// On or after this date.
    pub from: Option<jiff::civil::Date>,
    /// On or before this date.
    pub to: Option<jiff::civil::Date>,
    /// Text in the description, merchant, or note. Not case-sensitive.
    pub search: Option<String>,
    /// Also list hidden transactions.
    pub include_hidden: bool,
}

/// A change to the person's edits. `None` leaves a field as it is.
/// `Some(None)` clears it.
#[derive(Debug, Clone, Default)]
pub struct TransactionPatch {
    /// The category.
    pub category_id: Option<Option<Uuid>>,
    /// The merchant name to show.
    pub merchant_name: Option<Option<String>>,
    /// The note.
    pub note: Option<Option<String>>,
    /// Hidden.
    pub hidden: Option<bool>,
}

/// The select list and joins for a transaction as the person sees it.
const SELECT: &str = "SELECT t.id, t.account_id, t.amount_minor, t.currency, t.date, t.name, \
       t.merchant_id, coalesce(o.merchant_name, m.name, t.merchant_name) AS merchant_name, \
       t.pending, coalesce(o.category_id, t.category_id) AS category_id, \
       CASE WHEN o.category_id IS NOT NULL THEN 'person' ELSE t.category_source END \
         AS category_source, \
       o.note, coalesce(o.hidden, false) AS hidden, \
       (SELECT jsonb_agg(jsonb_build_object('id', s.id, 'category_id', s.category_id, \
                 'amount_minor', s.amount_minor) ORDER BY s.position) \
        FROM app.transaction_splits s WHERE s.transaction_id = t.id) AS splits \
     FROM app.transactions t \
     LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
     LEFT JOIN app.merchants m ON m.id = t.merchant_id ";

/// A `LIKE` pattern that matches `text` anywhere, with `%`, `_`, and `\`
/// escaped.
fn contains_pattern(text: &str) -> String {
    let mut pattern = String::with_capacity(text.len() + 2);
    pattern.push('%');
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

/// Trims and checks an optional text. Empty clears it.
fn clean_text(
    value: Option<String>,
    max: usize,
    error: LedgerError,
) -> Result<Option<String>, LedgerError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(error);
    }
    Ok(Some(value.to_owned()))
}

/// Transactions and the person's edits.
#[derive(Debug, Clone)]
pub struct LedgerService {
    pool: PgPool,
}

impl LedgerService {
    /// Creates the service.
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Lists transactions, newest first. `starting_after` is the last ID of
    /// the previous page (§11.5).
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn list_transactions(
        &self,
        household_id: Uuid,
        filter: &TransactionFilter,
        limit: u16,
        starting_after: Option<Uuid>,
    ) -> Result<Vec<Transaction>> {
        let (category, uncategorized) = match filter.category {
            CategoryFilter::Any => (None, false),
            CategoryFilter::Uncategorized => (None, true),
            CategoryFilter::Id(id) => (Some(id), false),
        };
        let search = filter
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(contains_pattern);
        let sql = format!(
            "{SELECT} \
             WHERE ($2::uuid IS NULL OR (t.date, t.id) < \
                     (SELECT c.date, c.id FROM app.transactions c WHERE c.id = $2)) \
               AND ($3::uuid IS NULL OR t.account_id = $3) \
               AND ($4::date IS NULL OR t.date >= $4) \
               AND ($5::date IS NULL OR t.date <= $5) \
               AND ($6 OR NOT coalesce(o.hidden, false)) \
               AND ($7::uuid IS NULL \
                    OR EXISTS (SELECT 1 FROM app.transaction_splits s \
                               WHERE s.transaction_id = t.id AND s.category_id = $7) \
                    OR (coalesce(o.category_id, t.category_id) = $7 \
                        AND NOT EXISTS (SELECT 1 FROM app.transaction_splits s \
                                        WHERE s.transaction_id = t.id))) \
               AND (NOT $8 OR (coalesce(o.category_id, t.category_id) IS NULL \
                    AND NOT EXISTS (SELECT 1 FROM app.transaction_splits s \
                                    WHERE s.transaction_id = t.id))) \
               AND ($9::text IS NULL OR t.name ILIKE $9 OR t.merchant_name ILIKE $9 \
                    OR m.name ILIKE $9 OR o.merchant_name ILIKE $9 OR o.note ILIKE $9) \
             ORDER BY t.date DESC, t.id DESC \
             LIMIT $1"
        );
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(i64::from(limit))
            .bind(starting_after)
            .bind(filter.account_id)
            .bind(filter.from.map(jiff_sqlx::Date::from))
            .bind(filter.to.map(jiff_sqlx::Date::from))
            .bind(filter.include_hidden)
            .bind(category)
            .bind(uncategorized)
            .bind(search)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(rows.into_iter().map(Transaction::from).collect())
    }

    /// Gets one transaction.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] or a database error.
    pub async fn get_transaction(&self, household_id: Uuid, id: Uuid) -> Result<Transaction> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let found = Self::fetch(&mut tx, id).await?;
        tx.commit().await?;
        found.ok_or(LedgerError::NotFound)
    }

    async fn fetch(conn: &mut sqlx::PgConnection, id: Uuid) -> Result<Option<Transaction>> {
        let sql = format!("{SELECT} WHERE t.id = $1");
        let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(conn)
            .await?;
        Ok(row.map(Transaction::from))
    }

    /// The category exists in this household. Foreign keys do not apply
    /// row-level security, so this check is needed (§12.2).
    async fn check_categories(conn: &mut sqlx::PgConnection, ids: &[Uuid]) -> Result<()> {
        let mut unique = ids.to_vec();
        unique.sort_unstable();
        unique.dedup();
        let found: i64 =
            sqlx::query_scalar("SELECT count(*) FROM app.categories WHERE id = ANY($1)")
                .bind(&unique)
                .fetch_one(conn)
                .await?;
        if usize::try_from(found).ok() == Some(unique.len()) {
            Ok(())
        } else {
            Err(invalid(
                "category_invalid",
                "That category does not exist.",
                "category_id",
            ))
        }
    }

    /// Changes the person's edits.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`], [`LedgerError::Invalid`], or a database
    /// error.
    pub async fn update_transaction(
        &self,
        household_id: Uuid,
        id: Uuid,
        patch: TransactionPatch,
    ) -> Result<Transaction> {
        let merchant_name = patch
            .merchant_name
            .map(|v| {
                clean_text(
                    v,
                    100,
                    invalid(
                        "merchant_name_invalid",
                        "Use up to 100 characters.",
                        "merchant_name",
                    ),
                )
            })
            .transpose()?;
        let note = patch
            .note
            .map(|v| {
                clean_text(
                    v,
                    500,
                    invalid("note_invalid", "Use up to 500 characters.", "note"),
                )
            })
            .transpose()?;

        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM app.transactions WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(LedgerError::NotFound);
        }
        if let Some(Some(category_id)) = patch.category_id {
            Self::check_categories(&mut tx, &[category_id]).await?;
        }
        let current: Option<OverrideRow> = sqlx::query_as(
            "SELECT category_id, merchant_name, note, hidden FROM app.user_overrides \
                 WHERE transaction_id = $1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let (mut category_id, mut merchant, mut text, mut hidden) = current.unwrap_or_default();
        if let Some(v) = patch.category_id {
            category_id = v;
        }
        if let Some(v) = merchant_name {
            merchant = v;
        }
        if let Some(v) = note {
            text = v;
        }
        if let Some(v) = patch.hidden {
            hidden = v;
        }

        if category_id.is_none() && merchant.is_none() && text.is_none() && !hidden {
            sqlx::query("DELETE FROM app.user_overrides WHERE transaction_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query(
                "INSERT INTO app.user_overrides (transaction_id, household_id, category_id, \
                   merchant_name, note, hidden) VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (transaction_id) DO UPDATE SET category_id = EXCLUDED.category_id, \
                   merchant_name = EXCLUDED.merchant_name, note = EXCLUDED.note, \
                   hidden = EXCLUDED.hidden, updated_at = now()",
            )
            .bind(id)
            .bind(household_id)
            .bind(category_id)
            .bind(&merchant)
            .bind(&text)
            .bind(hidden)
            .execute(&mut *tx)
            .await?;
        }
        let updated = Self::fetch(&mut tx, id)
            .await?
            .ok_or(LedgerError::NotFound)?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Replaces the split parts. An empty list removes the split.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`], [`LedgerError::Invalid`], or a database
    /// error.
    pub async fn set_splits(
        &self,
        household_id: Uuid,
        id: Uuid,
        parts: &[NewSplit],
    ) -> Result<Transaction> {
        if parts.len() == 1 || parts.len() > MAX_SPLITS {
            return Err(invalid(
                "splits_count",
                "Use 2 to 10 parts, or none to remove the split.",
                "splits",
            ));
        }
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let found: Option<(i64, bool)> = sqlx::query_as(
            "SELECT amount_minor, pending FROM app.transactions WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let (amount, pending) = found.ok_or(LedgerError::NotFound)?;
        if !parts.is_empty() {
            if pending {
                return Err(invalid(
                    "transaction_pending",
                    "Split this transaction after it posts.",
                    "splits",
                ));
            }
            let same_sign = |p: &NewSplit| p.amount_minor.signum() == amount.signum();
            if amount == 0 || !parts.iter().all(same_sign) {
                return Err(invalid(
                    "split_amount_invalid",
                    "Each part must be more than zero.",
                    "splits",
                ));
            }
            let total: i128 = parts.iter().map(|p| i128::from(p.amount_minor)).sum();
            if total != i128::from(amount) {
                return Err(invalid(
                    "splits_total",
                    "The parts must add up to the transaction amount.",
                    "splits",
                ));
            }
            let categories: Vec<Uuid> = parts.iter().map(|p| p.category_id).collect();
            Self::check_categories(&mut tx, &categories).await?;
        }
        sqlx::query("DELETE FROM app.transaction_splits WHERE transaction_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for (position, part) in (0_i32..).zip(parts) {
            sqlx::query(
                "INSERT INTO app.transaction_splits (household_id, transaction_id, category_id, \
                   amount_minor, position) VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(household_id)
            .bind(id)
            .bind(part.category_id)
            .bind(part.amount_minor)
            .bind(position)
            .execute(&mut *tx)
            .await?;
        }
        let updated = Self::fetch(&mut tx, id)
            .await?
            .ok_or(LedgerError::NotFound)?;
        tx.commit().await?;
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_like_patterns() {
        assert_eq!(contains_pattern("joe's"), "%joe's%");
        assert_eq!(contains_pattern("50%_off\\"), "%50\\%\\_off\\\\%");
    }

    #[test]
    fn cleans_text() {
        let e = || invalid("x", "x", "x");
        assert_eq!(clean_text(None, 5, e()).unwrap(), None);
        assert_eq!(clean_text(Some("  ".into()), 5, e()).unwrap(), None);
        assert_eq!(
            clean_text(Some(" hi ".into()), 5, e()).unwrap(),
            Some("hi".into())
        );
        assert!(clean_text(Some("toolong".into()), 5, e()).is_err());
        assert!(clean_text(Some("a\u{0}b".into()), 5, e()).is_err());
    }
}
