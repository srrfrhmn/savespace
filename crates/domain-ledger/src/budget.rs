//! The flex budget for a month and the calm state (design doc §6.1).
//!
//! - A plan applies from its month until a later plan changes it.
//! - Fixed and flexible plans are monthly. A non-monthly plan is yearly: 1/12
//!   is set aside each month, and spending is compared with what was set aside
//!   so far this year, so a yearly cost is not "over budget" in its month.
//! - Rollover carries what is left (or overspent) into the next month.
//! - Hidden transactions do not count. A split counts in each part's category.
//! - The calm state uses cash in checking and savings, the recurring bills
//!   expected before the month ends, and what is left of the flexible plan.
//!   The math is in `savespace_core::budget`.

use std::collections::HashMap;

use savespace_core::budget;
use savespace_core::recurring::{Cadence, EXPECTED_CONFIDENCE};
use savespace_core::{Currency, Day, Money};
use serde::Serialize;
use uuid::Uuid;

use crate::{LedgerError, LedgerService, Result, invalid};

/// Rollover looks back at most this many months.
const MAX_ROLLOVER_MONTHS: i32 = 24;

/// Totals for one bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    /// The sum of the plans (for non-monthly: set aside so far this year).
    pub planned: i64,
    /// Spent (for non-monthly: spent so far this year).
    pub spent: i64,
    /// Left of the plans: the sum over categories with a plan.
    pub left: i64,
}

/// One category in a month.
#[derive(Debug, Clone, Serialize)]
pub struct CategoryBudget {
    /// The category.
    pub category_id: Uuid,
    /// `fixed`, `flex`, or `non_monthly`.
    pub kind: String,
    /// The plan: monthly, or yearly for non-monthly. `None` if no plan.
    pub plan: Option<i64>,
    /// Carried in from earlier months (rollover). Zero if rollover is off.
    pub carried: i64,
    /// Spent this month. Refunds lower it.
    pub spent: i64,
    /// Left this month: plan + carried − spent. For non-monthly: set aside so
    /// far this year − spent so far this year. `None` if no plan.
    pub left: Option<i64>,
    /// True if rollover is on.
    pub rollover: bool,
    /// Non-monthly only: set aside by the end of this month.
    pub set_aside: Option<i64>,
    /// Non-monthly only: spent from January to the end of this month.
    pub spent_this_year: Option<i64>,
}

/// The calm state and the facts behind it. Current month only.
#[derive(Debug, Clone, Serialize)]
pub struct Calm {
    /// `covered`, `tight`, or `needs_attention`.
    pub state: &'static str,
    /// Cash in checking and savings.
    pub cash: i64,
    /// Expected recurring bills from today to the end of the month.
    pub bills: i64,
    /// Left of the flexible plan (categories that have a plan, not below zero).
    pub flex_left: i64,
    /// The last day the bills cover.
    pub until: jiff::civil::Date,
}

/// The budget for one month.
#[derive(Debug, Clone, Serialize)]
pub struct MonthBudget {
    /// The first day of the month.
    pub month: jiff::civil::Date,
    /// The household currency.
    pub currency: String,
    /// Fixed, flexible, and non-monthly categories.
    pub categories: Vec<CategoryBudget>,
    /// Fixed bucket.
    pub fixed: Totals,
    /// Flexible bucket.
    pub flex: Totals,
    /// Non-monthly bucket (year to date).
    pub non_monthly: Totals,
    /// Money in, in income categories, this month.
    pub income: i64,
    /// Spent this month without a category.
    pub uncategorized: i64,
    /// The calm state, for the current month only.
    pub calm: Option<Calm>,
}

/// A change to one category's plan.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanChange {
    /// `Some(None)` ends the plan from this month.
    pub plan: Option<Option<i64>>,
    /// Turns rollover on from this month, or off.
    pub rollover: Option<bool>,
}

fn day_of(date: jiff::civil::Date) -> Day {
    Day::from_ymd(
        i32::from(date.year()),
        u8::try_from(date.month()).unwrap_or(1),
        u8::try_from(date.day()).unwrap_or(1),
    )
    .unwrap_or(Day(0))
}

fn date_of(day: Day) -> jiff::civil::Date {
    let (y, m, d) = day.ymd();
    jiff::civil::Date::new(
        i16::try_from(y).unwrap_or(1970),
        i8::try_from(m).unwrap_or(1),
        i8::try_from(d).unwrap_or(1),
    )
    .unwrap_or_default()
}

fn money_error(e: savespace_core::MoneyError) -> LedgerError {
    tracing::warn!(error = %e, "budget math");
    invalid(
        "amount_out_of_range",
        "An amount is too large to calculate.",
        "plan_minor",
    )
}

type CategoryRow = (Uuid, String, Option<jiff_sqlx::Date>);
type SpendRow = (jiff_sqlx::Date, Option<Uuid>, i64);
type PlanRow = (Uuid, jiff_sqlx::Date, Option<i64>);
type SeriesRow = (String, i64, jiff_sqlx::Date);

/// Today in the household owner's time zone.
async fn today(conn: &mut sqlx::PgConnection) -> Result<jiff::civil::Date> {
    let zone: Option<String> = sqlx::query_scalar(
        "SELECT u.time_zone FROM app.users u \
         JOIN app.household_members m ON m.user_id = u.id \
         WHERE m.household_id = app.current_household_id() \
         ORDER BY m.created_at LIMIT 1",
    )
    .fetch_optional(conn)
    .await?;
    let tz = zone
        .and_then(|z| jiff::tz::TimeZone::get(&z).ok())
        .or_else(|| jiff::tz::TimeZone::get("America/New_York").ok())
        .unwrap_or(jiff::tz::TimeZone::UTC);
    Ok(jiff::Timestamp::now().to_zoned_datetime(tz).date())
}

/// The effective plan for `month` from rows sorted by month.
fn plan_for(rows: &[(Day, Option<i64>)], month: Day) -> Option<i64> {
    rows.iter()
        .take_while(|(m, _)| *m <= month)
        .last()
        .and_then(|(_, p)| *p)
}

impl LedgerService {
    /// The household currency.
    async fn currency(conn: &mut sqlx::PgConnection) -> Result<Currency> {
        let code: String = sqlx::query_scalar(
            "SELECT currency FROM app.households WHERE id = app.current_household_id()",
        )
        .fetch_one(conn)
        .await?;
        code.parse().map_err(|_| {
            invalid(
                "currency_unsupported",
                "The household currency is not supported.",
                "currency",
            )
        })
    }

    /// The budget for the month that starts on `month` (a first day).
    ///
    /// # Errors
    ///
    /// A database error, or an amount out of range.
    #[expect(clippy::too_many_lines, reason = "one month, step by step")]
    pub async fn budget_month(
        &self,
        household_id: Uuid,
        month: jiff::civil::Date,
    ) -> Result<MonthBudget> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let currency = Self::currency(&mut tx).await?;
        let today = day_of(today(&mut tx).await?);
        let month_day = day_of(month).month_start();
        let next_month = month_day.plus_months(1);
        let (year, _, _) = month_day.ymd();
        let year_start = Day::from_ymd(year, 1, 1).unwrap_or(month_day);
        let month_of_year = month_day.ymd().1;

        let categories: Vec<CategoryRow> = sqlx::query_as(
            "SELECT id, kind, rollover_from FROM app.categories \
             WHERE kind IN ('fixed', 'flex', 'non_monthly') ORDER BY position, name",
        )
        .fetch_all(&mut *tx)
        .await?;
        let oldest_rollover = categories
            .iter()
            .filter_map(|c| c.2.map(|d| day_of(d.to_jiff())))
            .min()
            .map_or(year_start, |d| {
                d.max(month_day.plus_months(-MAX_ROLLOVER_MONTHS))
            });
        let from = year_start.min(oldest_rollover);

        let mut plans: HashMap<Uuid, Vec<(Day, Option<i64>)>> = HashMap::new();
        let plan_rows: Vec<PlanRow> = sqlx::query_as(
            "SELECT category_id, month, plan_minor FROM app.budgets \
             WHERE month < $1 AND currency = $2 ORDER BY month",
        )
        .bind(jiff_sqlx::Date::from(date_of(next_month)))
        .bind(currency.code())
        .fetch_all(&mut *tx)
        .await?;
        for (category, m, plan) in plan_rows {
            plans
                .entry(category)
                .or_default()
                .push((day_of(m.to_jiff()), plan));
        }

        // Signed sums per month and category. A split counts in each part.
        let spend_rows: Vec<SpendRow> = sqlx::query_as(
            "WITH lines AS ( \
               SELECT t.date, s.category_id, s.amount_minor FROM app.transactions t \
               JOIN app.transaction_splits s ON s.transaction_id = t.id \
               LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
               WHERE NOT coalesce(o.hidden, false) AND t.currency = $3 \
                 AND t.date >= $1 AND t.date < $2 \
               UNION ALL \
               SELECT t.date, coalesce(o.category_id, t.category_id), t.amount_minor \
               FROM app.transactions t \
               LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
               WHERE NOT coalesce(o.hidden, false) AND t.currency = $3 \
                 AND t.date >= $1 AND t.date < $2 \
                 AND NOT EXISTS (SELECT 1 FROM app.transaction_splits s \
                                 WHERE s.transaction_id = t.id)) \
             SELECT date_trunc('month', date)::date, category_id, sum(amount_minor)::bigint \
             FROM lines GROUP BY 1, 2",
        )
        .bind(jiff_sqlx::Date::from(date_of(from)))
        .bind(jiff_sqlx::Date::from(date_of(next_month)))
        .bind(currency.code())
        .fetch_all(&mut *tx)
        .await?;
        // Spending is positive: money out of a category.
        let mut spent: HashMap<(Day, Option<Uuid>), i64> = HashMap::new();
        for (m, category, sum) in spend_rows {
            spent.insert((day_of(m.to_jiff()), category), sum.saturating_neg());
        }
        let spent_in = |m: Day, c: Option<Uuid>| spent.get(&(m, c)).copied().unwrap_or(0);

        let income_ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM app.categories WHERE kind = 'income'")
                .fetch_all(&mut *tx)
                .await?;
        let income: i64 = income_ids
            .iter()
            .map(|id| spent_in(month_day, Some(*id)).saturating_neg())
            .sum();

        let usd = |minor: i64| Money::from_minor(minor, currency);
        let mut out = Vec::with_capacity(categories.len());
        let (mut fixed, mut flex, mut non_monthly) =
            (Totals::default(), Totals::default(), Totals::default());
        for (id, kind, rollover_from) in categories {
            let rows = plans.remove(&id).unwrap_or_default();
            let plan = plan_for(&rows, month_day);
            let spent_now = spent_in(month_day, Some(id));
            let mut item = CategoryBudget {
                category_id: id,
                kind: kind.clone(),
                plan,
                carried: 0,
                spent: spent_now,
                left: None,
                rollover: rollover_from.is_some(),
                set_aside: None,
                spent_this_year: None,
            };
            if kind == "non_monthly" {
                let spent_year: i64 = (1..=month_of_year)
                    .filter_map(|m| Day::from_ymd(year, m, 1))
                    .map(|m| spent_in(m, Some(id)))
                    .sum();
                item.spent_this_year = Some(spent_year);
                if let Some(yearly) = plan {
                    let aside = budget::set_aside_by_month(usd(yearly), month_of_year)
                        .map_err(money_error)?
                        .minor();
                    item.set_aside = Some(aside);
                    item.left = Some(aside.saturating_sub(spent_year));
                    non_monthly.planned += aside;
                    non_monthly.left += aside.saturating_sub(spent_year);
                }
                non_monthly.spent += spent_year;
            } else {
                if let Some(start) = rollover_from.map(|d| day_of(d.to_jiff())) {
                    let mut history = Vec::new();
                    let mut m = start.max(month_day.plus_months(-MAX_ROLLOVER_MONTHS));
                    while m < month_day {
                        let p = plan_for(&rows, m).unwrap_or(0);
                        history.push((usd(p), usd(spent_in(m, Some(id)))));
                        m = m.plus_months(1);
                    }
                    item.carried = budget::rollover(currency, &history)
                        .map_err(money_error)?
                        .minor();
                }
                if let Some(p) = plan {
                    let left = budget::left(usd(p), usd(item.carried), usd(spent_now))
                        .map_err(money_error)?
                        .minor();
                    item.left = Some(left);
                }
                let totals = if kind == "fixed" {
                    &mut fixed
                } else {
                    &mut flex
                };
                totals.planned += plan.unwrap_or(0);
                totals.spent += spent_now;
                totals.left += item.left.unwrap_or(0);
            }
            out.push(item);
        }

        let calm = if month_day == today.month_start() {
            self::calm(&mut tx, currency, today, next_month, &out).await?
        } else {
            None
        };
        tx.commit().await?;
        Ok(MonthBudget {
            month: date_of(month_day),
            currency: currency.code().to_owned(),
            categories: out,
            fixed,
            flex,
            non_monthly,
            income,
            uncategorized: spent_in(month_day, None),
            calm,
        })
    }

    /// Sets a category's plan from `month` on, and turns rollover on or off.
    ///
    /// # Errors
    ///
    /// [`LedgerError::NotFound`] for an unknown category, [`LedgerError::Invalid`]
    /// for a category without a budget (income, transfers), or a database error.
    pub async fn set_plan(
        &self,
        household_id: Uuid,
        category_id: Uuid,
        month: jiff::civil::Date,
        change: PlanChange,
    ) -> Result<()> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let kind: Option<String> =
            sqlx::query_scalar("SELECT kind FROM app.categories WHERE id = $1 FOR UPDATE")
                .bind(category_id)
                .fetch_optional(&mut *tx)
                .await?;
        let kind = kind.ok_or(LedgerError::NotFound)?;
        if !matches!(kind.as_str(), "fixed" | "flex" | "non_monthly") {
            return Err(invalid(
                "category_not_budgeted",
                "Income and transfers do not have a plan.",
                "category_id",
            ));
        }
        let month = date_of(day_of(month).month_start());
        if let Some(plan) = change.plan {
            let currency = Self::currency(&mut tx).await?;
            sqlx::query(
                "INSERT INTO app.budgets (household_id, category_id, month, plan_minor, currency) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (category_id, month) DO UPDATE SET plan_minor = EXCLUDED.plan_minor, \
                   currency = EXCLUDED.currency, updated_at = now()",
            )
            .bind(household_id)
            .bind(category_id)
            .bind(jiff_sqlx::Date::from(month))
            .bind(plan)
            .bind(currency.code())
            .execute(&mut *tx)
            .await?;
        }
        if let Some(on) = change.rollover {
            if on && kind == "non_monthly" {
                return Err(invalid(
                    "rollover_not_available",
                    "Non-monthly categories already carry their savings through the year.",
                    "rollover",
                ));
            }
            sqlx::query(
                "UPDATE app.categories SET rollover_from = CASE WHEN $2 \
                   THEN coalesce(least(rollover_from, $3), $3) ELSE NULL END WHERE id = $1",
            )
            .bind(category_id)
            .bind(on)
            .bind(jiff_sqlx::Date::from(month))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

/// The calm state for today: cash, bills until the month ends, flex left.
async fn calm(
    conn: &mut sqlx::PgConnection,
    currency: Currency,
    today: Day,
    next_month: Day,
    categories: &[CategoryBudget],
) -> Result<Option<Calm>> {
    // Cash: the lower of available and current for each account (least()
    // skips NULL). Many banks put overdraft protection into "available", and
    // that is not the person's money; pending payments lower "available".
    // Same rule as domain-insights::facts.
    let cash: Option<i64> = sqlx::query_scalar(
        "SELECT sum(least(available_minor, current_minor))::bigint FROM app.accounts \
         WHERE type = 'depository' AND currency = $1",
    )
    .bind(currency.code())
    .fetch_one(&mut *conn)
    .await?;
    let Some(cash) = cash else {
        return Ok(None);
    };
    let until = next_month.plus_days(-1);
    let series: Vec<SeriesRow> = sqlx::query_as(
        "SELECT cadence, amount_minor, next_date FROM app.recurring_series \
         WHERE direction = 'out' AND active AND confidence_bp >= $1 AND currency = $2",
    )
    .bind(EXPECTED_CONFIDENCE.0)
    .bind(currency.code())
    .fetch_all(&mut *conn)
    .await?;
    let mut bills: i64 = 0;
    for (cadence, amount, next) in series {
        let Some(cadence) = Cadence::parse(&cadence) else {
            continue;
        };
        let mut day = day_of(next.to_jiff());
        let mut guard = 0;
        while day <= until && guard < 64 {
            if day >= today {
                bills = bills.saturating_add(amount.saturating_abs());
            }
            day = cadence.next_after(day);
            guard += 1;
        }
    }
    let flex_left: i64 = categories
        .iter()
        .filter(|c| c.kind == "flex")
        .filter_map(|c| c.left)
        .map(|l| l.max(0))
        .sum();
    let m = |minor| Money::from_minor(minor, currency);
    let state = budget::calm_state(m(cash), m(bills), m(flex_left)).map_err(money_error)?;
    Ok(Some(Calm {
        state: state.as_str(),
        cash,
        bills,
        flex_left,
        until: date_of(until),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_applies_until_changed() {
        let d = |m| Day::from_ymd(2026, m, 1).unwrap();
        let rows = [(d(3), Some(40_000)), (d(6), Some(45_000)), (d(9), None)];
        assert_eq!(plan_for(&rows, d(2)), None);
        assert_eq!(plan_for(&rows, d(3)), Some(40_000));
        assert_eq!(plan_for(&rows, d(5)), Some(40_000));
        assert_eq!(plan_for(&rows, d(7)), Some(45_000));
        assert_eq!(plan_for(&rows, d(10)), None);
    }

    #[test]
    fn converts_dates() {
        let d = jiff::civil::date(2028, 2, 29);
        assert_eq!(date_of(day_of(d)), d);
    }
}
