//! The provider-neutral aggregator interface (design doc §8.3, §17.1).
//!
//! `aggregator-plaid` implements it. MX (Phase 3) will too. Amounts use
//! the SaveSpace sign: negative is money out of the account.

use async_trait::async_trait;
use savespace_core::{Currency, Money};

/// Errors from an aggregator.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AggregatorError {
    /// Data changed during a paginated sync. Restart from the first cursor.
    #[error("data changed during pagination")]
    MutationDuringPagination,
    /// The person must sign in to the bank again (update mode).
    #[error("the bank login needs attention: {code}")]
    LoginRequired {
        /// The provider error code.
        code: String,
    },
    /// The connection no longer exists at the provider, or access was revoked.
    #[error("the connection is gone: {code}")]
    ItemGone {
        /// The provider error code.
        code: String,
    },
    /// The provider limits requests. Retry later.
    #[error("rate limited by the provider")]
    RateLimited,
    /// Another provider error.
    #[error("provider error {code}: {message}")]
    Provider {
        /// The provider error code.
        code: String,
        /// The provider message. Never contains secrets.
        message: String,
    },
    /// A network or HTTP error. Retry later.
    #[error("transport error: {0}")]
    Transport(String),
    /// The provider response was not what this code expects.
    #[error("invalid provider response: {0}")]
    Invalid(String),
}

impl AggregatorError {
    /// True if a retry later can succeed without the person doing anything.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::MutationDuringPagination | Self::RateLimited | Self::Transport(_)
        )
    }
}

/// Input for a link token.
#[derive(Debug, Clone)]
pub struct LinkTokenRequest {
    /// A stable, non-personal ID for the person (`usr_…`).
    pub client_user_id: String,
    /// Where the provider sends webhooks. `None` for local development.
    pub webhook_url: Option<String>,
    /// Set for update mode (repair a connection).
    pub update_access_token: Option<String>,
}

/// A link token for the client SDK.
#[derive(Debug, Clone)]
pub struct LinkToken {
    /// The token.
    pub link_token: String,
    /// When it expires (RFC 3339), if the provider says.
    pub expiration: Option<String>,
}

/// The result of a public token exchange.
#[derive(Debug, Clone)]
pub struct NewItem {
    /// The provider item ID.
    pub item_id: String,
    /// The access token. Secret: encrypt before storage.
    pub access_token: String,
}

/// Balances in minor units.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderBalances {
    /// The current balance.
    pub current: Option<i64>,
    /// The available balance.
    pub available: Option<i64>,
    /// The credit limit.
    pub limit: Option<i64>,
}

/// An account at the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccount {
    /// The provider account ID.
    pub provider_account_id: String,
    /// The account name.
    pub name: String,
    /// The official name, if any.
    pub official_name: Option<String>,
    /// The last digits of the account number.
    pub mask: Option<String>,
    /// For example `depository`, `credit`, `loan`, `investment`.
    pub kind: String,
    /// For example `checking`, `savings`, `credit card`.
    pub subtype: Option<String>,
    /// The account currency.
    pub currency: Currency,
    /// The balances.
    pub balances: ProviderBalances,
}

/// A transaction at the provider.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderTransaction {
    /// The provider transaction ID.
    pub provider_transaction_id: String,
    /// The provider account ID.
    pub provider_account_id: String,
    /// The amount. Negative is money out.
    pub amount: Money,
    /// The bank date.
    pub date: jiff::civil::Date,
    /// The date the person made the purchase, if known.
    pub authorized_date: Option<jiff::civil::Date>,
    /// The description.
    pub name: String,
    /// The merchant, if the provider knows it.
    pub merchant_name: Option<String>,
    /// True while pending.
    pub pending: bool,
    /// For a posted transaction: the pending transaction it replaces.
    pub pending_transaction_id: Option<String>,
    /// The provider's broad category.
    pub category_primary: Option<String>,
    /// The provider's detailed category.
    pub category_detailed: Option<String>,
    /// The full provider payload, stored append-only.
    pub raw: serde_json::Value,
}

/// One page of changes.
#[derive(Debug, Clone, Default)]
pub struct SyncPage {
    /// All accounts of the item, with current balances.
    pub accounts: Vec<ProviderAccount>,
    /// New transactions.
    pub added: Vec<ProviderTransaction>,
    /// Changed transactions.
    pub modified: Vec<ProviderTransaction>,
    /// Removed transaction IDs.
    pub removed: Vec<String>,
    /// The cursor for the next call.
    pub next_cursor: String,
    /// True if more pages follow.
    pub has_more: bool,
}

/// A bank data aggregator.
#[async_trait]
pub trait Aggregator: Send + Sync {
    /// The provider name stored in `app.connections.provider`.
    fn provider(&self) -> &'static str;

    /// Creates a link token for the client SDK.
    async fn create_link_token(
        &self,
        request: &LinkTokenRequest,
    ) -> Result<LinkToken, AggregatorError>;

    /// Exchanges the client's public token for an access token.
    async fn exchange_public_token(&self, public_token: &str) -> Result<NewItem, AggregatorError>;

    /// Gets one page of changes after `cursor` (`None` for the first sync).
    async fn sync_page(
        &self,
        access_token: &str,
        cursor: Option<&str>,
    ) -> Result<SyncPage, AggregatorError>;

    /// Removes the item at the provider. Stops billing and data access.
    async fn remove_item(&self, access_token: &str) -> Result<(), AggregatorError>;
}

/// The health of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// The first sync has not finished.
    Syncing,
    /// Healthy.
    Ok,
    /// The person must sign in to the bank again.
    LoginRequired,
    /// The bank consent expires soon. The person should sign in again.
    PendingExpiration,
    /// Access was revoked. The connection cannot sync.
    Revoked,
    /// The last sync failed with a provider error.
    Error,
}

impl ConnectionStatus {
    /// The stored value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Syncing => "syncing",
            Self::Ok => "ok",
            Self::LoginRequired => "login_required",
            Self::PendingExpiration => "pending_expiration",
            Self::Revoked => "revoked",
            Self::Error => "error",
        }
    }
}

/// What to do with a webhook, decided by the provider adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookAction {
    /// New data is available. Sync now.
    Sync,
    /// Set the connection status.
    SetStatus {
        /// The new status.
        status: ConnectionStatus,
        /// The provider error code, if any.
        error_code: Option<String>,
    },
    /// Nothing to do.
    Ignore,
}
