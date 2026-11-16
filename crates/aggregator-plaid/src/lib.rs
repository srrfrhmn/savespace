//! `aggregator-plaid`: the Plaid adapter (design doc §8.1, §10.1, §10.2).
//!
//! A small hand-written client for the endpoints SaveSpace uses. It
//! implements [`domain_sync::Aggregator`] and verifies Plaid webhooks.
//!
//! - Auth: `PLAID-CLIENT-ID` and `PLAID-SECRET` headers. The secret never
//!   appears in logs or errors.
//! - Amounts: Plaid's positive amount is money out. This adapter negates it,
//!   so SaveSpace amounts are negative for money out. Amounts are parsed
//!   from the JSON text with `savespace_core::parse_decimal`, never through
//!   floating-point arithmetic.

mod model;
pub mod webhook;

use std::time::Duration;

use async_trait::async_trait;
use domain_sync::{Aggregator, AggregatorError, LinkToken, LinkTokenRequest, NewItem, SyncPage};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub use webhook::{WebhookError, webhook_action};

/// The Plaid environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaidEnv {
    /// Test data. Free.
    Sandbox,
    /// Real data (the Trial plan in Phase 1).
    Production,
}

impl PlaidEnv {
    /// Parses `sandbox` or `production`.
    ///
    /// # Errors
    ///
    /// Returns the input if it is neither.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "sandbox" => Ok(Self::Sandbox),
            "production" => Ok(Self::Production),
            other => Err(other.to_owned()),
        }
    }

    /// The API base URL.
    pub fn base_url(self) -> &'static str {
        match self {
            Self::Sandbox => "https://sandbox.plaid.com",
            Self::Production => "https://production.plaid.com",
        }
    }
}

/// Plaid credentials and the environment.
#[derive(Clone)]
pub struct PlaidConfig {
    /// The environment.
    pub env: PlaidEnv,
    /// `PLAID_CLIENT_ID`.
    pub client_id: String,
    /// `PLAID_SECRET`.
    pub secret: String,
    /// Overrides the base URL (tests).
    pub base_url: Option<String>,
}

impl std::fmt::Debug for PlaidConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaidConfig")
            .field("env", &self.env)
            .field("client_id", &self.client_id)
            .field("secret", &"[redacted]")
            .finish_non_exhaustive()
    }
}

impl PlaidConfig {
    /// Reads `PLAID_ENV`, `PLAID_CLIENT_ID`, and `PLAID_SECRET`. Returns
    /// `None` if the client ID or the secret is not set.
    ///
    /// # Errors
    ///
    /// Returns a message if `PLAID_ENV` is not `sandbox` or `production`.
    pub fn from_env() -> Result<Option<Self>, String> {
        let env = std::env::var("PLAID_ENV").unwrap_or_else(|_| "sandbox".into());
        let env = PlaidEnv::parse(&env).map_err(|v| format!("PLAID_ENV={v:?} is not valid"))?;
        let client_id = std::env::var("PLAID_CLIENT_ID").unwrap_or_default();
        let secret = std::env::var("PLAID_SECRET").unwrap_or_default();
        if client_id.trim().is_empty() || secret.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            env,
            client_id: client_id.trim().to_owned(),
            secret: secret.trim().to_owned(),
            base_url: None,
        }))
    }
}

/// Builds the sync service with Plaid from the environment: `PLAID_*` and
/// `SAVESPACE_TOKEN_KEK`. Returns `None` if Plaid is not configured.
///
/// # Errors
///
/// Returns an error if `PLAID_ENV` or the key-encryption key is not valid,
/// or the HTTP client cannot start.
pub fn sync_service_from_env(
    pool: platform_db::PgPool,
) -> Result<Option<(domain_sync::SyncService, PlaidClient)>, String> {
    let Some(config) = PlaidConfig::from_env()? else {
        return Ok(None);
    };
    let kek = platform_crypto::Kek::from_env()
        .map_err(|e| format!("{}: {e}", platform_crypto::KEK_ENV))?;
    let plaid = PlaidClient::new(config).map_err(|e| e.to_string())?;
    let sync = domain_sync::SyncService::new(
        pool,
        platform_crypto::Keyring::new(kek),
        std::sync::Arc::new(plaid.clone()),
    );
    Ok(Some((sync, plaid)))
}

/// The Plaid API client.
#[derive(Clone, Debug)]
pub struct PlaidClient {
    http: reqwest::Client,
    config: PlaidConfig,
    keys: webhook::KeyCache,
}

/// The Plaid API version this client speaks.
const API_VERSION: &str = "2020-09-14";
/// Transactions per sync page (Plaid maximum: 500).
const SYNC_PAGE_SIZE: u32 = 500;
/// History requested on the first link (Plaid maximum: 730 days).
const DAYS_REQUESTED: u32 = 730;

impl PlaidClient {
    /// Creates a client.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot start (TLS setup).
    pub fn new(config: PlaidConfig) -> Result<Self, AggregatorError> {
        // reqwest uses rustls without a built-in provider. Use ring, like sqlx.
        // A second install (another client, a test) is not an error.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .user_agent("savespace")
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| AggregatorError::Transport(e.to_string()))?;
        Ok(Self {
            http,
            config,
            keys: webhook::KeyCache::default(),
        })
    }

    /// The environment.
    pub fn env(&self) -> PlaidEnv {
        self.config.env
    }

    fn url(&self, path: &str) -> String {
        let base = self
            .config
            .base_url
            .as_deref()
            .unwrap_or(self.config.env.base_url());
        format!("{base}{path}")
    }

    /// Calls a Plaid endpoint and decodes the response.
    pub(crate) async fn call<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, AggregatorError> {
        let response = self
            .http
            .post(self.url(path))
            .header("PLAID-CLIENT-ID", &self.config.client_id)
            .header("PLAID-SECRET", &self.config.secret)
            .header("Plaid-Version", API_VERSION)
            .json(body)
            .send()
            .await
            .map_err(|e| AggregatorError::Transport(without_url(&e)))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| AggregatorError::Transport(without_url(&e)))?;
        if status.is_success() {
            return serde_json::from_slice(&bytes)
                .map_err(|e| AggregatorError::Invalid(format!("{path}: {e}")));
        }
        let error: model::PlaidError = serde_json::from_slice(&bytes).unwrap_or_default();
        tracing::info!(
            path,
            status = status.as_u16(),
            error_type = %error.error_type,
            error_code = %error.error_code,
            request_id = %error.request_id,
            "plaid error"
        );
        Err(map_error(status.as_u16(), error))
    }

    /// Sandbox only: creates a public token without the Link UI. Tests and
    /// the local end-to-end check use it.
    ///
    /// # Errors
    ///
    /// An aggregator error.
    pub async fn sandbox_public_token(
        &self,
        institution_id: &str,
        webhook_url: Option<&str>,
    ) -> Result<String, AggregatorError> {
        let mut options = json!({});
        if let Some(url) = webhook_url {
            options["webhook"] = Value::from(url);
        }
        let response: model::PublicTokenResponse = self
            .call(
                "/sandbox/public_token/create",
                &json!({
                    "institution_id": institution_id,
                    "initial_products": ["transactions"],
                    "options": options,
                }),
            )
            .await?;
        Ok(response.public_token)
    }

    /// Sandbox only: asks Plaid to send a webhook for an item.
    ///
    /// # Errors
    ///
    /// An aggregator error.
    pub async fn sandbox_fire_webhook(
        &self,
        access_token: &str,
        webhook_code: &str,
    ) -> Result<(), AggregatorError> {
        let _: Value = self
            .call(
                "/sandbox/item/fire_webhook",
                &json!({ "access_token": access_token, "webhook_code": webhook_code }),
            )
            .await?;
        Ok(())
    }
}

/// A reqwest error without the URL (the URL has no secrets, but keep logs
/// short and stable).
fn without_url(e: &reqwest::Error) -> String {
    let mut message = e.to_string();
    if let Some(url) = e.url() {
        message = message.replace(url.as_str(), "[url]");
    }
    message
}

fn map_error(status: u16, error: model::PlaidError) -> AggregatorError {
    let code = error.error_code;
    match (error.error_type.as_str(), code.as_str()) {
        (_, "TRANSACTIONS_SYNC_MUTATION_DURING_PAGINATION") => {
            AggregatorError::MutationDuringPagination
        }
        (_, "ITEM_LOGIN_REQUIRED" | "PENDING_EXPIRATION" | "INVALID_UPDATED_USERNAME") => {
            AggregatorError::LoginRequired { code }
        }
        (
            _,
            "ITEM_NOT_FOUND"
            | "ACCESS_NOT_GRANTED"
            | "USER_PERMISSION_REVOKED"
            | "INVALID_ACCESS_TOKEN",
        ) => AggregatorError::ItemGone { code },
        ("RATE_LIMIT_EXCEEDED", _) => AggregatorError::RateLimited,
        _ if status == 429 => AggregatorError::RateLimited,
        _ if status >= 500 || code.is_empty() => {
            AggregatorError::Transport(format!("plaid status {status} {code}"))
        }
        _ => AggregatorError::Provider {
            code,
            message: error.error_message,
        },
    }
}

#[async_trait]
impl Aggregator for PlaidClient {
    fn provider(&self) -> &'static str {
        "plaid"
    }

    async fn create_link_token(
        &self,
        request: &LinkTokenRequest,
    ) -> Result<LinkToken, AggregatorError> {
        let mut body = json!({
            "client_name": "SaveSpace",
            "language": "en",
            "country_codes": ["US"],
            "user": { "client_user_id": request.client_user_id },
        });
        if let Some(token) = &request.update_access_token {
            // Update mode: no products.
            body["access_token"] = Value::from(token.as_str());
        } else {
            body["products"] = json!(["transactions"]);
            body["transactions"] = json!({ "days_requested": DAYS_REQUESTED });
        }
        if let Some(url) = &request.webhook_url {
            body["webhook"] = Value::from(url.as_str());
        }
        let response: model::LinkTokenResponse = self.call("/link/token/create", &body).await?;
        Ok(LinkToken {
            link_token: response.link_token,
            expiration: response.expiration,
        })
    }

    async fn exchange_public_token(&self, public_token: &str) -> Result<NewItem, AggregatorError> {
        let response: model::ExchangeResponse = self
            .call(
                "/item/public_token/exchange",
                &json!({ "public_token": public_token }),
            )
            .await?;
        Ok(NewItem {
            item_id: response.item_id,
            access_token: response.access_token,
        })
    }

    async fn sync_page(
        &self,
        access_token: &str,
        cursor: Option<&str>,
    ) -> Result<SyncPage, AggregatorError> {
        let mut body = json!({
            "access_token": access_token,
            "count": SYNC_PAGE_SIZE,
            "options": { "include_original_description": false },
        });
        if let Some(cursor) = cursor {
            body["cursor"] = Value::from(cursor);
        }
        let response: model::SyncResponse = self.call("/transactions/sync", &body).await?;
        response.into_page()
    }

    async fn remove_item(&self, access_token: &str) -> Result<(), AggregatorError> {
        let _: Value = self
            .call("/item/remove", &json!({ "access_token": access_token }))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(error_type: &str, code: &str) -> model::PlaidError {
        model::PlaidError {
            error_type: error_type.into(),
            error_code: code.into(),
            error_message: "message".into(),
            request_id: "req".into(),
        }
    }

    #[test]
    fn maps_plaid_errors() {
        assert_eq!(
            map_error(
                400,
                error(
                    "TRANSACTIONS_ERROR",
                    "TRANSACTIONS_SYNC_MUTATION_DURING_PAGINATION"
                )
            ),
            AggregatorError::MutationDuringPagination
        );
        assert!(matches!(
            map_error(400, error("ITEM_ERROR", "ITEM_LOGIN_REQUIRED")),
            AggregatorError::LoginRequired { .. }
        ));
        assert!(matches!(
            map_error(400, error("ITEM_ERROR", "ITEM_NOT_FOUND")),
            AggregatorError::ItemGone { .. }
        ));
        assert_eq!(
            map_error(429, error("RATE_LIMIT_EXCEEDED", "TRANSACTIONS_LIMIT")),
            AggregatorError::RateLimited
        );
        assert!(matches!(
            map_error(500, error("API_ERROR", "INTERNAL_SERVER_ERROR")),
            AggregatorError::Transport(_)
        ));
        assert!(matches!(
            map_error(400, error("INVALID_REQUEST", "MISSING_FIELDS")),
            AggregatorError::Provider { .. }
        ));
    }

    #[test]
    fn config_debug_hides_the_secret() {
        let config = PlaidConfig {
            env: PlaidEnv::Sandbox,
            client_id: "id".into(),
            secret: "super-secret".into(),
            base_url: None,
        };
        assert!(!format!("{config:?}").contains("super-secret"));
    }

    #[test]
    fn parses_env() {
        assert_eq!(PlaidEnv::parse("sandbox"), Ok(PlaidEnv::Sandbox));
        assert_eq!(PlaidEnv::parse(" production "), Ok(PlaidEnv::Production));
        assert!(PlaidEnv::parse("development").is_err());
    }
}
