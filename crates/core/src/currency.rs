use std::fmt;
use std::str::FromStr;

/// An ISO 4217 currency that SaveSpace supports.
///
/// Add a currency only with its correct minor-unit exponent and a golden
/// format test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "UPPERCASE")
)]
pub enum Currency {
    /// United States dollar.
    Usd,
    /// Euro.
    Eur,
    /// Pound sterling.
    Gbp,
    /// Canadian dollar.
    Cad,
    /// Australian dollar.
    Aud,
    /// Japanese yen.
    Jpy,
}

impl Currency {
    /// All supported currencies.
    pub const ALL: [Currency; 6] = [
        Currency::Usd,
        Currency::Eur,
        Currency::Gbp,
        Currency::Cad,
        Currency::Aud,
        Currency::Jpy,
    ];

    /// The three-letter ISO 4217 code, for example `"USD"`.
    pub const fn code(self) -> &'static str {
        match self {
            Currency::Usd => "USD",
            Currency::Eur => "EUR",
            Currency::Gbp => "GBP",
            Currency::Cad => "CAD",
            Currency::Aud => "AUD",
            Currency::Jpy => "JPY",
        }
    }

    /// The number of decimal digits in the minor unit (2 for USD, 0 for JPY).
    pub const fn exponent(self) -> u8 {
        match self {
            Currency::Jpy => 0,
            _ => 2,
        }
    }

    /// The number of minor units in one major unit (100 for USD, 1 for JPY).
    pub const fn minor_per_major(self) -> u64 {
        10_u64.pow(self.exponent() as u32)
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// The error for an unknown or unsupported currency code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported currency code: {0:?}")]
pub struct ParseCurrencyError(pub String);

impl FromStr for Currency {
    type Err = ParseCurrencyError;

    /// Parses an uppercase ISO 4217 code. Lowercase input is rejected.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Currency::ALL
            .into_iter()
            .find(|c| c.code() == s)
            .ok_or_else(|| ParseCurrencyError(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trips() {
        for c in Currency::ALL {
            assert_eq!(c.code().parse::<Currency>(), Ok(c));
            assert_eq!(c.to_string(), c.code());
        }
    }

    #[test]
    fn rejects_unknown_and_lowercase() {
        assert!("XYZ".parse::<Currency>().is_err());
        assert!("usd".parse::<Currency>().is_err());
    }

    #[test]
    fn minor_units_per_major_unit() {
        assert_eq!(Currency::Usd.minor_per_major(), 100);
        assert_eq!(Currency::Jpy.minor_per_major(), 1);
    }
}
