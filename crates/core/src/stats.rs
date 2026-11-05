//! Robust statistics for personal baselines (design doc §7.3).
//!
//! All functions use integer math. Inputs are counts of minor units.

/// Returns the median of `values`, or `None` if `values` is empty.
///
/// For an even number of values, the median is the midpoint of the two middle
/// values, rounded toward negative infinity.
///
/// # Panics
///
/// Does not panic. The `expect` call guards an arithmetic invariant.
pub fn median(values: &[i64]) -> Option<i64> {
    let wide: Vec<i128> = values.iter().map(|v| i128::from(*v)).collect();
    median_i128(wide).map(|m| i64::try_from(m).expect("the median lies inside the input range"))
}

/// Returns the median absolute deviation (MAD) of `values`, or `None` if
/// `values` is empty.
///
/// MAD = median(|x - median(values)|). The same rounding rule as [`median`]
/// applies at each step.
///
/// # Panics
///
/// Does not panic. The `expect` call guards an arithmetic invariant.
pub fn mad(values: &[i64]) -> Option<u64> {
    let wide: Vec<i128> = values.iter().map(|v| i128::from(*v)).collect();
    let center = median_i128(wide.clone())?;
    let deviations: Vec<i128> = wide.into_iter().map(|v| (v - center).abs()).collect();
    median_i128(deviations)
        .map(|m| u64::try_from(m).expect("a deviation between two i64 values fits in u64"))
}

fn median_i128(mut values: Vec<i128>) -> Option<i128> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[mid])
    } else {
        Some((values[mid - 1] + values[mid]).div_euclid(2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_odd_and_even() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[5]), Some(5));
        assert_eq!(median(&[3, 1, 2]), Some(2));
        assert_eq!(median(&[1, 2, 3, 4]), Some(2));
        assert_eq!(median(&[-1, -2]), Some(-2));
    }

    #[test]
    fn median_extremes_do_not_overflow() {
        assert_eq!(median(&[i64::MAX, i64::MAX]), Some(i64::MAX));
        assert_eq!(median(&[i64::MIN, i64::MAX]), Some(-1));
    }

    #[test]
    fn mad_values() {
        assert_eq!(mad(&[]), None);
        assert_eq!(mad(&[1, 1, 2, 2, 4, 6, 9]), Some(1));
        assert_eq!(mad(&[10, 10, 10]), Some(0));
        assert_eq!(mad(&[i64::MIN, i64::MAX, 0]), Some(i64::MAX.unsigned_abs()));
    }
}
