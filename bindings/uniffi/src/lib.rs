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
}

/// Formats an amount in minor units for display (en-US).
///
/// # Errors
///
/// Returns [`CoreError::UnsupportedCurrency`] for an unknown currency code.
#[uniffi::export]
pub fn format_money(minor: i64, currency: &str) -> Result<String, CoreError> {
    let currency: Currency = currency
        .parse()
        .map_err(|_| CoreError::UnsupportedCurrency {
            code: currency.to_owned(),
        })?;
    Ok(savespace_core::format_money(
        Money::from_minor(minor, currency),
        Locale::EnUs,
    ))
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
    }
}
