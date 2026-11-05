//! Golden-file tests. The web app runs the same file through WASM
//! (`apps/web/src/core.test.ts`). Both MUST produce the same strings.

use savespace_core::{Currency, Locale, Money, format_money};
use serde::Deserialize;

#[derive(Deserialize)]
struct GoldenFile {
    cases: Vec<Case>,
}

/// `minor` is a string because JSON numbers lose precision above 2^53 in
/// JavaScript, and the web test reads the same file.
#[derive(Deserialize)]
struct Case {
    minor: String,
    currency: String,
    expected: String,
}

#[test]
fn format_money_matches_golden_file() {
    let raw = include_str!("golden/format.json");
    let golden: GoldenFile = serde_json::from_str(raw).expect("valid golden file");
    assert!(!golden.cases.is_empty());
    for case in golden.cases {
        let minor: i64 = case.minor.parse().expect("minor is an i64");
        let currency: Currency = case.currency.parse().expect("supported currency");
        let actual = format_money(Money::from_minor(minor, currency), Locale::EnUs);
        assert_eq!(actual, case.expected, "minor={minor} currency={currency}");
    }
}
