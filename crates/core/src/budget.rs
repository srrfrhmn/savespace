//! Flex budget math and the calm-mode state (design doc §6.1, §5 Mental
//! accounting, calm mode).
//!
//! Three buckets: fixed, flexible, and non-monthly. A non-monthly plan is a
//! yearly amount, set aside in 12 parts, so a large yearly cost is not "over
//! budget" in the month it posts. Spending is a positive amount here.

use crate::{Money, MoneyError};

/// What is left of a monthly plan: `plan + carried − spent`. Negative means
/// more was spent than planned.
///
/// # Errors
///
/// [`MoneyError`] for mixed currencies or overflow.
pub fn left(plan: Money, carried: Money, spent: Money) -> Result<Money, MoneyError> {
    plan.checked_add(carried)?.checked_sub(spent)
}

/// The rollover into a month: the sum of `plan − spent` over the earlier
/// months, oldest first. Unspent money carries forward; overspending carries
/// as a negative amount.
///
/// # Errors
///
/// [`MoneyError`] for mixed currencies or overflow.
pub fn rollover(
    currency: crate::Currency,
    history: &[(Money, Money)],
) -> Result<Money, MoneyError> {
    history
        .iter()
        .try_fold(Money::zero(currency), |carry, (plan, spent)| {
            carry.checked_add(*plan)?.checked_sub(*spent)
        })
}

/// The part of a yearly plan to set aside by the end of `month` (1–12): the
/// yearly amount in 12 parts that differ by at most one minor unit, summed for
/// months 1 to `month`.
///
/// # Errors
///
/// [`MoneyError::Overflow`] if the amounts overflow. A month outside 1–12 is
/// treated as 12.
pub fn set_aside_by_month(yearly: Money, month: u8) -> Result<Money, MoneyError> {
    let parts = yearly
        .allocate(&[1; 12])
        .map_err(|_| MoneyError::Overflow)?;
    let months = usize::from(month.clamp(1, 12));
    Money::sum(yearly.currency(), parts.into_iter().take(months))
}

/// The calm-mode state (§6.1): a word instead of a balance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalmState {
    /// Cash covers the expected bills and the rest of the flexible plan.
    Covered,
    /// Cash covers the expected bills, not all of the flexible plan.
    Tight,
    /// Cash may not cover the expected bills. Never hidden (§5 Gentle exposure).
    NeedsAttention,
}

impl CalmState {
    /// The stable name: `covered`, `tight`, or `needs_attention`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Covered => "covered",
            Self::Tight => "tight",
            Self::NeedsAttention => "needs_attention",
        }
    }
}

/// The calm state from cash on hand, the bills expected before the month
/// ends, and what is left of the flexible plan.
///
/// - `NeedsAttention`: cash < bills.
/// - `Tight`: cash < bills + flexible left.
/// - `Covered`: otherwise.
///
/// A negative `flex_left` counts as zero.
///
/// # Errors
///
/// [`MoneyError`] for mixed currencies or overflow.
pub fn calm_state(cash: Money, bills: Money, flex_left: Money) -> Result<CalmState, MoneyError> {
    cash.same_currency(bills)?;
    if cash.minor() < bills.minor() {
        return Ok(CalmState::NeedsAttention);
    }
    let flex = if flex_left.is_negative() {
        Money::zero(flex_left.currency())
    } else {
        flex_left
    };
    let needed = bills.checked_add(flex)?;
    Ok(if cash.minor() < needed.minor() {
        CalmState::Tight
    } else {
        CalmState::Covered
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Currency;

    fn usd(minor: i64) -> Money {
        Money::from_minor(minor, Currency::Usd)
    }

    #[test]
    fn left_and_rollover() {
        assert_eq!(left(usd(40_000), usd(0), usd(31_000)), Ok(usd(9000)));
        assert_eq!(left(usd(40_000), usd(-5000), usd(41_000)), Ok(usd(-6000)));
        let history = [(usd(40_000), usd(30_000)), (usd(40_000), usd(45_000))];
        assert_eq!(rollover(Currency::Usd, &history), Ok(usd(5000)));
        assert_eq!(rollover(Currency::Usd, &[]), Ok(usd(0)));
        assert!(matches!(
            left(usd(1), usd(0), Money::from_minor(1, Currency::Eur)),
            Err(MoneyError::CurrencyMismatch { .. })
        ));
    }

    #[test]
    fn yearly_plans_are_set_aside_in_twelve_parts() {
        assert_eq!(set_aside_by_month(usd(120_000), 1), Ok(usd(10_000)));
        assert_eq!(set_aside_by_month(usd(120_000), 6), Ok(usd(60_000)));
        assert_eq!(set_aside_by_month(usd(120_000), 12), Ok(usd(120_000)));
        // 100.01 / 12: the parts differ by one cent and add up exactly.
        assert_eq!(set_aside_by_month(usd(10_001), 12), Ok(usd(10_001)));
        let first = set_aside_by_month(usd(10_001), 1).unwrap().minor();
        assert!(first == 833 || first == 834);
    }

    #[test]
    fn calm_states() {
        assert_eq!(
            calm_state(usd(100_000), usd(50_000), usd(30_000)),
            Ok(CalmState::Covered)
        );
        assert_eq!(
            calm_state(usd(70_000), usd(50_000), usd(30_000)),
            Ok(CalmState::Tight)
        );
        assert_eq!(
            calm_state(usd(40_000), usd(50_000), usd(0)),
            Ok(CalmState::NeedsAttention)
        );
        // Overspent flex does not make the state worse than the bills say.
        assert_eq!(
            calm_state(usd(60_000), usd(50_000), usd(-9000)),
            Ok(CalmState::Covered)
        );
        assert_eq!(
            calm_state(usd(50_000), usd(50_000), usd(0)),
            Ok(CalmState::Covered)
        );
    }
}
