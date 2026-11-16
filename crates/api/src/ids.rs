//! Prefixed resource IDs (design doc §9.1, §11.1).
//!
//! The database uses `UUIDv7`. The API shows `prefix_` plus 32 hex digits, for
//! example `usr_0193a6c0e0b07c4d8f0e2a1b3c4d5e6f`. A wrong prefix is a 404, so
//! an ID of one type never finds a resource of another type.

use uuid::Uuid;

/// User.
pub const USER: &str = "usr";
/// Household.
pub const HOUSEHOLD: &str = "hh";
/// Passkey.
pub const PASSKEY: &str = "pk";
/// WebAuthn challenge.
pub const CHALLENGE: &str = "chl";
/// Bank connection.
pub const CONNECTION: &str = "conn";
/// Bank or card account.
pub const ACCOUNT: &str = "acct";
/// Transaction.
pub const TRANSACTION: &str = "txn";
/// Category.
pub const CATEGORY: &str = "cat";
/// Merchant.
pub const MERCHANT: &str = "mer";
/// Split part.
pub const SPLIT: &str = "spl";
/// Categorization rule.
pub const RULE: &str = "rule";
/// Recurring series.
pub const RECURRING: &str = "rec";

/// Formats an ID with its prefix.
pub fn encode(prefix: &str, id: Uuid) -> String {
    format!("{prefix}_{}", id.simple())
}

/// Parses a prefixed ID. Returns `None` for a wrong prefix or a bad UUID.
pub fn decode(prefix: &str, s: &str) -> Option<Uuid> {
    let rest = s.strip_prefix(prefix)?.strip_prefix('_')?;
    if rest.len() != 32 {
        return None;
    }
    Uuid::try_parse(rest).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let id = Uuid::now_v7();
        let s = encode(PASSKEY, id);
        assert!(s.starts_with("pk_"));
        assert_eq!(decode(PASSKEY, &s), Some(id));
    }

    #[test]
    fn rejects_wrong_prefix_and_bad_ids() {
        let id = Uuid::now_v7();
        assert_eq!(decode(USER, &encode(PASSKEY, id)), None);
        assert_eq!(decode(PASSKEY, "pk_"), None);
        assert_eq!(decode(PASSKEY, &format!("pk_{id}")), None);
        assert_eq!(
            decode(PASSKEY, "pkx_00000000000000000000000000000000"),
            None
        );
    }
}
