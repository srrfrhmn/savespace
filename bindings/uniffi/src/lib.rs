//! `savespace-core-ffi`: the mobile apps' view of `savespace-core`.
//!
//! Keep this API identical to `bindings/wasm` (design doc §8.4). Phase 2
//! builds an XCFramework for iOS; Phase 3 builds an AAR for Android.

use savespace_core::{Currency, Locale, Money};

uniffi::setup_scaffolding!();

/// An error from the core bindings.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    /// The currency code is not a supported ISO 4217 code.
    #[error("unsupported currency code: {code}")]
    UnsupportedCurrency {
        /// The code that failed to parse.
        code: String,
    },
    /// The text is not an amount in this currency.
    #[error("not a valid amount: {reason}")]
    InvalidAmount {
        /// Why it failed.
        reason: String,
    },
}

fn currency(code: &str) -> Result<Currency, CoreError> {
    code.parse().map_err(|_| CoreError::UnsupportedCurrency {
        code: code.to_owned(),
    })
}

/// Formats an amount in minor units for display (en-US).
///
/// # Errors
///
/// Returns [`CoreError::UnsupportedCurrency`] for an unknown currency code.
#[uniffi::export]
pub fn format_money(minor: i64, currency: &str) -> Result<String, CoreError> {
    let currency = self::currency(currency)?;
    Ok(savespace_core::format_money(
        Money::from_minor(minor, currency),
        Locale::EnUs,
    ))
}

/// Parses an amount a person typed (en-US) into minor units.
///
/// # Errors
///
/// [`CoreError::UnsupportedCurrency`] or [`CoreError::InvalidAmount`].
#[uniffi::export]
pub fn parse_money(text: &str, currency: &str) -> Result<i64, CoreError> {
    let currency = self::currency(currency)?;
    savespace_core::parse_money_input(text, currency, Locale::EnUs)
        .map(Money::minor)
        .map_err(|e| CoreError::InvalidAmount {
            reason: e.to_string(),
        })
}

/// The calm-mode state: `covered`, `tight`, or `needs_attention`.
///
/// # Errors
///
/// [`CoreError::UnsupportedCurrency`], or [`CoreError::InvalidAmount`] on
/// overflow.
#[uniffi::export]
pub fn calm_state(
    cash: i64,
    bills: i64,
    flex_left: i64,
    currency: &str,
) -> Result<String, CoreError> {
    let currency = self::currency(currency)?;
    let m = |minor| Money::from_minor(minor, currency);
    savespace_core::budget::calm_state(m(cash), m(bills), m(flex_left))
        .map(|s| s.as_str().to_owned())
        .map_err(|e| CoreError::InvalidAmount {
            reason: e.to_string(),
        })
}

/// The version of `savespace-core` in this build.
#[uniffi::export]
pub fn core_version() -> String {
    savespace_core::VERSION.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_rejects() {
        assert_eq!(format_money(180_000, "USD").unwrap(), "$1,800");
        assert!(matches!(
            format_money(1, "XYZ"),
            Err(CoreError::UnsupportedCurrency { .. })
        ));
        assert_eq!(parse_money("$12.50", "USD").unwrap(), 1250);
        assert!(matches!(
            parse_money("1.005", "USD"),
            Err(CoreError::InvalidAmount { .. })
        ));
    }
}
