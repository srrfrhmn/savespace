use std::fmt;

use crate::{Money, MoneyError};

/// A ratio in basis points: 10,000 basis points = 100%.
///
/// SaveSpace uses basis points instead of floating-point numbers for every
/// ratio (budget used, change from baseline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BasisPoints(pub i32);

impl BasisPoints {
    /// 100%.
    pub const ONE: BasisPoints = BasisPoints(10_000);
}

impl fmt::Display for BasisPoints {
    /// Formats as a percent with up to two decimals, for example `"12.5%"`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let whole = abs / 100;
        let frac = abs % 100;
        match frac {
            0 => write!(f, "{sign}{whole}%"),
            f2 if f2 % 10 == 0 => write!(f, "{sign}{whole}.{}%", f2 / 10),
            f2 => write!(f, "{sign}{whole}.{f2:02}%"),
        }
    }
}

/// Divides `num` by `den` and rounds half away from zero.
fn div_round_half_away(num: i128, den: i128) -> i128 {
    let q = num / den;
    let r = num % den;
    if 2 * r.abs() >= den.abs() {
        q + num.signum() * den.signum()
    } else {
        q
    }
}

impl Money {
    /// Returns `self / denominator` in basis points, rounded half away from zero.
    ///
    /// Both amounts must have the same currency.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::CurrencyMismatch`] if the currencies differ,
    /// [`MoneyError::DivisionByZero`] if `denominator` is zero, or
    /// [`MoneyError::Overflow`] if the ratio does not fit in an `i32`.
    pub fn ratio_to(self, denominator: Money) -> Result<BasisPoints, MoneyError> {
        self.same_currency(denominator)?;
        if denominator.is_zero() {
            return Err(MoneyError::DivisionByZero);
        }
        let bp = div_round_half_away(
            i128::from(self.minor()) * 10_000,
            i128::from(denominator.minor()),
        );
        i32::try_from(bp)
            .map(BasisPoints)
            .map_err(|_| MoneyError::Overflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Currency;

    const fn usd(minor: i64) -> Money {
        Money::from_minor(minor, Currency::Usd)
    }

    #[test]
    fn ratio_rounds_half_away_from_zero() {
        assert_eq!(usd(1).ratio_to(usd(3)), Ok(BasisPoints(3_333)));
        assert_eq!(usd(2).ratio_to(usd(3)), Ok(BasisPoints(6_667)));
        assert_eq!(usd(-2).ratio_to(usd(3)), Ok(BasisPoints(-6_667)));
        assert_eq!(usd(1).ratio_to(usd(20_000)), Ok(BasisPoints(1)));
    }

    #[test]
    fn ratio_errors() {
        assert_eq!(usd(1).ratio_to(usd(0)), Err(MoneyError::DivisionByZero));
        assert_eq!(usd(i64::MAX).ratio_to(usd(1)), Err(MoneyError::Overflow));
    }

    #[test]
    fn display() {
        assert_eq!(BasisPoints(1_250).to_string(), "12.5%");
        assert_eq!(BasisPoints(10_000).to_string(), "100%");
        assert_eq!(BasisPoints(-5).to_string(), "-0.05%");
    }
}
