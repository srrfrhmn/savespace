//! `savespace-core`: the single source of truth for money math in SaveSpace.
//!
//! The server links this crate natively. The web app uses it through WASM
//! (`bindings/wasm`). The iOS and Android apps use it through UniFFI
//! (`bindings/uniffi`). See design doc §8.4.
//!
//! Rules for this crate:
//! - No I/O, no clock access, no randomness, no global state.
//! - Money is an `i64` count of minor units plus a [`Currency`].
//! - No floating-point arithmetic. Ratios use [`BasisPoints`].
#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

mod currency;
mod format;
mod money;
mod ratio;
pub mod stats;

pub use currency::{Currency, ParseCurrencyError};
pub use format::{Locale, format_money};
pub use money::{AllocationError, Money, MoneyError};
pub use ratio::BasisPoints;

/// The version of this crate. Clients show it in diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
