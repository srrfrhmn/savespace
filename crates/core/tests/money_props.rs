//! Property tests for money arithmetic (design doc §14.3).

use proptest::prelude::*;
use savespace_core::{Currency, Locale, Money, format_money, stats};

fn usd(minor: i64) -> Money {
    Money::from_minor(minor, Currency::Usd)
}

proptest! {
    #[test]
    fn add_then_sub_round_trips(a in any::<i64>(), b in any::<i64>()) {
        if let Ok(sum) = usd(a).checked_add(usd(b)) {
            prop_assert_eq!(sum.checked_sub(usd(b)), Ok(usd(a)));
        }
    }

    #[test]
    fn add_matches_wide_math(a in any::<i64>(), b in any::<i64>()) {
        let wide = i128::from(a) + i128::from(b);
        match usd(a).checked_add(usd(b)) {
            Ok(m) => prop_assert_eq!(i128::from(m.minor()), wide),
            Err(_) => prop_assert!(i64::try_from(wide).is_err()),
        }
    }

    #[test]
    fn sum_is_order_independent(mut values in prop::collection::vec(any::<i64>(), 0..32)) {
        let forward = Money::sum(Currency::Usd, values.iter().copied().map(usd));
        values.reverse();
        let backward = Money::sum(Currency::Usd, values.iter().copied().map(usd));
        prop_assert_eq!(forward, backward);
    }

    #[test]
    fn allocate_sums_to_total(
        amount in any::<i64>(),
        weights in prop::collection::vec(0_u32..10_000, 1..12),
    ) {
        prop_assume!(weights.iter().any(|w| *w > 0));
        let parts = usd(amount).allocate(&weights).unwrap();
        prop_assert_eq!(parts.len(), weights.len());
        prop_assert_eq!(Money::sum(Currency::Usd, parts), Ok(usd(amount)));
    }

    #[test]
    fn allocate_parts_differ_from_exact_share_by_less_than_one(
        amount in -1_000_000_i64..1_000_000,
        weights in prop::collection::vec(1_u32..100, 1..8),
    ) {
        let total: i128 = weights.iter().map(|w| i128::from(*w)).sum();
        let parts = usd(amount).allocate(&weights).unwrap();
        for (part, w) in parts.iter().zip(&weights) {
            // |part * total - amount * w| < total  <=>  |part - exact| < 1
            let diff = i128::from(part.minor()) * total - i128::from(amount) * i128::from(*w);
            prop_assert!(diff.abs() < total);
        }
    }

    #[test]
    fn format_never_panics(minor in any::<i64>(), idx in 0_usize..Currency::ALL.len()) {
        let s = format_money(Money::from_minor(minor, Currency::ALL[idx]), Locale::EnUs);
        prop_assert!(!s.is_empty());
        prop_assert_eq!(s.starts_with('-'), minor < 0);
    }

    #[test]
    fn median_lies_within_range(values in prop::collection::vec(any::<i64>(), 1..64)) {
        let m = stats::median(&values).unwrap();
        prop_assert!(m >= *values.iter().min().unwrap());
        prop_assert!(m <= *values.iter().max().unwrap());
    }
}
