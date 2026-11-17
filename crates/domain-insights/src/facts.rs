//! The fact builder (design doc §7.2 step 1, §7.3).
//!
//! Reads one household inside its tenant transaction. Spending counts the
//! same way as the budget: flexible categories, hidden transactions left out,
//! a split counted in each part's category, refunds lowering spending.
//! The flexible plan numbers come from the caller (the budget in
//! `domain-ledger`), so "left" in an insight equals "left" on the Budget page.

use std::collections::HashMap;

use savespace_core::recurring::{Cadence, EXPECTED_CONFIDENCE};
use savespace_core::{Currency, Day};
use uuid::Uuid;

use crate::rules::{BalanceFacts, FactSet, MonthFacts, SeriesCharge, Upcoming, WeekFacts};

/// Weekly history looks back at most this many weeks.
const MAX_WEEKS: i32 = 12;

/// The flexible budget for this month, from the Budget page numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonthInput {
    /// The plans plus carried amounts, over planned flexible categories.
    pub plan: i64,
    /// Left of those plans.
    pub left: i64,
}

/// A date as a core day.
pub fn day_of(date: jiff::civil::Date) -> Day {
    Day::from_ymd(
        i32::from(date.year()),
        u8::try_from(date.month()).unwrap_or(1),
        u8::try_from(date.day()).unwrap_or(1),
    )
    .unwrap_or(Day(0))
}

/// A core day as a date.
pub fn date_of(day: Day) -> jiff::civil::Date {
    let (y, m, d) = day.ymd();
    jiff::civil::Date::new(
        i16::try_from(y).unwrap_or(1970),
        i8::try_from(m).unwrap_or(1),
        i8::try_from(d).unwrap_or(1),
    )
    .unwrap_or_default()
}

fn sql_date(day: Day) -> jiff_sqlx::Date {
    jiff_sqlx::Date::from(date_of(day))
}

/// The owner's time zone, or New York.
pub fn zone(name: Option<&str>) -> jiff::tz::TimeZone {
    name.and_then(|z| jiff::tz::TimeZone::get(z).ok())
        .or_else(|| jiff::tz::TimeZone::get("America/New_York").ok())
        .unwrap_or(jiff::tz::TimeZone::UTC)
}

/// Today in the household owner's time zone.
///
/// # Errors
///
/// A database error.
pub async fn today(conn: &mut sqlx::PgConnection) -> Result<Day, sqlx::Error> {
    let name: Option<String> = sqlx::query_scalar(
        "SELECT u.time_zone FROM app.users u \
         JOIN app.household_members m ON m.user_id = u.id \
         WHERE m.household_id = app.current_household_id() \
         ORDER BY m.created_at LIMIT 1",
    )
    .fetch_optional(&mut *conn)
    .await?;
    let tz = zone(name.as_deref());
    Ok(day_of(jiff::Timestamp::now().to_zoned_datetime(tz).date()))
}

/// The Monday of `day`'s week.
pub fn week_start(day: Day) -> Day {
    // 1970-01-01 was a Thursday: day 0 is weekday 3 (Monday = 0).
    let weekday = (day.0 + 3).rem_euclid(7);
    day.plus_days(-weekday)
}

/// The household currency.
///
/// # Errors
///
/// A database error.
pub async fn currency(conn: &mut sqlx::PgConnection) -> Result<Currency, sqlx::Error> {
    let code: String = sqlx::query_scalar(
        "SELECT currency FROM app.households WHERE id = app.current_household_id()",
    )
    .fetch_one(conn)
    .await?;
    Ok(code.parse().unwrap_or(Currency::Usd))
}

/// Flexible spending per day, from `from` to `to` (both included).
///
/// # Errors
///
/// A database error.
pub async fn flex_by_day(
    conn: &mut sqlx::PgConnection,
    currency: Currency,
    from: Day,
    to: Day,
) -> Result<HashMap<Day, i64>, sqlx::Error> {
    let rows: Vec<(jiff_sqlx::Date, i64)> = sqlx::query_as(
        "WITH lines AS ( \
           SELECT t.date, s.category_id, s.amount_minor FROM app.transactions t \
           JOIN app.transaction_splits s ON s.transaction_id = t.id \
           LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
           WHERE NOT coalesce(o.hidden, false) AND t.currency = $3 \
             AND t.date BETWEEN $1 AND $2 \
           UNION ALL \
           SELECT t.date, coalesce(o.category_id, t.category_id), t.amount_minor \
           FROM app.transactions t LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
           WHERE NOT coalesce(o.hidden, false) AND t.currency = $3 \
             AND t.date BETWEEN $1 AND $2 \
             AND NOT EXISTS (SELECT 1 FROM app.transaction_splits s \
                             WHERE s.transaction_id = t.id)) \
         SELECT l.date, (-sum(l.amount_minor))::bigint FROM lines l \
         JOIN app.categories c ON c.id = l.category_id \
         WHERE c.kind = 'flex' GROUP BY l.date",
    )
    .bind(sql_date(from))
    .bind(sql_date(to))
    .bind(currency.code())
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(d, sum)| (day_of(d.to_jiff()), sum))
        .collect())
}

fn sum_days(by_day: &HashMap<Day, i64>, from: Day, to: Day) -> i64 {
    by_day
        .iter()
        .filter(|(d, _)| **d >= from && **d <= to)
        .map(|(_, v)| *v)
        .sum()
}

type SeriesRow = (Uuid, String, String, i64, jiff_sqlx::Date);
type ChargeRow = (
    Uuid,
    String,
    i64,
    jiff_sqlx::Date,
    Option<jiff_sqlx::Date>,
    i64,
    i64,
    String,
    i32,
);

/// Expected charges: the next 7 days, and today to the end of the month.
async fn expected_bills(
    conn: &mut sqlx::PgConnection,
    currency: Currency,
    today: Day,
    month_end: Day,
) -> Result<(Vec<Upcoming>, Vec<Upcoming>), sqlx::Error> {
    // Expected charges: active, confident, money out.
    let series: Vec<SeriesRow> = sqlx::query_as(
        "SELECT s.id, m.name, s.cadence, s.amount_minor, s.next_date \
         FROM app.recurring_series s JOIN app.merchants m ON m.id = s.merchant_id \
         WHERE s.direction = 'out' AND s.active AND s.confidence_bp >= $1 AND s.currency = $2",
    )
    .bind(EXPECTED_CONFIDENCE.0)
    .bind(currency.code())
    .fetch_all(conn)
    .await?;
    let mut upcoming = Vec::new();
    let mut bills = Vec::new();
    for (id, merchant, cadence, amount, next) in series {
        let Some(cadence) = Cadence::parse(&cadence) else {
            continue;
        };
        let mut date = day_of(next.to_jiff());
        let mut guard = 0;
        while date <= month_end.max(today.plus_days(7)) && guard < 64 {
            let bill = Upcoming {
                series_id: id,
                merchant: merchant.clone(),
                amount: amount.saturating_abs(),
                date,
            };
            if date >= today && date <= today.plus_days(7) {
                upcoming.push(bill.clone());
            }
            if date >= today && date <= month_end {
                bills.push(bill);
            }
            date = cadence.next_after(date);
            guard += 1;
        }
    }
    upcoming.sort_by_key(|u| u.date);
    bills.sort_by_key(|u| u.date);

    Ok((upcoming, bills))
}

/// Recurring charges posted in the last 7 days, with the series' previous date.
async fn recent_charges(
    conn: &mut sqlx::PgConnection,
    today: Day,
) -> Result<Vec<SeriesCharge>, sqlx::Error> {
    let rows: Vec<ChargeRow> = sqlx::query_as(
        "SELECT t.id, m.name, t.amount_minor, t.date, \
           (SELECT max(p.date) FROM app.transactions p \
            WHERE p.recurring_series_id = s.id AND p.date < t.date), \
           s.amount_minor, s.spread_minor, s.cadence, s.confidence_bp \
         FROM app.transactions t \
         JOIN app.recurring_series s ON s.id = t.recurring_series_id \
         JOIN app.merchants m ON m.id = s.merchant_id \
         LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
         WHERE NOT t.pending AND NOT coalesce(o.hidden, false) AND t.date >= $1",
    )
    .bind(sql_date(today.plus_days(-7)))
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(SeriesCharge {
                transaction_id: r.0,
                merchant: r.1,
                amount: r.2,
                date: day_of(r.3.to_jiff()),
                previous: r.4.map(|p| day_of(p.to_jiff())),
                typical: r.5,
                spread: u64::try_from(r.6).unwrap_or(0),
                cadence: Cadence::parse(&r.7)?,
                confidence_bp: r.8,
            })
        })
        .collect())
}

/// Builds the fact set for the household of the open tenant transaction.
///
/// # Errors
///
/// A database error.
pub async fn build(
    conn: &mut sqlx::PgConnection,
    month: Option<MonthInput>,
) -> Result<FactSet, sqlx::Error> {
    let today = today(conn).await?;
    let currency = currency(conn).await?;
    let monday = week_start(today);
    let elapsed = monday.days_until(today);

    // Weekly: this week to today, and the same span in earlier weeks that
    // start on or after the first transaction.
    let first: Option<jiff_sqlx::Date> =
        sqlx::query_scalar("SELECT min(date) FROM app.transactions")
            .fetch_one(&mut *conn)
            .await?;
    let week = match first.map(|d| day_of(d.to_jiff())) {
        Some(first) => {
            let from = monday.plus_days(-7 * MAX_WEEKS);
            let by_day = flex_by_day(conn, currency, from, today).await?;
            let history = (1..=MAX_WEEKS)
                .map(|k| monday.plus_days(-7 * k))
                .take_while(|start| *start >= first)
                .map(|start| sum_days(&by_day, start, start.plus_days(elapsed)))
                .collect();
            Some(WeekFacts {
                week_start: monday,
                spent: sum_days(&by_day, monday, today),
                history,
            })
        }
        None => None,
    };

    let (year, m, d) = today.ymd();
    let month_start = Day::from_ymd(year, m, 1).unwrap_or(today);
    let days = savespace_core::date::days_in_month(year, m);
    let month = month.filter(|x| x.plan > 0).map(|x| MonthFacts {
        month_start,
        day: d,
        days,
        plan: x.plan,
        spent: x.plan - x.left,
        left: x.left,
    });

    let month_end = month_start.plus_months(1).plus_days(-1);
    let (upcoming, bills) = expected_bills(conn, currency, today, month_end).await?;

    // Cash: the lower of available and current for each account (least()
    // skips NULL). Many banks put overdraft protection into "available", and
    // that is not the person's money; pending payments lower "available".
    // Same rule as domain-ledger::budget.
    let cash: Option<i64> = sqlx::query_scalar(
        "SELECT sum(least(available_minor, current_minor))::bigint FROM app.accounts \
         WHERE type = 'depository' AND currency = $1",
    )
    .bind(currency.code())
    .fetch_one(&mut *conn)
    .await?;
    let balance = cash.map(|cash| BalanceFacts { cash, bills });

    let recent_charges = recent_charges(conn, today).await?;

    Ok(FactSet {
        today,
        currency,
        week,
        month,
        upcoming,
        recent_charges,
        balance,
    })
}

/// The facts for the weekly reflection of the week that starts on `monday`.
///
/// # Errors
///
/// A database error.
pub async fn week_summary(
    conn: &mut sqlx::PgConnection,
    monday: Day,
) -> Result<crate::reflection::WeekSummary, sqlx::Error> {
    let today = today(conn).await?;
    let currency = currency(conn).await?;
    let sunday = monday.plus_days(6);
    let last = sunday.min(today);
    let partial = today <= sunday;

    let first: Option<jiff_sqlx::Date> =
        sqlx::query_scalar("SELECT min(date) FROM app.transactions")
            .fetch_one(&mut *conn)
            .await?;
    let first = first.map(|d| day_of(d.to_jiff()));
    let by_day = flex_by_day(conn, currency, monday.plus_days(-7 * MAX_WEEKS), last).await?;
    let history = (1..=MAX_WEEKS)
        .map(|k| monday.plus_days(-7 * k))
        .take_while(|start| first.is_some_and(|f| *start >= f))
        .map(|start| sum_days(&by_day, start, start.plus_days(6)))
        .collect();
    let start = first.map_or(monday, |f| f.max(monday));
    let mut no_spend_days = 0;
    let mut d = start;
    while d <= last {
        if by_day.get(&d).copied().unwrap_or(0) <= 0 {
            no_spend_days += 1;
        }
        d = d.plus_days(1);
    }

    let (from, to) = (sql_date(monday), sql_date(last));
    let top: Option<(String, i64)> = sqlx::query_as(
        "WITH lines AS ( \
           SELECT s.category_id, s.amount_minor FROM app.transactions t \
           JOIN app.transaction_splits s ON s.transaction_id = t.id \
           LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
           WHERE NOT coalesce(o.hidden, false) AND t.currency = $3 AND t.date BETWEEN $1 AND $2 \
           UNION ALL \
           SELECT coalesce(o.category_id, t.category_id), t.amount_minor FROM app.transactions t \
           LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
           WHERE NOT coalesce(o.hidden, false) AND t.currency = $3 AND t.date BETWEEN $1 AND $2 \
             AND NOT EXISTS (SELECT 1 FROM app.transaction_splits s \
                             WHERE s.transaction_id = t.id)) \
         SELECT c.name, (-sum(l.amount_minor))::bigint AS spent FROM lines l \
         JOIN app.categories c ON c.id = l.category_id WHERE c.kind = 'flex' \
         GROUP BY c.name HAVING -sum(l.amount_minor) > 0 ORDER BY spent DESC, c.name LIMIT 1",
    )
    .bind(from)
    .bind(to)
    .bind(currency.code())
    .fetch_optional(&mut *conn)
    .await?;
    let (recurring_count, recurring_total, income, transactions): (i64, i64, i64, i64) =
        sqlx::query_as(
            "SELECT \
               count(*) FILTER (WHERE s.id IS NOT NULL AND t.amount_minor < 0), \
               coalesce(-sum(t.amount_minor) FILTER (WHERE s.id IS NOT NULL \
                 AND t.amount_minor < 0), 0)::bigint, \
               coalesce(sum(t.amount_minor) FILTER (WHERE c.kind = 'income' \
                 AND t.amount_minor > 0), 0)::bigint, \
               count(*) \
             FROM app.transactions t \
             LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
             LEFT JOIN app.categories c ON c.id = coalesce(o.category_id, t.category_id) \
             LEFT JOIN app.recurring_series s ON s.id = t.recurring_series_id \
               AND s.confidence_bp >= $4 \
             WHERE NOT coalesce(o.hidden, false) AND t.currency = $3 \
               AND t.date BETWEEN $1 AND $2",
        )
        .bind(from)
        .bind(to)
        .bind(currency.code())
        .bind(EXPECTED_CONFIDENCE.0)
        .fetch_one(&mut *conn)
        .await?;
    Ok(crate::reflection::WeekSummary {
        week_start: monday,
        partial,
        currency,
        flex_spent: sum_days(&by_day, monday, last),
        history,
        top_category: top,
        recurring: (u32::try_from(recurring_count).unwrap_or(0), recurring_total),
        income,
        no_spend_days,
        transactions: u32::try_from(transactions).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weeks_start_on_monday() {
        let monday = Day::from_ymd(2026, 11, 16).unwrap();
        for offset in 0..7 {
            assert_eq!(week_start(monday.plus_days(offset)), monday);
        }
        assert_eq!(week_start(monday.plus_days(7)), monday.plus_days(7));
        assert_eq!(week_start(Day(0)).ymd(), (1969, 12, 29));
    }
}
