//! Insight rules (design doc §7.4). Rules are Rust code: each one reads the
//! fact set and returns zero or more candidates. Templates only (D10).

use savespace_core::recurring::{Cadence, EXPECTED_CONFIDENCE, amount_fits};
use savespace_core::stats::{mad, median};
use savespace_core::{Currency, Day};
use uuid::Uuid;

use crate::model::{Candidate, Fact, Kind, money, short_date};

/// Weekly comparisons need this many complete weeks (§7.3).
pub const MIN_WEEKS: usize = 6;

/// Flexible spending this week and in earlier weeks, like with like: each
/// history value is spending from Monday to the same weekday as today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeekFacts {
    /// The Monday of this week.
    pub week_start: Day,
    /// Spent this week so far (positive).
    pub spent: i64,
    /// The same span in earlier complete weeks, newest first.
    pub history: Vec<i64>,
}

/// The flexible budget this month.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonthFacts {
    /// The first day of the month.
    pub month_start: Day,
    /// Today's day of the month, from 1.
    pub day: u8,
    /// Days in the month.
    pub days: u8,
    /// The flexible plan (sum over categories with a plan).
    pub plan: i64,
    /// Spent this month in planned flexible categories.
    pub spent: i64,
    /// Left of the flexible plan.
    pub left: i64,
}

/// A recurring charge expected soon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upcoming {
    /// The series.
    pub series_id: Uuid,
    /// The merchant name.
    pub merchant: String,
    /// The typical amount (positive).
    pub amount: i64,
    /// The expected date.
    pub date: Day,
}

/// A posted transaction that belongs to a recurring series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesCharge {
    /// The transaction.
    pub transaction_id: Uuid,
    /// The merchant name.
    pub merchant: String,
    /// The amount, signed. Negative is money out.
    pub amount: i64,
    /// The bank date.
    pub date: Day,
    /// The date of the series' previous transaction.
    pub previous: Option<Day>,
    /// The series' typical amount, signed.
    pub typical: i64,
    /// The series' spread.
    pub spread: u64,
    /// The series' cadence.
    pub cadence: Cadence,
    /// The series' confidence.
    pub confidence_bp: i32,
}

/// Cash and the bills expected before the month ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceFacts {
    /// Cash in checking and savings.
    pub cash: i64,
    /// Expected bills from today to the end of the month, by date.
    pub bills: Vec<Upcoming>,
}

/// Everything the rules read. Built by `facts::build` from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactSet {
    /// Today in the owner's time zone.
    pub today: Day,
    /// The household currency.
    pub currency: Currency,
    /// Weekly flexible spending, if there is any history.
    pub week: Option<WeekFacts>,
    /// The flexible budget, if there is a plan.
    pub month: Option<MonthFacts>,
    /// Expected recurring charges in the next 7 days.
    pub upcoming: Vec<Upcoming>,
    /// Recurring charges posted in the last 7 days.
    pub recent_charges: Vec<SeriesCharge>,
    /// Cash and bills, if there is a checking or savings account.
    pub balance: Option<BalanceFacts>,
}

/// A rule (§7.4.4).
pub trait InsightRule: Send + Sync {
    /// For example `spend.week_below_usual`.
    fn id(&self) -> &'static str;
    /// Increases on any logic change.
    fn version(&self) -> u16;
    /// The kind of its candidates.
    fn kind(&self) -> Kind;
    /// The candidates for this fact set.
    fn evaluate(&self, facts: &FactSet) -> Vec<Candidate>;
}

/// All rules in this release.
pub fn registry() -> Vec<Box<dyn InsightRule>> {
    vec![
        Box::new(WeekBelowUsual),
        Box::new(WeekAboveUsual),
        Box::new(MonthOnTrack),
        Box::new(RecurringExpected),
        Box::new(RecurringHigher),
        Box::new(RecurringUpcoming),
        Box::new(MayNotCover),
    ]
}

/// A merchant name for insight text: at most 40 characters, so every
/// template stays within the 140-character feed limit.
fn name(merchant: &str) -> String {
    let trimmed: String = merchant.trim().chars().take(40).collect();
    trimmed.trim_end().to_owned()
}

fn fact(key: &'static str, label: &'static str, display: String) -> Fact {
    Fact {
        key,
        display,
        label,
    }
}

fn clamp_bp(value: i64) -> i32 {
    i32::try_from(value.clamp(0, 10_000)).unwrap_or(0)
}

/// The weekly baseline: (median, MAD, threshold) or `None` without enough
/// history. Threshold = max(1 × MAD, 5% of median, $10) (§7.4.1).
fn baseline(week: &WeekFacts) -> Option<(i64, i64, i64)> {
    if week.history.len() < MIN_WEEKS {
        return None;
    }
    let m = median(&week.history)?;
    let d = i64::try_from(mad(&week.history)?).unwrap_or(i64::MAX);
    if m <= 0 {
        return None;
    }
    Some((m, d, d.max(m / 20).max(1000)))
}

fn week_confidence(week: &WeekFacts, distance: i64, threshold: i64) -> i32 {
    let history = i64::try_from(week.history.len().saturating_sub(MIN_WEEKS).min(6)).unwrap_or(0);
    let far = (distance.saturating_mul(1000) / threshold.max(1)).min(2000);
    clamp_bp(7000 + history * 150 + far / 2)
}

/// `spend.week_below_usual` (reassurance, §7.4.1).
struct WeekBelowUsual;

impl InsightRule for WeekBelowUsual {
    fn id(&self) -> &'static str {
        "spend.week_below_usual"
    }
    fn version(&self) -> u16 {
        1
    }
    fn kind(&self) -> Kind {
        Kind::Reassurance
    }
    fn evaluate(&self, f: &FactSet) -> Vec<Candidate> {
        let Some(week) = &f.week else {
            return Vec::new();
        };
        let Some((usual, _, threshold)) = baseline(week) else {
            return Vec::new();
        };
        if week.spent > usual - threshold {
            return Vec::new();
        }
        let (spent, usual_text) = (money(week.spent, f.currency), money(usual, f.currency));
        vec![Candidate {
            rule_id: self.id(),
            rule_version: self.version(),
            kind: self.kind(),
            scope: format!("week:{}", short_iso(week.week_start)),
            text: format!(
                "You spent less than usual this week: {spent} so far, compared to a usual {usual_text}."
            ),
            facts: vec![
                fact(
                    "wtd_spend_minor",
                    "Flexible spending this week so far",
                    spent,
                ),
                fact(
                    "baseline_median_minor",
                    "Usual for this point in the week",
                    usual_text,
                ),
                fact(
                    "weeks_of_history",
                    "Weeks compared",
                    week.history.len().to_string(),
                ),
            ],
            confidence_bp: week_confidence(week, usual - week.spent, threshold),
            relevance_bp: 6000,
            transaction_id: None,
        }]
    }
}

/// `spend.week_above_usual` (awareness).
struct WeekAboveUsual;

impl InsightRule for WeekAboveUsual {
    fn id(&self) -> &'static str {
        "spend.week_above_usual"
    }
    fn version(&self) -> u16 {
        1
    }
    fn kind(&self) -> Kind {
        Kind::Awareness
    }
    fn evaluate(&self, f: &FactSet) -> Vec<Candidate> {
        let Some(week) = &f.week else {
            return Vec::new();
        };
        let Some((usual, _, threshold)) = baseline(week) else {
            return Vec::new();
        };
        if week.spent < usual + threshold {
            return Vec::new();
        }
        let (spent, usual_text) = (money(week.spent, f.currency), money(usual, f.currency));
        vec![Candidate {
            rule_id: self.id(),
            rule_version: self.version(),
            kind: self.kind(),
            scope: format!("week:{}", short_iso(week.week_start)),
            text: format!(
                "Flexible spending is higher than usual this week: {spent} so far, compared to a usual {usual_text}."
            ),
            facts: vec![
                fact(
                    "wtd_spend_minor",
                    "Flexible spending this week so far",
                    spent,
                ),
                fact(
                    "baseline_median_minor",
                    "Usual for this point in the week",
                    usual_text,
                ),
                fact(
                    "weeks_of_history",
                    "Weeks compared",
                    week.history.len().to_string(),
                ),
            ],
            confidence_bp: week_confidence(week, week.spent - usual, threshold),
            relevance_bp: 7000,
            transaction_id: None,
        }]
    }
}

/// `budget.month_on_track` (reassurance, §7.4.2). Expected spend uses a
/// linear pace (the fallback in §7.4.2).
struct MonthOnTrack;

impl InsightRule for MonthOnTrack {
    fn id(&self) -> &'static str {
        "budget.month_on_track"
    }
    fn version(&self) -> u16 {
        1
    }
    fn kind(&self) -> Kind {
        Kind::Reassurance
    }
    fn evaluate(&self, f: &FactSet) -> Vec<Candidate> {
        let Some(m) = f.month else { return Vec::new() };
        if m.plan <= 0 || m.left < 0 || m.day < 3 {
            return Vec::new();
        }
        let expected = i128::from(m.plan) * i128::from(m.day) / i128::from(m.days.max(1));
        // spent ≤ expected × 1.05
        if i128::from(m.spent) * 100 > expected * 105 {
            return Vec::new();
        }
        let days_left = m.days.saturating_sub(m.day).saturating_add(1);
        let left = money(m.left, f.currency);
        let days = if days_left == 1 {
            "1 day".to_owned()
        } else {
            format!("{days_left} days")
        };
        let (y, mo, _) = m.month_start.ymd();
        vec![Candidate {
            rule_id: self.id(),
            rule_version: self.version(),
            kind: self.kind(),
            scope: format!("month:{y:04}-{mo:02}"),
            text: format!("You are on track for your monthly budget. {left} left for {days}."),
            facts: vec![
                fact(
                    "mtd_flex_spend_minor",
                    "Flexible spending this month",
                    money(m.spent, f.currency),
                ),
                fact(
                    "flex_plan_minor",
                    "Flexible plan this month",
                    money(m.plan, f.currency),
                ),
                fact("flex_left_minor", "Left of the flexible plan", left),
                fact("days_left", "Days left in the month", days_left.to_string()),
            ],
            confidence_bp: 9000,
            relevance_bp: 5000,
            transaction_id: None,
        }]
    }
}

/// The days a charge can be early or late and still be "expected".
const fn date_tolerance(cadence: Cadence) -> i32 {
    match cadence {
        Cadence::Weekly => 1,
        Cadence::Biweekly => 2,
        Cadence::Monthly => 3,
        Cadence::Annual => 7,
    }
}

const fn cadence_word(cadence: Cadence) -> &'static str {
    match cadence {
        Cadence::Weekly => "week",
        Cadence::Biweekly => "2 weeks",
        Cadence::Monthly => "month",
        Cadence::Annual => "year",
    }
}

fn on_time(c: &SeriesCharge) -> bool {
    c.previous.is_some_and(|p| {
        let expected = c.cadence.next_after(p);
        expected.days_until(c.date).abs() <= date_tolerance(c.cadence)
    })
}

/// `txn.recurring_expected` (reassurance, §7.4.3).
struct RecurringExpected;

impl InsightRule for RecurringExpected {
    fn id(&self) -> &'static str {
        "txn.recurring_expected"
    }
    fn version(&self) -> u16 {
        1
    }
    fn kind(&self) -> Kind {
        Kind::Reassurance
    }
    fn evaluate(&self, f: &FactSet) -> Vec<Candidate> {
        f.recent_charges
            .iter()
            .filter(|c| c.confidence_bp >= EXPECTED_CONFIDENCE.0 && on_time(c))
            .filter(|c| amount_fits(c.typical, c.spread, c.amount))
            .map(|c| {
                let about = money(c.typical.saturating_abs(), f.currency);
                let each = cadence_word(c.cadence);
                let text = if c.amount < 0 {
                    format!(
                        "This charge is recurring and expected. {} charges about {about} each {each}.",
                        name(&c.merchant)
                    )
                } else {
                    format!(
                        "This deposit is recurring and expected. {} pays about {about} each {each}.",
                        name(&c.merchant)
                    )
                };
                Candidate {
                    rule_id: self.id(),
                    rule_version: self.version(),
                    kind: self.kind(),
                    scope: format!("txn:{}", c.transaction_id.simple()),
                    text,
                    facts: vec![
                        fact("expected_amount_minor", "Usual amount", about),
                        fact("actual_amount_minor", "This amount", money(c.amount.saturating_abs(), f.currency)),
                        fact("actual_date", "Date", short_date(c.date)),
                        fact("cadence", "Repeats every", each.to_owned()),
                    ],
                    confidence_bp: c.confidence_bp,
                    relevance_bp: 4000,
                    transaction_id: Some(c.transaction_id),
                }
            })
            .collect()
    }
}

/// `txn.recurring_higher` (awareness): the §7.4.3 variant for a charge above
/// its band.
struct RecurringHigher;

impl InsightRule for RecurringHigher {
    fn id(&self) -> &'static str {
        "txn.recurring_higher"
    }
    fn version(&self) -> u16 {
        1
    }
    fn kind(&self) -> Kind {
        Kind::Awareness
    }
    fn evaluate(&self, f: &FactSet) -> Vec<Candidate> {
        f.recent_charges
            .iter()
            .filter(|c| c.confidence_bp >= EXPECTED_CONFIDENCE.0 && c.amount < 0)
            .filter(|c| c.amount < c.typical && !amount_fits(c.typical, c.spread, c.amount))
            .map(|c| {
                let delta = money(c.typical.saturating_sub(c.amount), f.currency);
                Candidate {
                    rule_id: self.id(),
                    rule_version: self.version(),
                    kind: self.kind(),
                    scope: format!("txn:{}", c.transaction_id.simple()),
                    text: format!(
                        "This {} charge is {delta} higher than usual.",
                        name(&c.merchant)
                    ),
                    facts: vec![
                        fact(
                            "actual_amount_minor",
                            "This amount",
                            money(-c.amount, f.currency),
                        ),
                        fact(
                            "expected_amount_minor",
                            "Usual amount",
                            money(-c.typical, f.currency),
                        ),
                        fact("delta_minor", "Difference", delta),
                    ],
                    confidence_bp: c.confidence_bp,
                    relevance_bp: 6500,
                    transaction_id: Some(c.transaction_id),
                }
            })
            .collect()
    }
}

/// `recurring.upcoming` (anticipation): an expected charge in the next 3 days.
struct RecurringUpcoming;

impl InsightRule for RecurringUpcoming {
    fn id(&self) -> &'static str {
        "recurring.upcoming"
    }
    fn version(&self) -> u16 {
        1
    }
    fn kind(&self) -> Kind {
        Kind::Anticipation
    }
    fn evaluate(&self, f: &FactSet) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = f
            .upcoming
            .iter()
            .filter(|u| f.today.days_until(u.date) <= 3 && u.date >= f.today)
            .map(|u| {
                let amount = money(u.amount, f.currency);
                let date = short_date(u.date);
                // "Covers it": cash covers this bill and every bill before it.
                let covered = f.balance.as_ref().is_some_and(|b| {
                    let before: i64 = b
                        .bills
                        .iter()
                        .filter(|x| x.date <= u.date)
                        .map(|x| x.amount)
                        .sum();
                    b.cash >= before.max(u.amount)
                });
                let text = if covered {
                    format!(
                        "{} ({amount}) is expected on {date}. Your cash covers it.",
                        name(&u.merchant)
                    )
                } else {
                    format!("{} ({amount}) is expected on {date}.", name(&u.merchant))
                };
                Candidate {
                    rule_id: self.id(),
                    rule_version: self.version(),
                    kind: self.kind(),
                    scope: format!("upcoming:{}:{}", u.series_id.simple(), short_iso(u.date)),
                    text,
                    facts: vec![
                        fact("expected_amount_minor", "Usual amount", amount),
                        fact("expected_date", "Expected on", date),
                    ],
                    confidence_bp: 8500,
                    relevance_bp: clamp_bp(4000 + u.amount / 1000),
                    transaction_id: None,
                }
            })
            .collect();
        out.sort_by_key(|c| -c.relevance_bp);
        out
    }
}

/// `balance.may_not_cover` (concern): cash may not cover a bill before the
/// month ends. States the fact and the date. No alarm words (§7.6).
struct MayNotCover;

impl InsightRule for MayNotCover {
    fn id(&self) -> &'static str {
        "balance.may_not_cover"
    }
    fn version(&self) -> u16 {
        1
    }
    fn kind(&self) -> Kind {
        Kind::Concern
    }
    fn evaluate(&self, f: &FactSet) -> Vec<Candidate> {
        let Some(b) = &f.balance else {
            return Vec::new();
        };
        let mut bills = b.bills.clone();
        bills.sort_by_key(|x| x.date);
        let mut total: i64 = 0;
        for bill in &bills {
            total = total.saturating_add(bill.amount);
            if total > b.cash {
                let amount = money(bill.amount, f.currency);
                let date = short_date(bill.date);
                let (y, m, _) = f.today.ymd();
                return vec![Candidate {
                    rule_id: self.id(),
                    rule_version: self.version(),
                    kind: self.kind(),
                    scope: format!("month:{y:04}-{m:02}"),
                    text: format!(
                        "Your cash may not cover {} ({amount}) on {date}. You can review your upcoming bills.",
                        name(&bill.merchant)
                    ),
                    facts: vec![
                        fact(
                            "cash_minor",
                            "Cash in checking and savings",
                            money(b.cash, f.currency),
                        ),
                        fact(
                            "bills_minor",
                            "Bills expected through this one",
                            money(total, f.currency),
                        ),
                        fact("expected_amount_minor", "This bill", amount),
                        fact("expected_date", "Expected on", date),
                    ],
                    confidence_bp: 8000,
                    relevance_bp: 10_000,
                    transaction_id: None,
                }];
            }
        }
        Vec::new()
    }
}

/// `2026-11-16`.
pub fn short_iso(day: Day) -> String {
    let (y, m, d) = day.ymd();
    format!("{y:04}-{m:02}-{d:02}")
}
