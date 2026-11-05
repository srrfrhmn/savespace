use std::fmt::Write as _;

use crate::{Currency, Money};

/// A display locale.
///
/// Phase 1 supports `en-US` only (design doc §17.2). Adopt ICU4X when SaveSpace
/// adds a second locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Locale {
    /// English (United States).
    #[default]
    EnUs,
}

const fn symbol_en_us(currency: Currency) -> &'static str {
    match currency {
        Currency::Usd => "$",
        Currency::Eur => "€",
        Currency::Gbp => "£",
        Currency::Cad => "CA$",
        Currency::Aud => "A$",
        Currency::Jpy => "¥",
    }
}

/// Formats an amount for display, as the insight copy style guide requires
/// (design doc Appendix B.3).
///
/// - Whole amounts have no decimals: `$1,800`.
/// - Other amounts show every minor-unit digit: `$15.49`, `$0.05`.
/// - Negative amounts have a leading minus sign: `-$3.20`.
/// - Thousands use a comma separator.
pub fn format_money(money: Money, locale: Locale) -> String {
    let Locale::EnUs = locale;
    let currency = money.currency();
    let per_major = currency.minor_per_major();
    let abs = money.minor().unsigned_abs();
    let major = abs / per_major;
    let minor = abs % per_major;

    let mut out = String::new();
    if money.is_negative() {
        out.push('-');
    }
    out.push_str(symbol_en_us(currency));
    out.push_str(&group_thousands(major));
    if minor != 0 {
        let width = usize::from(currency.exponent());
        // Writing to a String cannot fail.
        let _ = write!(out, ".{minor:0width$}");
    }
    out
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(minor: i64, currency: Currency) -> String {
        format_money(Money::from_minor(minor, currency), Locale::EnUs)
    }

    #[test]
    fn whole_and_fractional() {
        assert_eq!(fmt(180_000, Currency::Usd), "$1,800");
        assert_eq!(fmt(1_549, Currency::Usd), "$15.49");
        assert_eq!(fmt(5, Currency::Usd), "$0.05");
        assert_eq!(fmt(-320, Currency::Usd), "-$3.20");
        assert_eq!(fmt(0, Currency::Usd), "$0");
    }

    #[test]
    fn zero_exponent_currency() {
        assert_eq!(fmt(1_800, Currency::Jpy), "¥1,800");
    }

    #[test]
    fn extremes() {
        assert_eq!(fmt(i64::MIN, Currency::Usd), "-$92,233,720,368,547,758.08");
        assert_eq!(fmt(i64::MAX, Currency::Jpy), "¥9,223,372,036,854,775,807");
    }

    #[test]
    fn grouping() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }
}
