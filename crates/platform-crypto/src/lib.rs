//! `platform-crypto`: envelope encryption for secrets at rest (design doc
//! §12.3, §8.8).
//!
//! - Each household has a 256-bit data key (DEK). It encrypts that
//!   household's secret fields (Plaid access tokens, check-in notes) with
//!   AES-256-GCM.
//! - The key-encryption key (KEK, Secret Manager `token-kek`) wraps each DEK.
//!   The wrapped DEK is stored in `app.households.dek_wrapped`.
//! - Deleting a household's DEK makes its ciphertexts unreadable, also in
//!   backups (crypto-shredding, §12.5).
//!
//! Ciphertext format: `[version 1][nonce 12 bytes][ciphertext + 16-byte tag]`.
//! The associated data binds each ciphertext to its purpose and household,
//! so a ciphertext copied to another row or household does not decrypt.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use uuid::Uuid;
use zeroize::Zeroizing;

/// The ciphertext format version.
const VERSION: u8 = 1;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// How long a decrypted DEK stays in memory (design doc §12.3).
pub const DEK_CACHE_TTL: Duration = Duration::from_mins(5);

/// The environment variable that holds the KEK (32 bytes, base64).
pub const KEK_ENV: &str = "SAVESPACE_TOKEN_KEK";

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// The KEK is not 32 bytes of base64.
    #[error("the key-encryption key must be 32 bytes, base64")]
    BadKek,
    /// The ciphertext is not valid, or it belongs to another purpose or
    /// household, or the key is wrong.
    #[error("decryption failed")]
    Decrypt,
    /// A database error.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// What a ciphertext is for. Part of the associated data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A wrapped household DEK.
    DataKey,
    /// An aggregator access token.
    AccessToken,
    /// A check-in note.
    Note,
}

impl Purpose {
    fn label(self) -> &'static str {
        match self {
            Self::DataKey => "dek",
            Self::AccessToken => "access_token",
            Self::Note => "note",
        }
    }
}

fn aad(purpose: Purpose, household_id: Uuid) -> Vec<u8> {
    format!("savespace/v{VERSION}/{}/{household_id}", purpose.label()).into_bytes()
}

fn seal(cipher: &Aes256Gcm, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let mut nonce = [0_u8; NONCE_LEN];
    rand::fill(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .unwrap_or_default();
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    out.push(VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    out
}

fn open(cipher: &Aes256Gcm, aad: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if data.len() < 1 + NONCE_LEN + TAG_LEN || data[0] != VERSION {
        return Err(CryptoError::Decrypt);
    }
    let nonce: [u8; NONCE_LEN] = data[1..=NONCE_LEN]
        .try_into()
        .map_err(|_| CryptoError::Decrypt)?;
    cipher
        .decrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &data[1 + NONCE_LEN..],
                aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)
}

fn cipher(key: &[u8; 32]) -> Aes256Gcm {
    // A 32-byte slice is always a valid AES-256 key.
    Aes256Gcm::new_from_slice(key).unwrap_or_else(|_| unreachable!("32-byte key"))
}

/// The key-encryption key.
#[derive(Clone)]
pub struct Kek {
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for Kek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Kek([redacted])")
    }
}

impl Kek {
    /// Reads a KEK from base64 (standard alphabet, padding optional).
    ///
    /// # Errors
    ///
    /// [`CryptoError::BadKek`] if the value is not 32 bytes of base64.
    pub fn from_base64(value: &str) -> Result<Self, CryptoError> {
        let bytes = Zeroizing::new(
            STANDARD
                .decode(value.trim())
                .map_err(|_| CryptoError::BadKek)?,
        );
        let key: Zeroizing<[u8; 32]> = Zeroizing::new(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| CryptoError::BadKek)?,
        );
        Ok(Self {
            cipher: cipher(&key),
        })
    }

    /// Reads the KEK from `SAVESPACE_TOKEN_KEK`.
    ///
    /// # Errors
    ///
    /// [`CryptoError::BadKek`] if the variable is missing or not valid.
    pub fn from_env() -> Result<Self, CryptoError> {
        let value = Zeroizing::new(std::env::var(KEK_ENV).map_err(|_| CryptoError::BadKek)?);
        Self::from_base64(&value)
    }

    /// Wraps a DEK for storage.
    pub fn wrap(&self, dek: &Dek, household_id: Uuid) -> Vec<u8> {
        seal(
            &self.cipher,
            &aad(Purpose::DataKey, household_id),
            dek.key.as_slice(),
        )
    }

    /// Unwraps a stored DEK.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Decrypt`] if the value is not a DEK for this household
    /// under this KEK.
    pub fn unwrap(&self, wrapped: &[u8], household_id: Uuid) -> Result<Dek, CryptoError> {
        let bytes = Zeroizing::new(open(
            &self.cipher,
            &aad(Purpose::DataKey, household_id),
            wrapped,
        )?);
        let key: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::Decrypt)?;
        Ok(Dek::from_bytes(key))
    }
}

/// A household data key.
#[derive(Clone)]
pub struct Dek {
    key: Arc<Zeroizing<[u8; 32]>>,
    cipher: Aes256Gcm,
}

impl std::fmt::Debug for Dek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Dek([redacted])")
    }
}

impl Dek {
    /// A new random DEK.
    pub fn generate() -> Self {
        let mut key = [0_u8; 32];
        rand::fill(&mut key);
        Self::from_bytes(key)
    }

    fn from_bytes(key: [u8; 32]) -> Self {
        let key = Arc::new(Zeroizing::new(key));
        let cipher = cipher(&key);
        Self { key, cipher }
    }

    /// Encrypts a value for a household.
    pub fn seal(&self, purpose: Purpose, household_id: Uuid, plaintext: &[u8]) -> Vec<u8> {
        seal(&self.cipher, &aad(purpose, household_id), plaintext)
    }

    /// Decrypts a value.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Decrypt`] if the value was not sealed by this key for
    /// this purpose and household.
    pub fn open(
        &self,
        purpose: Purpose,
        household_id: Uuid,
        ciphertext: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        open(&self.cipher, &aad(purpose, household_id), ciphertext).map(Zeroizing::new)
    }
}

/// The KEK and a short-lived cache of household DEKs.
#[derive(Clone, Debug)]
pub struct Keyring {
    kek: Kek,
    cache: Arc<Mutex<HashMap<Uuid, (Instant, Dek)>>>,
}

impl Keyring {
    /// A keyring for a KEK.
    pub fn new(kek: Kek) -> Self {
        Self {
            kek,
            cache: Arc::default(),
        }
    }

    /// The household's DEK. Creates and stores one on first use.
    ///
    /// Use a connection inside `platform_db::tenant_tx` for the household, so
    /// row-level security applies.
    ///
    /// # Errors
    ///
    /// A database error, or [`CryptoError::Decrypt`] if the stored DEK does
    /// not unwrap with this KEK.
    pub async fn dek(
        &self,
        conn: &mut sqlx::PgConnection,
        household_id: Uuid,
    ) -> Result<Dek, CryptoError> {
        if let Some(dek) = self.cached(household_id) {
            return Ok(dek);
        }
        let stored: Option<Option<Vec<u8>>> =
            sqlx::query_scalar("SELECT dek_wrapped FROM app.households WHERE id = $1")
                .bind(household_id)
                .fetch_optional(&mut *conn)
                .await?;
        let dek = if let Some(wrapped) = stored.flatten() {
            self.kek.unwrap(&wrapped, household_id)?
        } else {
            let dek = Dek::generate();
            // Two first uses can race. Only one write wins. The loser reads
            // the winner's key.
            let written = sqlx::query(
                "UPDATE app.households SET dek_wrapped = $2 WHERE id = $1 AND dek_wrapped IS NULL",
            )
            .bind(household_id)
            .bind(self.kek.wrap(&dek, household_id))
            .execute(&mut *conn)
            .await?
            .rows_affected();
            if written == 1 {
                dek
            } else {
                let wrapped: Vec<u8> = sqlx::query_scalar(
                    "SELECT dek_wrapped FROM app.households WHERE id = $1 AND dek_wrapped IS NOT NULL",
                )
                .bind(household_id)
                .fetch_one(&mut *conn)
                .await?;
                self.kek.unwrap(&wrapped, household_id)?
            }
        };
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(household_id, (Instant::now(), dek.clone()));
        }
        Ok(dek)
    }

    fn cached(&self, household_id: Uuid) -> Option<Dek> {
        let mut cache = self.cache.lock().ok()?;
        cache.retain(|_, (at, _)| at.elapsed() < DEK_CACHE_TTL);
        cache.get(&household_id).map(|(_, dek)| dek.clone())
    }

    /// Removes a household's DEK from the cache (after deletion).
    pub fn forget(&self, household_id: Uuid) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(&household_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kek() -> Kek {
        Kek::from_base64(&STANDARD.encode([7_u8; 32])).unwrap_or_else(|_| unreachable!())
    }

    #[test]
    fn kek_must_be_32_bytes() {
        assert!(Kek::from_base64("").is_err());
        assert!(Kek::from_base64(&STANDARD.encode([1_u8; 16])).is_err());
        assert!(Kek::from_base64("not base64!").is_err());
        assert!(Kek::from_base64(&STANDARD.encode([1_u8; 32])).is_ok());
    }

    #[test]
    fn wrap_and_seal_round_trip() {
        let household = Uuid::now_v7();
        let dek = Dek::generate();
        let wrapped = kek().wrap(&dek, household);
        let dek2 = kek()
            .unwrap(&wrapped, household)
            .unwrap_or_else(|_| unreachable!());

        let sealed = dek.seal(Purpose::AccessToken, household, b"access-sandbox-123");
        assert_eq!(sealed[0], VERSION);
        let opened = dek2
            .open(Purpose::AccessToken, household, &sealed)
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(opened.as_slice(), b"access-sandbox-123");
    }

    #[test]
    fn ciphertexts_are_bound_to_purpose_household_and_key() {
        let household = Uuid::now_v7();
        let dek = Dek::generate();
        let sealed = dek.seal(Purpose::AccessToken, household, b"secret");

        assert!(dek.open(Purpose::Note, household, &sealed).is_err());
        assert!(
            dek.open(Purpose::AccessToken, Uuid::now_v7(), &sealed)
                .is_err()
        );
        assert!(
            Dek::generate()
                .open(Purpose::AccessToken, household, &sealed)
                .is_err()
        );
        assert!(
            kek()
                .unwrap(&kek().wrap(&dek, household), Uuid::now_v7())
                .is_err()
        );

        let mut tampered = sealed.clone();
        if let Some(last) = tampered.last_mut() {
            *last ^= 1;
        }
        assert!(
            dek.open(Purpose::AccessToken, household, &tampered)
                .is_err()
        );
        assert!(
            dek.open(Purpose::AccessToken, household, &sealed[..10])
                .is_err()
        );
    }

    #[test]
    fn nonces_differ() {
        let household = Uuid::now_v7();
        let dek = Dek::generate();
        let a = dek.seal(Purpose::Note, household, b"same");
        let b = dek.seal(Purpose::Note, household, b"same");
        assert_ne!(a, b);
    }

    #[test]
    fn debug_output_hides_keys() {
        assert_eq!(format!("{:?}", Dek::generate()), "Dek([redacted])");
        assert_eq!(format!("{:?}", kek()), "Kek([redacted])");
    }
}
