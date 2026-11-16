//! Calendar dates as a day count, for recurring detection and budgets.
//!
//! A [`Day`] is the number of days since 1970-01-01 in the proleptic
//! Gregorian calendar. The caller passes "today": this crate reads no clock.
//! The algorithms are from Howard Hinnant, "chrono-Compatible Low-Level Date
//! Algorithms".

/// A calendar date as days since 1970-01-01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Day(pub i32);

impl Day {
    /// The day for a year, month (1–12), and day of month (1–31). Returns
    /// `None` for a date that does not exist.
    pub fn from_ymd(year: i32, month: u8, day: u8) -> Option<Self> {
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        let y = if month <= 2 { year - 1 } else { year };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i32::from(month);
        let mp = if m > 2 { m - 3 } else { m + 9 };
        let doy = (153 * mp + 2) / 5 + i32::from(day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        Some(Self(era * 146_097 + doe - 719_468))
    }

    /// The year, month (1–12), and day of month.
    pub const fn ymd(self) -> (i32, u8, u8) {
        let z = self.0 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
        // The casts are exact: 1 ≤ m ≤ 12 and 1 ≤ d ≤ 31.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "month and day are small and positive"
        )]
        (y, m as u8, d as u8)
    }

    /// This day plus `days`.
    #[must_use]
    pub const fn plus_days(self, days: i32) -> Self {
        Self(self.0 + days)
    }

    /// The same day of the month, `months` later. A day that does not exist
    /// in the target month becomes its last day (Jan 31 + 1 month = Feb 28).
    #[must_use]
    pub fn plus_months(self, months: i32) -> Self {
        let (y, m, d) = self.ymd();
        let index = y * 12 + i32::from(m) - 1 + months;
        let year = index.div_euclid(12);
        // 1 ≤ month ≤ 12.
        let month = u8::try_from(index.rem_euclid(12) + 1).unwrap_or(1);
        let day = d.min(days_in_month(year, month));
        Self::from_ymd(year, month, day).unwrap_or(self)
    }

    /// The first day of this day's month.
    #[must_use]
    pub fn month_start(self) -> Self {
        let (y, m, _) = self.ymd();
        Self::from_ymd(y, m, 1).unwrap_or(self)
    }

    /// Days from `self` to `later`. Negative if `later` is earlier.
    pub const fn days_until(self, later: Self) -> i32 {
        later.0 - self.0
    }
}

const fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The number of days in a month (1–12).
pub const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_days() {
        assert_eq!(Day::from_ymd(1970, 1, 1), Some(Day(0)));
        assert_eq!(Day::from_ymd(2000, 3, 1), Some(Day(11_017)));
        assert_eq!(Day::from_ymd(2026, 11, 16), Some(Day(20_773)));
        assert_eq!(Day(20_773).ymd(), (2026, 11, 16));
        assert_eq!(Day(-1).ymd(), (1969, 12, 31));
        assert_eq!(Day::from_ymd(2026, 2, 29), None);
        assert_eq!(Day::from_ymd(2026, 13, 1), None);
    }

    #[test]
    fn round_trips_every_day_for_800_years() {
        let start = Day::from_ymd(1800, 1, 1).unwrap().0;
        let end = Day::from_ymd(2600, 1, 1).unwrap().0;
        for n in start..end {
            let (y, m, d) = Day(n).ymd();
            assert_eq!(Day::from_ymd(y, m, d), Some(Day(n)));
        }
    }

    #[test]
    fn adds_months_with_clamping() {
        let jan31 = Day::from_ymd(2026, 1, 31).unwrap();
        assert_eq!(jan31.plus_months(1).ymd(), (2026, 2, 28));
        assert_eq!(jan31.plus_months(13).ymd(), (2027, 2, 28));
        assert_eq!(
            Day::from_ymd(2028, 1, 31).unwrap().plus_months(1).ymd(),
            (2028, 2, 29)
        );
        assert_eq!(
            Day::from_ymd(2026, 12, 15).unwrap().plus_months(1).ymd(),
            (2027, 1, 15)
        );
        assert_eq!(
            Day::from_ymd(2026, 3, 15).unwrap().plus_months(-3).ymd(),
            (2025, 12, 15)
        );
        assert_eq!(
            Day::from_ymd(2026, 11, 16).unwrap().month_start().ymd(),
            (2026, 11, 1)
        );
    }
}
