//! The flex budget, calm state, and recurring series (design doc §6.1, §11.6).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use domain_ledger::budget::{self, PlanChange};
use domain_recurring::RecurringError;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::error::{ApiError, ErrorBody, Json};
use crate::session::Auth;
use crate::{AppState, ids};

/// Routes that need a session.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_budget))
        .routes(routes!(set_plan))
        .routes(routes!(list_recurring))
}

impl From<RecurringError> for ApiError {
    fn from(e: RecurringError) -> Self {
        tracing::error!(error = %e, "recurring error");
        Self::internal()
    }
}

/// The largest plan: 10 billion in minor units.
const MAX_PLAN: i64 = 1_000_000_000_000;

/// `YYYY-MM` to the first day of that month.
fn month(value: &str) -> Result<jiff::civil::Date, ApiError> {
    let bad = || ApiError::invalid("month_invalid", "The month must be YYYY-MM.").param("month");
    let (y, m) = value.split_once('-').ok_or_else(bad)?;
    if y.len() != 4 || m.len() != 2 {
        return Err(bad());
    }
    let year: i16 = y.parse().map_err(|_| bad())?;
    let month: i8 = m.parse().map_err(|_| bad())?;
    if !(2000..=2100).contains(&year) {
        return Err(bad());
    }
    jiff::civil::Date::new(year, month, 1).map_err(|_| bad())
}

/// Totals for one bucket.
#[derive(Debug, Serialize, ToSchema)]
pub struct BucketTotals {
    /// The sum of the plans. Non-monthly: set aside so far this year.
    pub planned_minor: i64,
    /// Spent. Non-monthly: spent so far this year.
    pub spent_minor: i64,
    /// Left of the plans, over categories that have a plan.
    pub left_minor: i64,
}

impl From<budget::Totals> for BucketTotals {
    fn from(t: budget::Totals) -> Self {
        Self {
            planned_minor: t.planned,
            spent_minor: t.spent,
            left_minor: t.left,
        }
    }
}

/// One category in the month.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryBudget {
    /// The category ID (`cat_…`).
    pub category_id: String,
    /// `fixed`, `flex`, or `non_monthly`.
    pub kind: String,
    /// The plan: monthly, or yearly for `non_monthly`. Null if no plan.
    pub plan_minor: Option<i64>,
    /// Carried in from earlier months (rollover).
    pub carried_minor: i64,
    /// Spent this month. Refunds lower it.
    pub spent_minor: i64,
    /// Left: plan + carried − spent. Non-monthly: set aside − spent this
    /// year. Null if no plan.
    pub left_minor: Option<i64>,
    /// True if rollover is on.
    pub rollover: bool,
    /// Non-monthly: set aside by the end of this month.
    pub set_aside_minor: Option<i64>,
    /// Non-monthly: spent from January to the end of this month.
    pub spent_this_year_minor: Option<i64>,
}

/// The calm state and the facts behind it.
#[derive(Debug, Serialize, ToSchema)]
pub struct CalmState {
    /// `covered`, `tight`, or `needs_attention`.
    pub state: String,
    /// Cash in checking and savings.
    pub cash_minor: i64,
    /// Expected recurring bills from today to `until`.
    pub bills_minor: i64,
    /// Left of the flexible plan.
    pub flex_left_minor: i64,
    /// The last day the bills cover (`YYYY-MM-DD`).
    #[schema(value_type = String, format = Date)]
    pub until: jiff::civil::Date,
}

/// The budget for one month.
#[derive(Debug, Serialize, ToSchema)]
pub struct MonthBudget {
    /// The month (`YYYY-MM`).
    pub month: String,
    /// ISO 4217 code.
    pub currency: String,
    /// Fixed, flexible, and non-monthly categories, in display order.
    pub categories: Vec<CategoryBudget>,
    /// The fixed bucket.
    pub fixed: BucketTotals,
    /// The flexible bucket.
    pub flex: BucketTotals,
    /// The non-monthly bucket, year to date.
    pub non_monthly: BucketTotals,
    /// Money in, in income categories, this month.
    pub income_minor: i64,
    /// Spent this month without a category.
    pub uncategorized_minor: i64,
    /// The calm state. Only for the current month, and only with a checking
    /// or savings account.
    pub calm: Option<CalmState>,
}

/// Gets the budget for a month.
#[utoipa::path(get, path = "/budgets/{month}", tag = "budget",
    params(("month" = String, Path, description = "The month, `YYYY-MM`.")),
    responses((status = 200, body = MonthBudget), (status = "4XX", body = ErrorBody)))]
async fn get_budget(
    State(state): State<AppState>,
    auth: Auth,
    Path(value): Path<String>,
) -> Result<Json<MonthBudget>, ApiError> {
    let b = state
        .ledger
        .budget_month(auth.session.household_id, month(&value)?)
        .await?;
    Ok(Json(MonthBudget {
        month: format!("{:04}-{:02}", b.month.year(), b.month.month()),
        currency: b.currency,
        categories: b
            .categories
            .into_iter()
            .map(|c| CategoryBudget {
                category_id: ids::encode(ids::CATEGORY, c.category_id),
                kind: c.kind,
                plan_minor: c.plan,
                carried_minor: c.carried,
                spent_minor: c.spent,
                left_minor: c.left,
                rollover: c.rollover,
                set_aside_minor: c.set_aside,
                spent_this_year_minor: c.spent_this_year,
            })
            .collect(),
        fixed: b.fixed.into(),
        flex: b.flex.into(),
        non_monthly: b.non_monthly.into(),
        income_minor: b.income,
        uncategorized_minor: b.uncategorized,
        calm: b.calm.map(|c| CalmState {
            state: c.state.to_owned(),
            cash_minor: c.cash,
            bills_minor: c.bills,
            flex_left_minor: c.flex_left,
            until: c.until,
        }),
    }))
}

/// `absent` is `None`, `null` is `Some(None)`.
#[expect(clippy::option_option, reason = "PUT: absent and null differ")]
fn double<'de, D>(d: D) -> Result<Option<Option<i64>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<i64>::deserialize(d).map(Some)
}

/// A plan change. A field that is not in the body does not change.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetPlanRequest {
    /// The plan in minor units, from this month on: monthly, or yearly for a
    /// non-monthly category. `null` ends the plan from this month.
    #[serde(default, deserialize_with = "double")]
    #[schema(value_type = Option<i64>, nullable)]
    pub plan_minor: Option<Option<i64>>,
    /// Carry what is left into the next month. Fixed and flexible only.
    pub rollover: Option<bool>,
}

/// Sets a category's plan from a month on.
#[utoipa::path(put, path = "/budgets/{month}/categories/{id}", tag = "budget",
    params(("month" = String, Path, description = "The month, `YYYY-MM`."),
           ("id" = String, Path, description = "The category ID (`cat_…`).")),
    request_body = SetPlanRequest,
    responses((status = 204), (status = "4XX", body = ErrorBody)))]
async fn set_plan(
    State(state): State<AppState>,
    auth: Auth,
    Path((value, id)): Path<(String, String)>,
    Json(body): Json<SetPlanRequest>,
) -> Result<StatusCode, ApiError> {
    let month = month(&value)?;
    let category = ids::decode(ids::CATEGORY, &id).ok_or_else(ApiError::not_found)?;
    if let Some(Some(plan)) = body.plan_minor
        && !(0..=MAX_PLAN).contains(&plan)
    {
        return Err(
            ApiError::invalid("plan_invalid", "Use an amount from 0 to 10 billion.")
                .param("plan_minor"),
        );
    }
    state
        .ledger
        .set_plan(
            auth.session.household_id,
            category,
            month,
            PlanChange {
                plan: body.plan_minor,
                rollover: body.rollover,
            },
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A recurring charge or income.
#[derive(Debug, Serialize, ToSchema)]
pub struct RecurringSeries {
    /// The series ID (`rec_…`).
    pub id: String,
    /// The merchant ID (`mer_…`).
    pub merchant_id: String,
    /// The merchant name.
    pub merchant_name: String,
    /// `in` or `out`.
    pub direction: String,
    /// ISO 4217 code.
    pub currency: String,
    /// `weekly`, `biweekly`, `monthly`, or `annual`.
    pub cadence: String,
    /// The typical amount. Negative is money out.
    pub amount_minor: i64,
    /// How far amounts usually are from the typical amount.
    pub spread_minor: i64,
    /// The latest date (`YYYY-MM-DD`).
    #[schema(value_type = String, format = Date)]
    pub last_date: jiff::civil::Date,
    /// The next expected date (`YYYY-MM-DD`).
    #[schema(value_type = String, format = Date)]
    pub next_date: jiff::civil::Date,
    /// How many transactions match.
    pub occurrences: i32,
    /// 0 to 10,000 basis points.
    pub confidence_bp: i32,
    /// False if the series probably stopped.
    pub active: bool,
    /// True if SaveSpace calls it "expected": active and confident (§7.4.3).
    pub expected: bool,
    /// The most common category (`cat_…`).
    pub category_id: Option<String>,
}

/// Recurring series: active first, then by next date.
#[derive(Debug, Serialize, ToSchema)]
pub struct RecurringList {
    /// The series.
    pub data: Vec<RecurringSeries>,
}

/// Lists recurring charges and income.
#[utoipa::path(get, path = "/recurring", tag = "budget",
    responses((status = 200, body = RecurringList), (status = 401, body = ErrorBody)))]
async fn list_recurring(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<RecurringList>, ApiError> {
    let data = state
        .recurring
        .list(auth.session.household_id)
        .await?
        .into_iter()
        .map(|s| RecurringSeries {
            id: ids::encode(ids::RECURRING, s.id),
            merchant_id: ids::encode(ids::MERCHANT, s.merchant_id),
            merchant_name: s.merchant_name,
            direction: s.direction,
            currency: s.currency,
            cadence: s.cadence,
            amount_minor: s.amount_minor,
            spread_minor: s.spread_minor,
            last_date: s.last_date,
            next_date: s.next_date,
            occurrences: s.occurrences,
            confidence_bp: s.confidence_bp,
            active: s.active,
            expected: s.expected,
            category_id: s.category_id.map(|c| ids::encode(ids::CATEGORY, c)),
        })
        .collect();
    Ok(Json(RecurringList { data }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_months() {
        assert_eq!(month("2026-11").unwrap(), jiff::civil::date(2026, 11, 1));
        for bad in [
            "2026-13", "2026-1", "26-11", "2026/11", "1999-01", "2026-00", "x",
        ] {
            assert!(month(bad).is_err(), "{bad}");
        }
    }
}
