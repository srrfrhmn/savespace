use crate::Currency;

/// An exact amount of money: a signed count of minor units in one currency.
///
/// `Money` never uses floating-point numbers. All arithmetic is checked and
/// returns an error on overflow or currency mismatch. It never panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Money {
    minor: i64,
    currency: Currency,
}

/// The error for a money operation that cannot give an exact result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MoneyError {
    /// The result does not fit in an `i64` count of minor units.
    #[error("money arithmetic overflow")]
    Overflow,
    /// The operands have different currencies.
    #[error("currency mismatch: {left} and {right}")]
    CurrencyMismatch {
        /// The currency of the left operand.
        left: Currency,
        /// The currency of the right operand.
        right: Currency,
    },
    /// The divisor is zero.
    #[error("division by zero")]
    DivisionByZero,
}

/// The error for an invalid [`Money::allocate`] request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AllocationError {
    /// The weight list is empty.
    #[error("allocation needs at least one weight")]
    NoWeights,
    /// All weights are zero.
    #[error("allocation weights sum to zero")]
    ZeroTotalWeight,
}

impl Money {
    /// Creates an amount from a count of minor units (cents for USD).
    pub const fn from_minor(minor: i64, currency: Currency) -> Self {
        Self { minor, currency }
    }

    /// Creates a zero amount.
    pub const fn zero(currency: Currency) -> Self {
        Self::from_minor(0, currency)
    }

    /// The count of minor units.
    pub const fn minor(self) -> i64 {
        self.minor
    }

    /// The currency.
    pub const fn currency(self) -> Currency {
        self.currency
    }

    /// Returns `true` if the amount is zero.
    pub const fn is_zero(self) -> bool {
        self.minor == 0
    }

    /// Returns `true` if the amount is less than zero.
    pub const fn is_negative(self) -> bool {
        self.minor < 0
    }

    pub(crate) fn same_currency(self, rhs: Money) -> Result<(), MoneyError> {
        if self.currency == rhs.currency {
            Ok(())
        } else {
            Err(MoneyError::CurrencyMismatch {
                left: self.currency,
                right: rhs.currency,
            })
        }
    }

    fn with_minor(self, minor: Option<i64>) -> Result<Money, MoneyError> {
        minor
            .map(|m| Money::from_minor(m, self.currency))
            .ok_or(MoneyError::Overflow)
    }

    /// Adds two amounts in the same currency.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::CurrencyMismatch`] if the currencies differ, or
    /// [`MoneyError::Overflow`] if the result does not fit in an `i64`.
    pub fn checked_add(self, rhs: Money) -> Result<Money, MoneyError> {
        self.same_currency(rhs)?;
        self.with_minor(self.minor.checked_add(rhs.minor))
    }

    /// Subtracts `rhs` from `self`. Both must have the same currency.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::CurrencyMismatch`] if the currencies differ, or
    /// [`MoneyError::Overflow`] if the result does not fit in an `i64`.
    pub fn checked_sub(self, rhs: Money) -> Result<Money, MoneyError> {
        self.same_currency(rhs)?;
        self.with_minor(self.minor.checked_sub(rhs.minor))
    }

    /// Negates the amount.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::Overflow`] if the result does not fit in an `i64`.
    pub fn checked_neg(self) -> Result<Money, MoneyError> {
        self.with_minor(self.minor.checked_neg())
    }

    /// Returns the absolute value of the amount.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::Overflow`] if the result does not fit in an `i64`.
    pub fn checked_abs(self) -> Result<Money, MoneyError> {
        self.with_minor(self.minor.checked_abs())
    }

    /// Multiplies the amount by an integer.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::Overflow`] if the result does not fit in an `i64`.
    pub fn checked_mul(self, factor: i64) -> Result<Money, MoneyError> {
        self.with_minor(self.minor.checked_mul(factor))
    }

    /// Adds all amounts. Every amount must be in `currency`.
    ///
    /// The result does not depend on the order of the amounts: the sum uses an
    /// `i128` accumulator and checks the range once at the end.
    ///
    /// # Errors
    ///
    /// Returns [`MoneyError::CurrencyMismatch`] if an amount is not in `currency`, or
    /// [`MoneyError::Overflow`] if the total does not fit in an `i64`.
    pub fn sum<I>(currency: Currency, amounts: I) -> Result<Money, MoneyError>
    where
        I: IntoIterator<Item = Money>,
    {
        let zero = Money::zero(currency);
        let mut total: i128 = 0;
        for m in amounts {
            zero.same_currency(m)?;
            total += i128::from(m.minor);
        }
        let minor = i64::try_from(total).map_err(|_| MoneyError::Overflow)?;
        Ok(Money::from_minor(minor, currency))
    }

    /// Splits the amount into parts in proportion to `weights`.
    ///
    /// The parts always add up to exactly `self`. Each part differs from its
    /// exact share by less than one minor unit. Method: largest remainder.
    /// Each part starts as the floor of its exact share. The remaining minor
    /// units go one each to the parts with the largest fractional remainder.
    /// Ties go to the earlier part. Use this for split transactions.
    ///
    /// # Errors
    ///
    /// Returns [`AllocationError::NoWeights`] for an empty `weights` list, and
    /// [`AllocationError::ZeroTotalWeight`] if every weight is zero.
    ///
    /// # Panics
    ///
    /// Does not panic. The `expect` calls guard arithmetic invariants.
    pub fn allocate(self, weights: &[u32]) -> Result<Vec<Money>, AllocationError> {
        if weights.is_empty() {
            return Err(AllocationError::NoWeights);
        }
        let total_weight: i128 = weights.iter().map(|w| i128::from(*w)).sum();
        if total_weight == 0 {
            return Err(AllocationError::ZeroTotalWeight);
        }
        let amount = i128::from(self.minor);
        let mut shares = Vec::with_capacity(weights.len());
        let mut fractions = Vec::with_capacity(weights.len());
        for (index, w) in weights.iter().enumerate() {
            let exact = amount * i128::from(*w);
            shares.push(exact.div_euclid(total_weight));
            fractions.push((exact.rem_euclid(total_weight), index));
        }
        // Floor shares never add up to more than the amount, and the gap is
        // smaller than the number of parts.
        let remainder = amount - shares.iter().sum::<i128>();
        let remainder = usize::try_from(remainder).expect("the remainder is in 0..parts");
        // Largest fraction first; ties keep index order.
        fractions.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for &(_, index) in fractions.iter().take(remainder) {
            shares[index] += 1;
        }
        Ok(shares
            .into_iter()
            .map(|s| {
                let minor = i64::try_from(s).expect("a share lies between zero and the amount");
                Money::from_minor(minor, self.currency)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn usd(minor: i64) -> Money {
        Money::from_minor(minor, Currency::Usd)
    }

    #[test]
    fn add_and_sub() {
        assert_eq!(usd(150).checked_add(usd(50)), Ok(usd(200)));
        assert_eq!(usd(150).checked_sub(usd(200)), Ok(usd(-50)));
    }

    #[test]
    fn overflow_is_an_error() {
        assert_eq!(usd(i64::MAX).checked_add(usd(1)), Err(MoneyError::Overflow));
        assert_eq!(usd(i64::MIN).checked_neg(), Err(MoneyError::Overflow));
        assert_eq!(usd(i64::MIN).checked_abs(), Err(MoneyError::Overflow));
        assert_eq!(usd(i64::MAX).checked_mul(2), Err(MoneyError::Overflow));
    }

    #[test]
    fn currency_mismatch_is_an_error() {
        let eur = Money::from_minor(1, Currency::Eur);
        assert_eq!(
            usd(1).checked_add(eur),
            Err(MoneyError::CurrencyMismatch {
                left: Currency::Usd,
                right: Currency::Eur
            })
        );
        assert!(Money::sum(Currency::Usd, [usd(1), eur]).is_err());
    }

    #[test]
    fn sum_tolerates_intermediate_overflow() {
        let total = Money::sum(Currency::Usd, [usd(i64::MAX), usd(1), usd(-1)]);
        assert_eq!(total, Ok(usd(i64::MAX)));
    }

    #[test]
    fn allocate_three_ways() {
        let parts = usd(100).allocate(&[1, 1, 1]).unwrap();
        assert_eq!(parts, vec![usd(34), usd(33), usd(33)]);
    }

    #[test]
    fn allocate_negative() {
        let parts = usd(-100).allocate(&[1, 1, 1]).unwrap();
        assert_eq!(parts, vec![usd(-33), usd(-33), usd(-34)]);
        assert_eq!(Money::sum(Currency::Usd, parts), Ok(usd(-100)));
    }

    #[test]
    fn allocate_gives_remainder_to_largest_fraction() {
        // Exact shares: -38008, -29561.78, -46454.22. Floors: -38008 (fraction
        // 0), -29562 (0.22), -46455 (0.78). The leftover unit goes to the part
        // with the largest fraction, not to the first part (proptest regression).
        let parts = usd(-114_024).allocate(&[36, 28, 44]).unwrap();
        assert_eq!(parts, vec![usd(-38_008), usd(-29_562), usd(-46_454)]);
    }

    #[test]
    fn allocate_errors() {
        assert_eq!(usd(1).allocate(&[]), Err(AllocationError::NoWeights));
        assert_eq!(
            usd(1).allocate(&[0, 0]),
            Err(AllocationError::ZeroTotalWeight)
        );
    }
}
