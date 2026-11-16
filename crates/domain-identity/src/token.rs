//! Random secrets and their hashes: session tokens and recovery codes.
//!
//! Both have at least 60 bits of entropy from a CSPRNG, so a fast hash
//! (SHA-256) is safe for storage. A slow password hash is not needed.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// The number of recovery codes in one set (D12).
pub const RECOVERY_CODE_COUNT: usize = 10;

/// Crockford base32 without I, L, O, U. Easy to read and to type.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A new session token: 32 random bytes, base64url without padding.
pub fn new_session_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The SHA-256 hash of a secret. This is the stored value.
pub fn hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

/// A new recovery code: 12 Crockford base32 characters (60 bits) in groups
/// of four, for example `7KQ2-M9XD-4TPA`.
pub fn new_recovery_code() -> String {
    let mut bytes = [0_u8; 12];
    rand::fill(&mut bytes);
    let chars: Vec<char> = bytes
        .iter()
        .map(|b| char::from(CROCKFORD[usize::from(b & 31)]))
        .collect();
    chars
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

/// Normalizes a recovery code that a person typed: removes spaces and
/// dashes, uses upper case, and maps the letters that Crockford base32
/// excludes to the digits they look like (O to 0, I and L to 1).
///
/// Returns `None` if the result is not 12 valid characters.
pub fn normalize_recovery_code(input: &str) -> Option<String> {
    let mut out = String::with_capacity(12);
    for c in input.chars() {
        let c = match c.to_ascii_uppercase() {
            ' ' | '-' => continue,
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        if !c.is_ascii() || !CROCKFORD.contains(&(c as u8)) {
            return None;
        }
        out.push(c);
    }
    (out.len() == 12).then_some(out)
}

/// The stored hash of a recovery code. Use the normalized form so that
/// `7kq2 m9xd 4tpa` and `7KQ2-M9XD-4TPA` match.
pub fn hash_recovery_code(normalized: &str) -> Vec<u8> {
    hash(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_tokens_are_unique_and_long() {
        let a = new_session_token();
        let b = new_session_token();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert_eq!(hash(&a).len(), 32);
    }

    #[test]
    fn recovery_codes_round_trip_through_normalize() {
        for _ in 0..100 {
            let code = new_recovery_code();
            assert_eq!(code.len(), 14, "{code}");
            let normalized = normalize_recovery_code(&code).unwrap_or_default();
            assert_eq!(normalized, code.replace('-', ""));
            let typed = code.to_lowercase().replace('-', " ");
            assert_eq!(normalize_recovery_code(&typed), Some(normalized));
        }
    }

    #[test]
    fn normalize_maps_look_alike_letters() {
        assert_eq!(
            normalize_recovery_code("o0il-1234-abcd").as_deref(),
            Some("00111234ABCD")
        );
    }

    #[test]
    fn normalize_rejects_bad_input() {
        assert_eq!(normalize_recovery_code(""), None);
        assert_eq!(normalize_recovery_code("ABCD-EFGH"), None);
        assert_eq!(normalize_recovery_code("ABCD-EFGH-JKMN-P"), None);
        assert_eq!(normalize_recovery_code("ABCD-EFGH-JKMU"), None);
        assert_eq!(normalize_recovery_code("ABCD-EFGH-JKM€"), None);
    }
}
