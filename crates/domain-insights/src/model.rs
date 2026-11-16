//! Insight types (design doc §7.1, §7.4.4).

use savespace_core::{Currency, Day, Locale, Money, format_money};
use serde::Serialize;
use uuid::Uuid;

/// The insight kinds (§7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Reports a condition that needs action. Always shown.
    Concern,
    /// Confirms that a thing is normal or on plan.
    Reassurance,
    /// Reports a change from the personal baseline.
    Awareness,
    /// Reports a known future event.
    Anticipation,
    /// Offers an optional action.
    Suggestion,
}

impl Kind {
    /// The stable name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Concern => "concern",
            Self::Reassurance => "reassurance",
            Self::Awareness => "awareness",
            Self::Anticipation => "anticipation",
            Self::Suggestion => "suggestion",
        }
    }

    /// Parses [`Kind::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Concern,
            Self::Reassurance,
            Self::Awareness,
            Self::Anticipation,
            Self::Suggestion,
        ]
        .into_iter()
        .find(|k| k.as_str() == value)
    }

    /// The most of this kind in one view (§7.1). `None`: no limit.
    pub const fn max_per_view(self) -> Option<usize> {
        match self {
            Self::Concern => None,
            Self::Reassurance | Self::Anticipation => Some(2),
            Self::Awareness | Self::Suggestion => Some(1),
        }
    }
}

/// One fact that an insight text uses. Shown under "Why am I seeing this?".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fact {
    /// For example `wtd_spend_minor`.
    pub key: &'static str,
    /// The value as the text shows it, for example `$212`.
    pub display: String,
    /// A plain label for the person, for example "Spent this week so far".
    pub label: &'static str,
}

/// Formats money for insight text (Appendix B.3).
pub fn money(minor: i64, currency: Currency) -> String {
    format_money(Money::from_minor(minor, currency), Locale::EnUs)
}

/// Formats a date as `Dec 1` (Appendix B.3).
pub fn short_date(day: Day) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (_, m, d) = day.ymd();
    let name = MONTHS
        .get(usize::from(m).saturating_sub(1))
        .copied()
        .unwrap_or("");
    format!("{name} {d}")
}

/// A candidate insight from one rule for one scope (§7.4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// For example `spend.week_below_usual`.
    pub rule_id: &'static str,
    /// Increases on any logic change.
    pub rule_version: u16,
    /// The kind.
    pub kind: Kind,
    /// What it is about, for example `week:2026-11-16` or `txn:<id>`.
    pub scope: String,
    /// The template text.
    pub text: String,
    /// The facts the text uses.
    pub facts: Vec<Fact>,
    /// 0 to 10,000.
    pub confidence_bp: i32,
    /// 0 to 10,000. Higher shows first within a kind.
    pub relevance_bp: i32,
    /// For insights about one transaction.
    pub transaction_id: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_dates_and_money() {
        assert_eq!(short_date(Day::from_ymd(2026, 12, 1).unwrap()), "Dec 1");
        assert_eq!(money(180_000, Currency::Usd), "$1,800");
        assert_eq!(money(1549, Currency::Usd), "$15.49");
    }

    #[test]
    fn kinds_round_trip() {
        for k in [
            Kind::Concern,
            Kind::Reassurance,
            Kind::Awareness,
            Kind::Anticipation,
            Kind::Suggestion,
        ] {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
    }
}
