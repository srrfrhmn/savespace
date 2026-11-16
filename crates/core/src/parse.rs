//! Exact parsing of decimal amounts, for example from a bank API.
//!
//! Aggregators send amounts as decimal text or JSON numbers ("12.34"). This
//! module turns the text into minor units without floating point.

use crate::{Currency, Money};

/// Errors from [`parse_decimal`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseAmountError {
    /// The text is not a plain decimal number.
    #[error("not a decimal amount: {0:?}")]
    Syntax(String),
    /// The amount has more decimal places than the currency allows, and the
    /// extra digits are not zero.
    #[error("{text:?} has more than {exponent} decimal places")]
    Precision {
        /// The input text.
        text: String,
        /// The currency exponent.
        exponent: u8,
    },
    /// The amount does not fit in an `i64` count of minor units.
    #[error("amount out of range: {0:?}")]
    Overflow(String),
}

/// Parses decimal text into [`Money`]: an optional sign, digits, and an
/// optional fraction. Exponent notation (`1e3`) is accepted only when it
/// gives an exact amount, because JSON encoders sometimes use it.
///
/// Extra fraction digits must be zero (`12.340` is fine for USD, `12.345`
/// is an error). This never rounds.
///
/// # Errors
///
/// [`ParseAmountError`] for bad syntax, lost precision, or overflow.
pub fn parse_decimal(text: &str, currency: Currency) -> Result<Money, ParseAmountError> {
    let syntax = || ParseAmountError::Syntax(text.to_owned());
    let s = text.trim();
    let (negative, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (mantissa, exp10) = match s.find(['e', 'E']) {
        Some(i) => {
            let e: i32 = s[i + 1..].parse().map_err(|_| syntax())?;
            (&s[..i], e)
        }
        None => (s, 0),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(syntax());
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(syntax());
    }

    // All digits, and the position of the decimal point after scaling.
    let digits: String = format!("{int_part}{frac_part}");
    let frac_len = i32::try_from(frac_part.len()).map_err(|_| syntax())?;
    // The number is digits × 10^(exp10 − frac_len). Minor units multiply by
    // 10^exponent.
    let shift = exp10 - frac_len + i32::from(currency.exponent());

    let digits = digits.trim_start_matches('0');
    let overflow = || ParseAmountError::Overflow(text.to_owned());
    let minor: i128 = if digits.is_empty() {
        0
    } else if shift >= 0 {
        if digits.len() + usize::try_from(shift).map_err(|_| overflow())? > 20 {
            return Err(overflow());
        }
        let base: i128 = digits.parse().map_err(|_| overflow())?;
        let factor = 10_i128
            .checked_pow(u32::try_from(shift).map_err(|_| overflow())?)
            .ok_or_else(overflow)?;
        base.checked_mul(factor).ok_or_else(overflow)?
    } else {
        let drop = usize::try_from(-shift).map_err(|_| overflow())?;
        let (keep, extra) = if drop >= digits.len() {
            ("", digits)
        } else {
            digits.split_at(digits.len() - drop)
        };
        if extra.bytes().any(|b| b != b'0') {
            return Err(ParseAmountError::Precision {
                text: text.to_owned(),
                exponent: currency.exponent(),
            });
        }
        if keep.is_empty() {
            0
        } else {
            if keep.len() > 20 {
                return Err(overflow());
            }
            keep.parse().map_err(|_| overflow())?
        }
    };
    let minor = if negative { -minor } else { minor };
    let minor = i64::try_from(minor).map_err(|_| overflow())?;
    Ok(Money::from_minor(minor, currency))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(text: &str) -> Result<i64, ParseAmountError> {
        parse_decimal(text, Currency::Usd).map(Money::minor)
    }

    #[test]
    fn parses_common_forms() {
        assert_eq!(usd("12.34"), Ok(1234));
        assert_eq!(usd("-12.34"), Ok(-1234));
        assert_eq!(usd("+0.5"), Ok(50));
        assert_eq!(usd("7"), Ok(700));
        assert_eq!(usd(".99"), Ok(99));
        assert_eq!(usd("5."), Ok(500));
        assert_eq!(usd("0"), Ok(0));
        assert_eq!(usd("-0.00"), Ok(0));
        assert_eq!(usd("12.340"), Ok(1234));
        assert_eq!(usd("1e2"), Ok(10_000));
        assert_eq!(usd("1.5E1"), Ok(1500));
        assert_eq!(usd("1234e-2"), Ok(1234));
        assert_eq!(
            parse_decimal("1800", Currency::Jpy).map(Money::minor),
            Ok(1800)
        );
    }

    #[test]
    fn rejects_lost_precision() {
        assert!(matches!(
            usd("12.345"),
            Err(ParseAmountError::Precision { .. })
        ));
        assert!(matches!(
            usd("1e-3"),
            Err(ParseAmountError::Precision { .. })
        ));
        assert!(matches!(
            parse_decimal("10.5", Currency::Jpy),
            Err(ParseAmountError::Precision { .. })
        ));
    }

    #[test]
    fn rejects_bad_syntax() {
        for bad in [
            "", "-", ".", "1.2.3", "1,000", "abc", "NaN", "1e", "--1", "1 2",
        ] {
            assert!(
                matches!(usd(bad), Err(ParseAmountError::Syntax(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn rejects_overflow() {
        assert!(matches!(usd("1e30"), Err(ParseAmountError::Overflow(_))));
        assert!(matches!(
            usd("92233720368547758.08"),
            Err(ParseAmountError::Overflow(_))
        ));
        assert_eq!(usd("92233720368547758.07"), Ok(i64::MAX));
    }

    proptest::proptest! {
        #[test]
        fn round_trips_cents(minor in -1_000_000_000_000_i64..1_000_000_000_000) {
            let sign = if minor < 0 { "-" } else { "" };
            let abs = minor.unsigned_abs();
            let text = format!("{sign}{}.{:02}", abs / 100, abs % 100);
            proptest::prop_assert_eq!(usd(&text), Ok(minor));
        }
    }
}
