//! Recurring series detection (design doc §6.1, §10.2).
//!
//! The input is one merchant's posted transactions in one direction: dates and
//! amounts. The output is a cadence, an amount band, the next expected date,
//! and a confidence. Integer math only.

use crate::BasisPoints;
use crate::date::Day;
use crate::stats::{mad, median};

/// How often a series repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cadence {
    /// Every 7 days.
    Weekly,
    /// Every 14 days.
    Biweekly,
    /// Once a month, on about the same day.
    Monthly,
    /// Once a year.
    Annual,
}

impl Cadence {
    /// The stable name: `weekly`, `biweekly`, `monthly`, or `annual`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Weekly => "weekly",
            Self::Biweekly => "biweekly",
            Self::Monthly => "monthly",
            Self::Annual => "annual",
        }
    }

    /// Parses [`Cadence::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "weekly" => Some(Self::Weekly),
            "biweekly" => Some(Self::Biweekly),
            "monthly" => Some(Self::Monthly),
            "annual" => Some(Self::Annual),
            _ => None,
        }
    }

    /// The gaps in days that fit this cadence.
    const fn gap_range(self) -> (i32, i32) {
        match self {
            Self::Weekly => (6, 8),
            Self::Biweekly => (12, 16),
            Self::Monthly => (27, 34),
            Self::Annual => (355, 375),
        }
    }

    /// The fewest occurrences that can show this cadence.
    const fn min_occurrences(self) -> usize {
        match self {
            Self::Weekly | Self::Biweekly => 4,
            Self::Monthly => 3,
            Self::Annual => 2,
        }
    }

    /// The next date after `last`.
    pub fn next_after(self, last: Day) -> Day {
        match self {
            Self::Weekly => last.plus_days(7),
            Self::Biweekly => last.plus_days(14),
            Self::Monthly => last.plus_months(1),
            Self::Annual => last.plus_months(12),
        }
    }

    const ALL: [Self; 4] = [Self::Weekly, Self::Biweekly, Self::Monthly, Self::Annual];
}

/// A detected series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Series {
    /// How often it repeats.
    pub cadence: Cadence,
    /// The typical amount (median), in minor units. Negative is money out.
    pub amount: i64,
    /// How far amounts usually are from the typical amount (MAD).
    pub spread: u64,
    /// The latest date.
    pub last: Day,
    /// The next expected date. Later than or equal to `today` if the series is
    /// still active.
    pub next: Day,
    /// How many transactions match.
    pub occurrences: usize,
    /// 0 to 10,000. The UI says "expected" only at 8,000 or more (§7.4.3).
    pub confidence: BasisPoints,
    /// False if the series missed its last expected date by more than its
    /// tolerance: it probably stopped.
    pub active: bool,
}

/// The confidence at which a charge counts as expected (§7.4.3: 0.8).
pub const EXPECTED_CONFIDENCE: BasisPoints = BasisPoints(8000);

/// True if `amount` is inside the series' amount band: within 10% of the
/// typical amount, or within 2 × spread for variable bills (§7.4.3).
pub fn amount_fits(series_amount: i64, spread: u64, amount: i64) -> bool {
    let typical = i128::from(series_amount);
    let diff = (i128::from(amount) - typical).abs();
    let ten_percent = typical.abs() / 10;
    let band = ten_percent.max(2 * i128::from(spread));
    diff <= band
}

/// Finds a series in one merchant's transactions, or `None`.
///
/// `items` are (date, amount) pairs in any order, all in one direction and
/// one currency. `today` decides whether the series is still active.
pub fn detect(items: &[(Day, i64)], today: Day) -> Option<Series> {
    if items.len() < 2 {
        return None;
    }
    let mut items = items.to_vec();
    items.sort_unstable();
    // Two charges on one day count once (for example a retry).
    items.dedup_by_key(|(day, _)| *day);

    let gaps: Vec<i32> = items
        .windows(2)
        .map(|w| w[0].0.days_until(w[1].0))
        .collect();
    let typical_gap = i32::try_from(median(
        &gaps.iter().map(|g| i64::from(*g)).collect::<Vec<_>>(),
    )?)
    .ok()?;
    let cadence = Cadence::ALL.into_iter().find(|c| {
        let (lo, hi) = c.gap_range();
        (lo..=hi).contains(&typical_gap)
    })?;
    if items.len() < cadence.min_occurrences() {
        return None;
    }

    let amounts: Vec<i64> = items.iter().map(|(_, a)| *a).collect();
    let amount = median(&amounts)?;
    let spread = mad(&amounts)?;

    let (lo, hi) = cadence.gap_range();
    let regular = gaps.iter().filter(|g| (lo..=hi).contains(*g)).count();
    let fitting = amounts
        .iter()
        .filter(|a| amount_fits(amount, spread, **a))
        .count();

    // Confidence: share of regular gaps × share of fitting amounts × a
    // factor for how much history there is.
    let gap_bp = share_bp(regular, gaps.len());
    let amount_bp = share_bp(fitting, amounts.len());
    let history_bp: i64 = match items.len() {
        0..=2 => 7000,
        3 => 8500,
        4 => 9000,
        5 => 9500,
        _ => 10_000,
    };
    let confidence = gap_bp * amount_bp / 10_000 * history_bp / 10_000;

    let last = items.last()?.0;
    let mut next = cadence.next_after(last);
    // Tolerance: half a cadence step, at least 3 days.
    let tolerance = ((hi - lo) / 2).max(3);
    let active = last.days_until(today) <= last.days_until(next) + tolerance;
    if active {
        while next < today {
            next = cadence.next_after(next);
        }
    }
    Some(Series {
        cadence,
        amount,
        spread,
        last,
        next,
        occurrences: items.len(),
        confidence: BasisPoints(i32::try_from(confidence).unwrap_or(0)),
        active,
    })
}

fn share_bp(part: usize, whole: usize) -> i64 {
    if whole == 0 {
        return 0;
    }
    let part = i64::try_from(part).unwrap_or(i64::MAX);
    let whole = i64::try_from(whole).unwrap_or(i64::MAX);
    part * 10_000 / whole
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u8, d: u8) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    #[test]
    fn finds_a_monthly_subscription() {
        let items: Vec<(Day, i64)> = (0..6)
            .map(|i| (day(2026, 5, 15).plus_months(i), -1549))
            .collect();
        let s = detect(&items, day(2026, 11, 1)).unwrap();
        assert_eq!(s.cadence, Cadence::Monthly);
        assert_eq!(s.amount, -1549);
        assert_eq!(s.spread, 0);
        assert_eq!(s.last.ymd(), (2026, 10, 15));
        assert_eq!(s.next.ymd(), (2026, 11, 15));
        assert_eq!(s.confidence, BasisPoints(10_000));
        assert!(s.active);
    }

    #[test]
    fn varying_bill_and_day_still_monthly() {
        let items = [
            (day(2026, 6, 3), -8210),
            (day(2026, 7, 2), -7955),
            (day(2026, 8, 4), -9120),
            (day(2026, 9, 3), -8600),
            (day(2026, 10, 1), -8333),
        ];
        let s = detect(&items, day(2026, 10, 20)).unwrap();
        assert_eq!(s.cadence, Cadence::Monthly);
        assert!(s.confidence >= EXPECTED_CONFIDENCE, "{:?}", s.confidence);
        assert_eq!(s.next.ymd(), (2026, 11, 1));
    }

    #[test]
    fn biweekly_paycheck() {
        let items: Vec<(Day, i64)> = (0..8)
            .map(|i| (day(2026, 7, 3).plus_days(14 * i), 250_000))
            .collect();
        let s = detect(&items, day(2026, 10, 25)).unwrap();
        assert_eq!(s.cadence, Cadence::Biweekly);
        assert_eq!(s.amount, 250_000);
        assert!(s.active);
        assert!(s.next >= day(2026, 10, 25));
    }

    #[test]
    fn irregular_purchases_are_not_a_series() {
        let items = [
            (day(2026, 10, 1), -540),
            (day(2026, 10, 3), -1210),
            (day(2026, 10, 12), -880),
            (day(2026, 10, 13), -400),
            (day(2026, 10, 30), -2300),
        ];
        assert_eq!(detect(&items, day(2026, 11, 1)), None);
    }

    #[test]
    fn too_few_is_not_a_series() {
        let items = [(day(2026, 9, 1), -999), (day(2026, 10, 1), -999)];
        assert_eq!(detect(&items, day(2026, 10, 2)), None);
    }

    #[test]
    fn a_stopped_series_is_inactive() {
        let items: Vec<(Day, i64)> = (0..4)
            .map(|i| (day(2026, 1, 10).plus_months(i), -1000))
            .collect();
        let s = detect(&items, day(2026, 11, 1)).unwrap();
        assert!(!s.active);
        assert_eq!(s.next.ymd(), (2026, 5, 10));
    }

    #[test]
    fn an_outlier_lowers_confidence() {
        let mut items: Vec<(Day, i64)> = (0..6)
            .map(|i| (day(2026, 4, 20).plus_months(i), -1000))
            .collect();
        items[2].1 = -5000;
        let s = detect(&items, day(2026, 10, 1)).unwrap();
        assert_eq!(s.amount, -1000);
        assert!(s.confidence < BasisPoints(10_000));
        assert!(s.confidence >= BasisPoints(8000));
    }

    #[test]
    fn amount_band() {
        assert!(amount_fits(-1000, 0, -1100));
        assert!(!amount_fits(-1000, 0, -1101));
        assert!(amount_fits(-1000, 200, -1400));
        assert!(!amount_fits(-1000, 200, -1401));
    }
}
