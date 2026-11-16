//! `domain-recurring`: recurring charges and income (design doc §6.1, §10.2).
//!
//! [`RecurringService::detect_household`] runs after enrichment (the
//! `transactions.categorized` outbox event) and after a person's edit. It
//! groups the last 400 days of posted, visible transactions by merchant and
//! direction, runs `savespace_core::recurring::detect`, and replaces the
//! household's series. Transfers are not recurring charges.

use std::collections::{BTreeMap, HashMap};

use savespace_core::Day;
use savespace_core::recurring::{self, Cadence, EXPECTED_CONFIDENCE};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

/// The job kind that finds one household's series.
pub const DETECT_JOB: &str = "recurring.household";

/// The outbox topic after a household's series were found again. It starts
/// the insight pipeline.
pub const RECURRING_UPDATED: &str = "recurring.updated";

/// How far back detection looks.
const LOOKBACK_DAYS: i32 = 400;

/// Series below this confidence are not stored (0.5).
const MIN_CONFIDENCE_BP: i32 = 5000;

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum RecurringError {
    /// A database error.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// The stored time zone is not valid.
    #[error("time zone: {0}")]
    TimeZone(String),
}

type Result<T, E = RecurringError> = std::result::Result<T, E>;

/// A recurring series.
#[derive(Debug, Clone, Serialize)]
pub struct Series {
    /// The series ID.
    pub id: Uuid,
    /// The merchant.
    pub merchant_id: Uuid,
    /// The merchant name.
    pub merchant_name: String,
    /// `in` or `out`.
    pub direction: String,
    /// The currency code.
    pub currency: String,
    /// `weekly`, `biweekly`, `monthly`, or `annual`.
    pub cadence: String,
    /// The typical amount. Negative is money out.
    pub amount_minor: i64,
    /// The usual distance from the typical amount.
    pub spread_minor: i64,
    /// The latest date.
    pub last_date: jiff::civil::Date,
    /// The next expected date.
    pub next_date: jiff::civil::Date,
    /// How many transactions match.
    pub occurrences: i32,
    /// 0 to 10,000.
    pub confidence_bp: i32,
    /// True if the series is still going.
    pub active: bool,
    /// True if the confidence is high enough to call it "expected" (§7.4.3).
    pub expected: bool,
    /// The most common category.
    pub category_id: Option<Uuid>,
}

/// Converts a database date to a core day.
pub fn day_of(date: jiff::civil::Date) -> Day {
    Day::from_ymd(
        i32::from(date.year()),
        u8::try_from(date.month()).unwrap_or(1),
        u8::try_from(date.day()).unwrap_or(1),
    )
    .unwrap_or(Day(0))
}

/// Converts a core day to a date.
pub fn date_of(day: Day) -> jiff::civil::Date {
    let (y, m, d) = day.ymd();
    jiff::civil::Date::new(
        i16::try_from(y).unwrap_or(1970),
        i8::try_from(m).unwrap_or(1),
        i8::try_from(d).unwrap_or(1),
    )
    .unwrap_or_default()
}

/// Today in the household owner's time zone.
///
/// # Errors
///
/// A database error, or a stored time zone that does not exist.
pub async fn today(conn: &mut sqlx::PgConnection) -> Result<jiff::civil::Date> {
    let zone: Option<String> = sqlx::query_scalar(
        "SELECT u.time_zone FROM app.users u \
         JOIN app.household_members m ON m.user_id = u.id \
         WHERE m.household_id = app.current_household_id() \
         ORDER BY m.created_at LIMIT 1",
    )
    .fetch_optional(conn)
    .await?;
    let zone = zone.unwrap_or_else(|| "America/New_York".to_owned());
    let tz = jiff::tz::TimeZone::get(&zone).map_err(|e| RecurringError::TimeZone(e.to_string()))?;
    Ok(jiff::Timestamp::now().to_zoned_datetime(tz).date())
}

type Row = (Uuid, Uuid, jiff_sqlx::Date, i64, String, Option<Uuid>);

/// One group: a merchant, a direction, a currency.
type Key = (Uuid, &'static str, String);

/// Recurring series.
#[derive(Debug, Clone)]
pub struct RecurringService {
    pool: PgPool,
}

impl RecurringService {
    /// Creates the service.
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Finds the household's series and replaces the stored ones. Returns
    /// how many series it stored.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn detect_household(&self, household_id: Uuid) -> Result<usize> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let today = today(&mut tx).await?;
        let today_day = day_of(today);
        let since = date_of(today_day.plus_days(-LOOKBACK_DAYS));
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT t.id, t.merchant_id, t.date, t.amount_minor, t.currency, \
               coalesce(o.category_id, t.category_id) \
             FROM app.transactions t \
             LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
             LEFT JOIN app.categories c ON c.id = coalesce(o.category_id, t.category_id) \
             WHERE NOT t.pending AND t.merchant_id IS NOT NULL AND t.amount_minor <> 0 \
               AND NOT coalesce(o.hidden, false) AND t.date >= $1 \
               AND (c.kind IS NULL OR c.kind <> 'transfer')",
        )
        .bind(jiff_sqlx::Date::from(since))
        .fetch_all(&mut *tx)
        .await?;

        let (series, members) = find(&rows, today_day);

        // Replace: keep IDs of series that still exist, delete the others.
        let stored: Vec<(String, Uuid)> = sqlx::query_as(
            "WITH input AS ( \
               SELECT * FROM jsonb_to_recordset($2) AS r(key text, merchant_id uuid, \
                 direction text, currency text, cadence text, amount bigint, spread bigint, \
                 last date, next date, occurrences integer, confidence integer, \
                 active boolean, category_id uuid)), \
             gone AS ( \
               DELETE FROM app.recurring_series s WHERE NOT EXISTS (SELECT 1 FROM input i \
                 WHERE i.merchant_id = s.merchant_id AND i.direction = s.direction \
                   AND i.currency = s.currency)) \
             INSERT INTO app.recurring_series AS s (household_id, merchant_id, direction, \
               currency, cadence, amount_minor, spread_minor, last_date, next_date, \
               occurrences, confidence_bp, active, category_id) \
             SELECT $1, merchant_id, direction, currency, cadence, amount, spread, last, next, \
               occurrences, confidence, active, category_id FROM input \
             ON CONFLICT (household_id, merchant_id, direction, currency) DO UPDATE SET \
               cadence = EXCLUDED.cadence, amount_minor = EXCLUDED.amount_minor, \
               spread_minor = EXCLUDED.spread_minor, last_date = EXCLUDED.last_date, \
               next_date = EXCLUDED.next_date, occurrences = EXCLUDED.occurrences, \
               confidence_bp = EXCLUDED.confidence_bp, active = EXCLUDED.active, \
               category_id = EXCLUDED.category_id, updated_at = now() \
             RETURNING s.merchant_id::text || '/' || s.direction || '/' || s.currency, s.id",
        )
        .bind(household_id)
        .bind(Value::from(series))
        .fetch_all(&mut *tx)
        .await?;
        let ids: HashMap<String, Uuid> = stored.into_iter().collect();
        let links: Vec<Value> = members
            .into_iter()
            .filter_map(|m| {
                let id = ids.get(m["key"].as_str()?)?;
                Some(json!({ "id": m["id"], "series": id }))
            })
            .collect();
        sqlx::query(
            "UPDATE app.transactions t SET recurring_series_id = l.series \
             FROM (SELECT t2.id, r.series FROM app.transactions t2 \
                   LEFT JOIN jsonb_to_recordset($1) AS r(id uuid, series uuid) ON r.id = t2.id) l \
             WHERE t.id = l.id AND t.recurring_series_id IS DISTINCT FROM l.series",
        )
        .bind(Value::from(links))
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO app.outbox (topic, household_id, payload) VALUES ($1, $2, $3)")
            .bind(RECURRING_UPDATED)
            .bind(household_id)
            .bind(json!({ "series": ids.len() }))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(ids.len())
    }

    /// Lists series: active first, then by next date.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn list(&self, household_id: Uuid) -> Result<Vec<Series>> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rows: Vec<SeriesRow> = sqlx::query_as(
            "SELECT s.id, s.merchant_id, m.name, s.direction, s.currency, s.cadence, \
               s.amount_minor, s.spread_minor, s.last_date, s.next_date, s.occurrences, \
               s.confidence_bp, s.active, s.category_id \
             FROM app.recurring_series s JOIN app.merchants m ON m.id = s.merchant_id \
             ORDER BY s.active DESC, s.next_date, m.name",
        )
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.into_iter().map(Series::from).collect())
    }
}

/// Runs detection on each merchant group. Returns the series and the
/// transactions that belong to them, as JSON for the bulk statements.
fn find(rows: &[Row], today_day: Day) -> (Vec<Value>, Vec<Value>) {
    let mut groups: BTreeMap<Key, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        let direction = if row.3 < 0 { "out" } else { "in" };
        groups
            .entry((row.1, direction, row.4.clone()))
            .or_default()
            .push(row);
    }

    let mut series = Vec::new();
    let mut members = Vec::new();
    for ((merchant, direction, currency), items) in &groups {
        let points: Vec<(Day, i64)> = items.iter().map(|r| (day_of(r.2.to_jiff()), r.3)).collect();
        let Some(found) = recurring::detect(&points, today_day) else {
            continue;
        };
        if found.confidence.0 < MIN_CONFIDENCE_BP {
            continue;
        }
        let fitting: Vec<&&Row> = items
            .iter()
            .filter(|r| recurring::amount_fits(found.amount, found.spread, r.3))
            .collect();
        let mut counts: HashMap<Uuid, usize> = HashMap::new();
        for r in &fitting {
            if let Some(c) = r.5 {
                *counts.entry(c).or_default() += 1;
            }
        }
        let category = counts
            .into_iter()
            .max_by_key(|(id, n)| (*n, *id))
            .map(|(id, _)| id);
        let key = format!("{merchant}/{direction}/{currency}");
        series.push(json!({
            "key": key,
            "merchant_id": merchant,
            "direction": direction,
            "currency": currency,
            "cadence": found.cadence.as_str(),
            "amount": found.amount,
            "spread": i64::try_from(found.spread).unwrap_or(i64::MAX),
            "last": date_of(found.last).to_string(),
            "next": date_of(found.next).to_string(),
            "occurrences": i32::try_from(found.occurrences).unwrap_or(i32::MAX),
            "confidence": found.confidence.0,
            "active": found.active,
            "category_id": category,
        }));
        members.extend(fitting.iter().map(|r| json!({ "id": r.0, "key": key })));
    }
    (series, members)
}

#[derive(sqlx::FromRow)]
struct SeriesRow {
    id: Uuid,
    merchant_id: Uuid,
    name: String,
    direction: String,
    currency: String,
    cadence: String,
    amount_minor: i64,
    spread_minor: i64,
    last_date: jiff_sqlx::Date,
    next_date: jiff_sqlx::Date,
    occurrences: i32,
    confidence_bp: i32,
    active: bool,
    category_id: Option<Uuid>,
}

impl From<SeriesRow> for Series {
    fn from(r: SeriesRow) -> Self {
        Self {
            id: r.id,
            merchant_id: r.merchant_id,
            merchant_name: r.name,
            direction: r.direction,
            currency: r.currency,
            cadence: r.cadence,
            amount_minor: r.amount_minor,
            spread_minor: r.spread_minor,
            last_date: r.last_date.to_jiff(),
            next_date: r.next_date.to_jiff(),
            occurrences: r.occurrences,
            confidence_bp: r.confidence_bp,
            active: r.active,
            expected: r.active && r.confidence_bp >= EXPECTED_CONFIDENCE.0,
            category_id: r.category_id,
        }
    }
}

/// The dates a series is expected on, from `from` to `to` (both included).
pub fn occurrences_between(
    cadence: &str,
    next: jiff::civil::Date,
    from: jiff::civil::Date,
    to: jiff::civil::Date,
) -> Vec<jiff::civil::Date> {
    let Some(cadence) = Cadence::parse(cadence) else {
        return Vec::new();
    };
    let (from, to) = (day_of(from), day_of(to));
    let mut day = day_of(next);
    let mut out = Vec::new();
    while day <= to && out.len() < 64 {
        if day >= from {
            out.push(date_of(day));
        }
        day = cadence.next_after(day);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_dates() {
        let d = jiff::civil::date(2026, 11, 16);
        assert_eq!(date_of(day_of(d)), d);
    }

    #[test]
    fn lists_occurrences_in_a_window() {
        let d = |m, day| jiff::civil::date(2026, m, day);
        assert_eq!(
            occurrences_between("weekly", d(11, 3), d(11, 16), d(11, 30)),
            [d(11, 17), d(11, 24)]
        );
        assert_eq!(
            occurrences_between("monthly", d(11, 20), d(11, 16), d(11, 30)),
            [d(11, 20)]
        );
        assert_eq!(
            occurrences_between("monthly", d(12, 1), d(11, 16), d(11, 30)),
            []
        );
        assert_eq!(
            occurrences_between("nope", d(11, 20), d(11, 16), d(11, 30)),
            []
        );
    }
}
