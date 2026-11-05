//! `savespace-core-wasm`: the web app's view of `savespace-core`.
//!
//! Keep this API identical to `bindings/uniffi` (design doc §8.4). Build with
//! `pnpm wasm` (wasm-pack, `--target web`).

use savespace_core::{Currency, Locale, Money};
use wasm_bindgen::prelude::*;

/// Formats an amount in minor units for display (en-US).
///
/// `minor` is a JavaScript `BigInt`, so amounts above 2^53 stay exact.
///
/// # Errors
///
/// Returns an error if `currency` is not a supported ISO 4217 code.
#[wasm_bindgen(js_name = formatMoney)]
pub fn format_money(minor: i64, currency: &str) -> Result<String, JsError> {
    let currency: Currency = currency.parse()?;
    Ok(savespace_core::format_money(
        Money::from_minor(minor, currency),
        Locale::EnUs,
    ))
}

/// The version of `savespace-core` in this build.
#[wasm_bindgen(js_name = coreVersion)]
pub fn core_version() -> String {
    savespace_core::VERSION.to_owned()
}
