//! Plaid webhook verification and handling (design doc §10.2).
//!
//! Each webhook has a `Plaid-Verification` header: a JWT signed with ES256.
//! Verification follows Plaid's guide:
//!
//! 1. The header algorithm must be ES256.
//! 2. The key comes from `/webhook_verification_key/get` by `kid`, and is
//!    cached. A key with `expired_at` set is rejected.
//! 3. The signature must be valid.
//! 4. `iat` must be at most 5 minutes old (replay protection).
//! 5. `request_body_sha256` must equal the SHA-256 of the raw body
//!    (constant-time compare).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use domain_sync::{AggregatorError, ConnectionStatus, WebhookAction};
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::PlaidClient;

/// The longest age of a webhook (seconds).
const MAX_AGE_SECS: i64 = 5 * 60;

/// Why a webhook was rejected.
#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    /// The header is not a well-formed JWT.
    #[error("malformed verification header")]
    Malformed,
    /// The algorithm is not ES256.
    #[error("unexpected JWT algorithm")]
    Algorithm,
    /// The key is unknown or expired.
    #[error("unknown or expired verification key")]
    Key,
    /// The signature is not valid.
    #[error("invalid signature")]
    Signature,
    /// The webhook is too old, or from the future.
    #[error("webhook too old")]
    Stale,
    /// The body does not match the signed hash.
    #[error("body hash mismatch")]
    Body,
    /// The key could not be fetched. Plaid retries the webhook.
    #[error(transparent)]
    Fetch(#[from] AggregatorError),
}

/// Verification keys by `kid`.
#[derive(Debug, Clone, Default)]
pub(crate) struct KeyCache(Arc<Mutex<HashMap<String, VerifyingKey>>>);

#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    kid: String,
}

#[derive(Debug, Deserialize)]
struct Claims {
    iat: i64,
    request_body_sha256: String,
}

#[derive(Debug, Deserialize)]
struct KeyResponse {
    key: Jwk,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    crv: String,
    kty: String,
    x: String,
    y: String,
    expired_at: Option<Value>,
}

fn decode<T: serde::de::DeserializeOwned>(part: &str) -> Result<T, WebhookError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| WebhookError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| WebhookError::Malformed)
}

/// Builds a P-256 key from JWK coordinates.
fn key_from_jwk(jwk: &Jwk) -> Result<VerifyingKey, WebhookError> {
    if jwk.kty != "EC"
        || jwk.crv != "P-256"
        || jwk.expired_at.as_ref().is_some_and(|v| !v.is_null())
    {
        return Err(WebhookError::Key);
    }
    let x = URL_SAFE_NO_PAD
        .decode(&jwk.x)
        .map_err(|_| WebhookError::Key)?;
    let y = URL_SAFE_NO_PAD
        .decode(&jwk.y)
        .map_err(|_| WebhookError::Key)?;
    if x.len() != 32 || y.len() != 32 {
        return Err(WebhookError::Key);
    }
    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04);
    sec1.extend_from_slice(&x);
    sec1.extend_from_slice(&y);
    VerifyingKey::from_sec1_bytes(&sec1).map_err(|_| WebhookError::Key)
}

impl PlaidClient {
    async fn verification_key(&self, kid: &str) -> Result<VerifyingKey, WebhookError> {
        if let Some(key) = self.keys.0.lock().ok().and_then(|k| k.get(kid).copied()) {
            return Ok(key);
        }
        let response: KeyResponse = self
            .call("/webhook_verification_key/get", &json!({ "key_id": kid }))
            .await
            .map_err(|e| match e {
                AggregatorError::Provider { .. } => WebhookError::Key,
                other => WebhookError::Fetch(other),
            })?;
        let key = key_from_jwk(&response.key)?;
        if let Ok(mut keys) = self.keys.0.lock() {
            keys.insert(kid.to_owned(), key);
        }
        Ok(key)
    }

    /// Verifies a webhook. `now` is the current Unix time in seconds.
    ///
    /// # Errors
    ///
    /// [`WebhookError`] if any check fails.
    pub async fn verify_webhook(
        &self,
        verification_header: &str,
        body: &[u8],
        now: i64,
    ) -> Result<(), WebhookError> {
        let mut parts = verification_header.split('.');
        let (Some(h), Some(p), Some(s), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(WebhookError::Malformed);
        };
        let header: Header = decode(h)?;
        if header.alg != "ES256" {
            return Err(WebhookError::Algorithm);
        }
        let signature = URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| WebhookError::Malformed)?;
        let signature = Signature::from_slice(&signature).map_err(|_| WebhookError::Signature)?;
        let key = self.verification_key(&header.kid).await?;
        let signed = &verification_header[..h.len() + 1 + p.len()];
        key.verify(signed.as_bytes(), &signature)
            .map_err(|_| WebhookError::Signature)?;

        let claims: Claims = decode(p)?;
        if now - claims.iat > MAX_AGE_SECS || claims.iat - now > MAX_AGE_SECS {
            return Err(WebhookError::Stale);
        }
        let actual = hex::encode(Sha256::digest(body));
        if !bool::from(
            actual
                .as_bytes()
                .ct_eq(claims.request_body_sha256.to_ascii_lowercase().as_bytes()),
        ) {
            return Err(WebhookError::Body);
        }
        Ok(())
    }
}

/// Decides what a Plaid webhook means for the connection.
pub fn webhook_action(webhook_type: &str, webhook_code: &str, body: &Value) -> WebhookAction {
    let set = |status, error_code: Option<String>| WebhookAction::SetStatus { status, error_code };
    match (webhook_type, webhook_code) {
        ("TRANSACTIONS", "SYNC_UPDATES_AVAILABLE") | ("ITEM", "LOGIN_REPAIRED") => {
            WebhookAction::Sync
        }
        ("ITEM", "ERROR") => {
            let code = body
                .pointer("/error/error_code")
                .and_then(Value::as_str)
                .map(str::to_owned);
            match code.as_deref() {
                Some("ITEM_LOGIN_REQUIRED") => set(ConnectionStatus::LoginRequired, code),
                _ => set(ConnectionStatus::Error, code),
            }
        }
        ("ITEM", "PENDING_EXPIRATION" | "PENDING_DISCONNECT") => {
            set(ConnectionStatus::PendingExpiration, None)
        }
        ("ITEM", "USER_PERMISSION_REVOKED" | "USER_ACCOUNT_REVOKED") => {
            set(ConnectionStatus::Revoked, Some(webhook_code.to_owned()))
        }
        _ => WebhookAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_webhooks_to_actions() {
        let none = json!({});
        assert_eq!(
            webhook_action("TRANSACTIONS", "SYNC_UPDATES_AVAILABLE", &none),
            WebhookAction::Sync
        );
        assert_eq!(
            webhook_action(
                "ITEM",
                "ERROR",
                &json!({ "error": { "error_code": "ITEM_LOGIN_REQUIRED" } })
            ),
            WebhookAction::SetStatus {
                status: ConnectionStatus::LoginRequired,
                error_code: Some("ITEM_LOGIN_REQUIRED".into())
            }
        );
        assert_eq!(
            webhook_action("ITEM", "PENDING_EXPIRATION", &none),
            WebhookAction::SetStatus {
                status: ConnectionStatus::PendingExpiration,
                error_code: None
            }
        );
        assert!(matches!(
            webhook_action("ITEM", "USER_PERMISSION_REVOKED", &none),
            WebhookAction::SetStatus {
                status: ConnectionStatus::Revoked,
                ..
            }
        ));
        assert_eq!(
            webhook_action("TRANSACTIONS", "DEFAULT_UPDATE", &none),
            WebhookAction::Ignore
        );
        assert_eq!(webhook_action("AUTH", "X", &none), WebhookAction::Ignore);
    }
}
