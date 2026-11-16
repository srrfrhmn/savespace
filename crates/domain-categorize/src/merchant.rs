//! Merchant normalization (design doc §10.2 Enrich).
//!
//! Bank descriptions carry payment processor prefixes, store numbers, and
//! reference codes: `SQ *BLUE BOTTLE #123 OAKLAND CA`. The key removes them,
//! so all purchases at one merchant share one key. Rules match on the key.
//!
//! The provider sends a merchant name for some transactions only. A cleaned
//! description that starts with a provider merchant name joins that merchant
//! ([`known_prefix`]): `UBER 063015 SF**POOL**` becomes `uber`, not `uber sf`.

/// A normalized merchant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merchant {
    /// The match key: lowercase, no processor prefix or store number.
    pub key: String,
    /// The name to show.
    pub name: String,
    /// True if the key comes from the provider's merchant name, not from the
    /// description.
    pub from_provider: bool,
}

/// The shortest provider key that can claim a description.
const MIN_PREFIX: usize = 3;

/// The provider merchant key that a description key starts with, at a word
/// boundary. The longest match wins: it is the most specific.
pub fn known_prefix<'a>(key: &str, known: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    known
        .into_iter()
        .filter(|k| {
            k.chars().count() >= MIN_PREFIX
                && (key == *k
                    || key
                        .strip_prefix(*k)
                        .is_some_and(|rest| rest.starts_with(' ')))
        })
        .max_by_key(|k| k.len())
}

/// The longest key, in characters (the database checks 200).
const MAX_KEY: usize = 120;

/// Prefixes that payment processors and banks add. Lowercase.
const PREFIXES: &[&str] = &[
    "sq *",
    "sq*",
    "tst* ",
    "tst*",
    "sp * ",
    "sp *",
    "paypal *",
    "pp*",
    "py *",
    "pos debit ",
    "pos ",
    "debit card purchase ",
    "purchase authorized on ",
    "checkcard ",
    "recurring ",
];

/// US state codes, removed from the end of a description.
const STATES: &[&str] = &[
    "al", "ak", "az", "ar", "ca", "co", "ct", "de", "dc", "fl", "ga", "hi", "id", "il", "in", "ia",
    "ks", "ky", "la", "me", "md", "ma", "mi", "mn", "ms", "mo", "mt", "ne", "nv", "nh", "nj", "nm",
    "ny", "nc", "nd", "oh", "ok", "or", "pa", "ri", "sc", "sd", "tn", "tx", "ut", "vt", "va", "wa",
    "wv", "wi", "wy",
];

/// Finds the merchant of a transaction. The provider's merchant name wins
/// when it has one. Returns `None` if nothing is left after cleaning.
pub fn normalize_merchant(description: &str, merchant_name: Option<&str>) -> Option<Merchant> {
    if let Some(name) = merchant_name.map(str::trim).filter(|n| !n.is_empty()) {
        let key = key_of(name);
        if !key.is_empty() {
            return Some(Merchant {
                key,
                name: truncate(&collapse(name), MAX_KEY),
                from_provider: true,
            });
        }
    }
    let cleaned = clean(description);
    if cleaned.is_empty() {
        return None;
    }
    Some(Merchant {
        name: title_case(&cleaned),
        key: cleaned,
        from_provider: false,
    })
}

/// Lowercase, single spaces, at most [`MAX_KEY`] characters.
fn key_of(name: &str) -> String {
    truncate(&collapse(&name.to_lowercase()), MAX_KEY)
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(s: &str, max: usize) -> String {
    s.chars()
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// The cleaned, lowercase description.
fn clean(description: &str) -> String {
    let mut s = collapse(&description.to_lowercase());
    // Prefixes can stack: "pos debit sq *coffee".
    loop {
        let before = s.len();
        for prefix in PREFIXES {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest.trim_start().to_owned();
            }
        }
        if s.len() == before {
            break;
        }
    }
    // "amazon.com*2k3l4m" and "netflix *ref123": the code follows the star.
    if let Some((head, _)) = s.split_once('*')
        && !head.trim().is_empty()
    {
        s = head.trim().to_owned();
    }
    let mut words: Vec<&str> = s
        .split_whitespace()
        .filter(|w| {
            let digits = w.chars().filter(char::is_ascii_digit).count();
            !w.starts_with('#') && digits < 3 && w.chars().any(char::is_alphanumeric)
        })
        .collect();
    // "coffee shop oakland ca": drop the state when other words stay.
    if words.len() > 2 && words.last().is_some_and(|w| STATES.contains(w)) {
        words.pop();
    }
    let joined = words.join(" ");
    let trimmed = joined.trim_matches(|c: char| !c.is_alphanumeric() && c != ')' && c != '\'');
    truncate(trimmed, MAX_KEY)
}

/// "blue bottle coffee" to "Blue Bottle Coffee".
fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(description: &str) -> Option<String> {
        normalize_merchant(description, None).map(|m| m.key)
    }

    #[test]
    fn provider_name_wins() {
        let m = normalize_merchant("SQ *BLUE BOTTLE #123", Some("  Blue   Bottle ")).unwrap();
        assert_eq!(m.key, "blue bottle");
        assert_eq!(m.name, "Blue Bottle");
        // A blank provider name falls back to the description.
        let m = normalize_merchant("TRADER JOE S #552 OAKLAND CA", Some(" ")).unwrap();
        assert_eq!(m.key, "trader joe s oakland");
        assert_eq!(m.name, "Trader Joe S Oakland");
    }

    #[test]
    fn removes_prefixes_codes_and_store_numbers() {
        assert_eq!(key("SQ *BLUE BOTTLE #123"), Some("blue bottle".into()));
        assert_eq!(key("TST* SWEETGREEN 0042"), Some("sweetgreen".into()));
        assert_eq!(
            key("AMAZON.COM*2K3L4M AMZN.COM/BILL WA"),
            Some("amazon.com".into())
        );
        assert_eq!(key("POS DEBIT PP*SPOTIFY"), Some("spotify".into()));
        assert_eq!(key("Uber 063015 SF**POOL**"), Some("uber sf".into()));
        assert_eq!(
            key("STARBUCKS STORE 12345 SEATTLE WA"),
            Some("starbucks store seattle".into())
        );
        assert_eq!(
            key("Touchstone Climbing"),
            Some("touchstone climbing".into())
        );
    }

    #[test]
    fn same_merchant_same_key() {
        assert_eq!(key("SQ *BLUE BOTTLE #123"), key("sq*Blue Bottle #77"));
    }

    #[test]
    fn a_description_joins_a_known_provider_merchant() {
        let known = ["uber", "uber eats", "ub", "blue bottle"];
        assert_eq!(known_prefix("uber sf", known), Some("uber"));
        assert_eq!(known_prefix("uber eats sf", known), Some("uber eats"));
        assert_eq!(known_prefix("uber", known), Some("uber"));
        // Word boundary: "uberx" is another word. Short keys never claim.
        assert_eq!(known_prefix("uberx sf", known), None);
        assert_eq!(known_prefix("ub sf", known), None);
        assert_eq!(known_prefix("bottle shop", known), None);
    }

    #[test]
    fn nothing_left_is_none() {
        assert_eq!(key("#12345 0000"), None);
        assert_eq!(key("   "), None);
        assert_eq!(normalize_merchant("", Some("")), None);
    }

    #[test]
    fn long_text_is_truncated() {
        let long = "a".repeat(500);
        assert_eq!(key(&long).unwrap().chars().count(), MAX_KEY);
        assert_eq!(
            normalize_merchant("x", Some(&long))
                .unwrap()
                .name
                .chars()
                .count(),
            MAX_KEY
        );
    }
}
