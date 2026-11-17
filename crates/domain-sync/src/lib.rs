//! `domain-sync`: bank connections and transaction sync (design doc §10.1,
//! §10.2).
//!
//! - [`Aggregator`] is the provider interface. `aggregator-plaid` implements it.
//! - [`SyncService`] connects accounts, runs the sync loop, and applies each
//!   sync in one database transaction: raw versions (append-only), current
//!   transactions, accounts, balances, the cursor, and an outbox event.
//! - Access tokens are encrypted with the household data key
//!   (`platform-crypto`). They never leave this crate in plain text, except
//!   to the aggregator.

mod provider;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use platform_crypto::{CryptoError, Keyring, Purpose};
use platform_db::jobs::{self, NewJob};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

pub use provider::{
    Aggregator, AggregatorError, ConnectionStatus, LinkToken, LinkTokenRequest, NewItem,
    ProviderAccount, ProviderBalances, ProviderTransaction, SyncPage, WebhookAction,
};

/// The job kind that syncs one connection.
pub const SYNC_JOB: &str = "sync.connection";
/// The job kind that processes one stored webhook.
pub const WEBHOOK_JOB: &str = "sync.webhook";
/// The job kind that enqueues fallback polls.
pub const POLL_JOB: &str = "sync.poll";
/// The outbox topic written after a sync that changed transactions.
pub const TRANSACTIONS_CHANGED: &str = "transactions.changed";

/// A sync restarts at most this many times after a mutation during
/// pagination.
const MAX_RESTARTS: u32 = 3;

/// The payload of a [`SYNC_JOB`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SyncJob {
    /// The household.
    pub household_id: Uuid,
    /// The connection.
    pub connection_id: Uuid,
}

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The connection does not exist in this household.
    #[error("connection not found")]
    NotFound,
    /// The connection cannot sync until the person signs in again.
    #[error("the connection needs attention")]
    NeedsAttention,
    /// Another sync changed the cursor first. Retry.
    #[error("another sync ran at the same time")]
    Concurrent,
    /// The institution is already connected. The new item was removed at the
    /// provider, so its accounts do not count twice. Holds the institution
    /// name.
    #[error("the institution is already connected")]
    AlreadyConnected(String),
    /// An aggregator error.
    #[error(transparent)]
    Aggregator(#[from] AggregatorError),
    /// A database error.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// An encryption error.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

type Result<T, E = SyncError> = std::result::Result<T, E>;

/// A connection, without secrets.
#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    /// The connection ID.
    pub id: Uuid,
    /// The provider, for example `plaid`.
    pub provider: String,
    /// The institution name, if known.
    pub institution_name: Option<String>,
    /// The status (`syncing`, `ok`, `login_required`, …).
    pub status: String,
    /// The provider error code of the last failure.
    pub error_code: Option<String>,
    /// The last successful sync.
    pub last_synced_at: Option<jiff::Timestamp>,
    /// When the person connected it.
    pub created_at: jiff::Timestamp,
}

/// An account with its latest balances.
#[derive(Debug, Clone, Serialize)]
pub struct Account {
    /// The account ID.
    pub id: Uuid,
    /// The connection ID.
    pub connection_id: Uuid,
    /// The name.
    pub name: String,
    /// The official name.
    pub official_name: Option<String>,
    /// The last digits of the account number.
    pub mask: Option<String>,
    /// The account type.
    pub kind: String,
    /// The account subtype.
    pub subtype: Option<String>,
    /// The currency code.
    pub currency: String,
    /// The current balance in minor units.
    pub current_minor: Option<i64>,
    /// The available balance in minor units.
    pub available_minor: Option<i64>,
    /// The credit limit in minor units.
    pub limit_minor: Option<i64>,
    /// When the balances were read.
    pub balance_at: Option<jiff::Timestamp>,
}

/// The counts from one sync.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SyncOutcome {
    /// Accounts seen.
    pub accounts: usize,
    /// Transactions added.
    pub added: usize,
    /// Transactions modified.
    pub modified: usize,
    /// Transactions removed.
    pub removed: usize,
}

/// Connections and sync.
#[derive(Clone)]
pub struct SyncService {
    pool: PgPool,
    keyring: Keyring,
    aggregator: Arc<dyn Aggregator>,
}

impl std::fmt::Debug for SyncService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncService")
            .field("provider", &self.aggregator.provider())
            .finish_non_exhaustive()
    }
}

type ConnectionRow = (
    Uuid,
    String,
    Option<String>,
    String,
    Option<String>,
    Option<jiff_sqlx::Timestamp>,
    jiff_sqlx::Timestamp,
);

type AccountRow = (
    Uuid,
    Uuid,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    String,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<jiff_sqlx::Timestamp>,
);

impl SyncService {
    /// Creates the service.
    pub fn new(pool: PgPool, keyring: Keyring, aggregator: Arc<dyn Aggregator>) -> Self {
        Self {
            pool,
            keyring,
            aggregator,
        }
    }

    /// The provider name.
    pub fn provider(&self) -> &'static str {
        self.aggregator.provider()
    }

    /// Creates a link token. With `connection_id`, the token opens update
    /// mode for that connection (to repair a login).
    ///
    /// # Errors
    ///
    /// [`SyncError::NotFound`], an aggregator error, or a database error.
    pub async fn create_link_token(
        &self,
        household_id: Uuid,
        client_user_id: &str,
        webhook_url: Option<&str>,
        connection_id: Option<Uuid>,
    ) -> Result<LinkToken> {
        let update_access_token = match connection_id {
            Some(id) => Some(self.access_token(household_id, id).await?.1),
            None => None,
        };
        Ok(self
            .aggregator
            .create_link_token(&LinkTokenRequest {
                client_user_id: client_user_id.to_owned(),
                webhook_url: webhook_url.map(str::to_owned),
                update_access_token,
            })
            .await?)
    }

    /// Exchanges a public token, stores the encrypted access token, and
    /// queues the first sync, in one transaction (§10.1 step 7).
    ///
    /// # Errors
    ///
    /// An aggregator error or a database error.
    pub async fn create_connection(
        &self,
        household_id: Uuid,
        user_id: Uuid,
        public_token: &str,
        institution: Option<(&str, &str)>,
        request_id: Option<&str>,
    ) -> Result<Connection> {
        let item = self.aggregator.exchange_public_token(public_token).await?;
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        if let Some((institution_id, name)) = institution {
            let existing: Option<Option<String>> = sqlx::query_scalar(
                "SELECT institution_name FROM app.connections WHERE institution_id = $1 LIMIT 1",
            )
            .bind(institution_id)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(existing) = existing {
                drop(tx);
                // Linking the same bank twice would count every transaction
                // twice. The person repairs a connection with update mode.
                if let Err(e) = self.aggregator.remove_item(&item.access_token).await {
                    tracing::warn!(error = %e, "could not remove a duplicate item");
                }
                return Err(SyncError::AlreadyConnected(
                    existing.unwrap_or_else(|| name.to_owned()),
                ));
            }
        }
        let dek = self.keyring.dek(&mut tx, household_id).await?;
        let sealed = dek.seal(
            Purpose::AccessToken,
            household_id,
            item.access_token.as_bytes(),
        );
        let row: ConnectionRow = sqlx::query_as(
            "INSERT INTO app.connections \
               (household_id, provider, provider_item_id, access_token_enc, \
                institution_id, institution_name) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id, provider, institution_name, status, error_code, \
                       last_synced_at, created_at",
        )
        .bind(household_id)
        .bind(self.aggregator.provider())
        .bind(&item.item_id)
        .bind(sealed)
        .bind(institution.map(|(id, _)| id))
        .bind(institution.map(|(_, name)| name))
        .fetch_one(&mut *tx)
        .await?;
        let connection = connection_from(row);
        enqueue_sync(&mut tx, household_id, connection.id).await?;
        audit(
            &mut tx,
            household_id,
            Some(user_id),
            "connection.create",
            json!({ "connection_id": connection.id }),
            request_id,
        )
        .await?;
        tx.commit().await?;
        Ok(connection)
    }

    /// Queues a sync for a connection. A queued sync for the same connection
    /// absorbs this one.
    ///
    /// # Errors
    ///
    /// [`SyncError::NotFound`] or a database error.
    pub async fn request_sync(&self, household_id: Uuid, connection_id: Uuid) -> Result<()> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM app.connections WHERE id = $1")
                .bind(connection_id)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(SyncError::NotFound);
        }
        enqueue_sync(&mut tx, household_id, connection_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Lists the household's connections.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn list_connections(&self, household_id: Uuid) -> Result<Vec<Connection>> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rows: Vec<ConnectionRow> = sqlx::query_as(
            "SELECT id, provider, institution_name, status, error_code, last_synced_at, created_at \
             FROM app.connections ORDER BY created_at",
        )
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows.into_iter().map(connection_from).collect())
    }

    /// Lists the household's accounts with their latest balances.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn list_accounts(&self, household_id: Uuid) -> Result<Vec<Account>> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rows: Vec<AccountRow> = sqlx::query_as(
            "SELECT id, connection_id, name, official_name, mask, type, subtype, currency, \
                    current_minor, available_minor, limit_minor, balance_at \
             FROM app.accounts ORDER BY connection_id, type, name",
        )
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(|r| Account {
                id: r.0,
                connection_id: r.1,
                name: r.2,
                official_name: r.3,
                mask: r.4,
                kind: r.5,
                subtype: r.6,
                currency: r.7,
                current_minor: r.8,
                available_minor: r.9,
                limit_minor: r.10,
                balance_at: r.11.map(jiff_sqlx::Timestamp::to_jiff),
            })
            .collect())
    }

    /// Removes every connection of the household at the provider (account
    /// deletion, design doc §12.5). The database rows stay: deleting the
    /// household removes them. Returns how many connections it removed.
    ///
    /// # Errors
    ///
    /// A transient aggregator error (the caller asks the person to try
    /// again), or a database or decryption error.
    pub async fn remove_all_items(&self, household_id: Uuid) -> Result<usize> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM app.connections")
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        for id in &ids {
            let (_, access_token) = self.access_token(household_id, *id).await?;
            match self.aggregator.remove_item(&access_token).await {
                Ok(()) | Err(AggregatorError::ItemGone { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(ids.len())
    }

    /// Removes a connection at the provider, then deletes it and all its
    /// accounts and transactions.
    ///
    /// # Errors
    ///
    /// [`SyncError::NotFound`], a transient aggregator error, or a database
    /// error.
    pub async fn delete_connection(
        &self,
        household_id: Uuid,
        user_id: Uuid,
        connection_id: Uuid,
        request_id: Option<&str>,
    ) -> Result<()> {
        let (_, access_token) = self.access_token(household_id, connection_id).await?;
        match self.aggregator.remove_item(&access_token).await {
            Ok(()) | Err(AggregatorError::ItemGone { .. }) => {}
            Err(e) => return Err(e.into()),
        }
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        sqlx::query("DELETE FROM app.connections WHERE id = $1")
            .bind(connection_id)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut tx,
            household_id,
            Some(user_id),
            "connection.delete",
            json!({ "connection_id": connection_id }),
            request_id,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The stored cursor and the decrypted access token.
    async fn access_token(
        &self,
        household_id: Uuid,
        connection_id: Uuid,
    ) -> Result<(Option<String>, String)> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let row: Option<(Option<String>, Vec<u8>)> = sqlx::query_as(
            "SELECT sync_cursor, access_token_enc FROM app.connections WHERE id = $1",
        )
        .bind(connection_id)
        .fetch_optional(&mut *tx)
        .await?;
        let (cursor, sealed) = row.ok_or(SyncError::NotFound)?;
        let dek = self.keyring.dek(&mut tx, household_id).await?;
        tx.commit().await?;
        let token = dek.open(Purpose::AccessToken, household_id, &sealed)?;
        let token = String::from_utf8(token.to_vec())
            .map_err(|_| SyncError::Crypto(CryptoError::Decrypt))?;
        Ok((cursor, token))
    }

    /// Syncs one connection (§10.2): pages from the stored cursor until
    /// `has_more` is false, then applies all changes in one transaction.
    ///
    /// A connection that needs the person (login required, revoked) gets
    /// its status set, and this returns [`SyncError::NeedsAttention`].
    ///
    /// # Errors
    ///
    /// [`SyncError::NeedsAttention`], [`SyncError::Concurrent`], a transient
    /// aggregator error (retry), or a database error.
    pub async fn sync_connection(
        &self,
        household_id: Uuid,
        connection_id: Uuid,
    ) -> Result<SyncOutcome> {
        let (start_cursor, access_token) = self.access_token(household_id, connection_id).await?;

        let mut restarts = 0;
        let changes = loop {
            match self.fetch_all(&access_token, start_cursor.clone()).await {
                Ok(changes) => break changes,
                Err(AggregatorError::MutationDuringPagination) if restarts < MAX_RESTARTS => {
                    restarts += 1;
                    tracing::info!(restarts, "data changed during pagination; restarting");
                }
                Err(e) => {
                    let status = match &e {
                        AggregatorError::LoginRequired { code } => {
                            Some((ConnectionStatus::LoginRequired, Some(code.clone())))
                        }
                        AggregatorError::ItemGone { code } => {
                            Some((ConnectionStatus::Revoked, Some(code.clone())))
                        }
                        AggregatorError::Provider { code, .. } => {
                            Some((ConnectionStatus::Error, Some(code.clone())))
                        }
                        _ => None,
                    };
                    if let Some((status, code)) = status {
                        self.set_status(household_id, connection_id, status, code.as_deref())
                            .await?;
                        if status != ConnectionStatus::Error {
                            return Err(SyncError::NeedsAttention);
                        }
                    }
                    return Err(e.into());
                }
            }
        };
        self.apply(
            household_id,
            connection_id,
            start_cursor.as_deref(),
            changes,
        )
        .await
    }

    async fn fetch_all(
        &self,
        access_token: &str,
        start: Option<String>,
    ) -> Result<Changes, AggregatorError> {
        let mut changes = Changes::default();
        let mut cursor = start;
        loop {
            let page = self
                .aggregator
                .sync_page(access_token, cursor.as_deref())
                .await?;
            changes.merge(page.accounts, page.added, page.modified, page.removed);
            cursor = Some(page.next_cursor);
            if !page.has_more {
                break;
            }
        }
        changes.cursor = cursor.unwrap_or_default();
        Ok(changes)
    }

    #[expect(clippy::too_many_lines, reason = "one transaction, step by step")]
    async fn apply(
        &self,
        household_id: Uuid,
        connection_id: Uuid,
        start_cursor: Option<&str>,
        changes: Changes,
    ) -> Result<SyncOutcome> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;

        // Compare and set: another sync may have applied in the meantime.
        let current: Option<Option<String>> =
            sqlx::query_scalar("SELECT sync_cursor FROM app.connections WHERE id = $1 FOR UPDATE")
                .bind(connection_id)
                .fetch_optional(&mut *tx)
                .await?;
        let current = current.ok_or(SyncError::NotFound)?;
        if current.as_deref() != start_cursor {
            return Err(SyncError::Concurrent);
        }

        let accounts: Vec<Value> = changes
            .accounts
            .values()
            .map(|a| {
                json!({
                    "id": a.provider_account_id,
                    "name": a.name,
                    "official_name": a.official_name,
                    "mask": a.mask,
                    "type": a.kind,
                    "subtype": a.subtype,
                    "currency": a.currency.code(),
                    "current": a.balances.current,
                    "available": a.balances.available,
                    "limit": a.balances.limit,
                })
            })
            .collect();
        sqlx::query(
            "WITH upserted AS ( \
               INSERT INTO app.accounts AS a (household_id, connection_id, provider_account_id, \
                 name, official_name, mask, type, subtype, currency, current_minor, \
                 available_minor, limit_minor, balance_at) \
               SELECT $1, $2, r.id, r.name, r.official_name, r.mask, r.type, r.subtype, \
                 r.currency, r.current, r.available, r.\"limit\", now() \
               FROM jsonb_to_recordset($3) AS r(id text, name text, official_name text, \
                 mask text, type text, subtype text, currency text, current bigint, \
                 available bigint, \"limit\" bigint) \
               ON CONFLICT (connection_id, provider_account_id) DO UPDATE SET \
                 name = EXCLUDED.name, official_name = EXCLUDED.official_name, \
                 mask = EXCLUDED.mask, type = EXCLUDED.type, subtype = EXCLUDED.subtype, \
                 currency = EXCLUDED.currency, current_minor = EXCLUDED.current_minor, \
                 available_minor = EXCLUDED.available_minor, \
                 limit_minor = EXCLUDED.limit_minor, balance_at = now(), updated_at = now() \
               RETURNING a.id, a.currency, a.current_minor, a.available_minor, a.limit_minor) \
             INSERT INTO app.balances (household_id, account_id, currency, current_minor, \
               available_minor, limit_minor) \
             SELECT $1, id, currency, current_minor, available_minor, limit_minor FROM upserted",
        )
        .bind(household_id)
        .bind(connection_id)
        .bind(Value::from(accounts))
        .execute(&mut *tx)
        .await?;

        let upserts: Vec<&ProviderTransaction> = changes.transactions.values().collect();
        if !upserts.is_empty() {
            let raw: Vec<Value> = upserts
                .iter()
                .map(|t| json!({ "id": t.provider_transaction_id, "payload": t.raw }))
                .collect();
            sqlx::query(
                "INSERT INTO app.transactions_raw (household_id, connection_id, \
                   provider_transaction_id, version, payload) \
                 SELECT $1, $2, r.id, coalesce(( \
                     SELECT max(x.version) FROM app.transactions_raw x \
                     WHERE x.connection_id = $2 AND x.provider_transaction_id = r.id), 0) + 1, \
                   r.payload \
                 FROM jsonb_to_recordset($3) AS r(id text, payload jsonb)",
            )
            .bind(household_id)
            .bind(connection_id)
            .bind(Value::from(raw))
            .execute(&mut *tx)
            .await?;

            let rows: Vec<Value> = upserts
                .iter()
                .map(|t| {
                    json!({
                        "id": t.provider_transaction_id,
                        "account": t.provider_account_id,
                        "amount": t.amount.minor(),
                        "currency": t.amount.currency().code(),
                        "date": t.date.to_string(),
                        "authorized_date": t.authorized_date.map(|d| d.to_string()),
                        "name": t.name,
                        "merchant_name": t.merchant_name,
                        "pending": t.pending,
                        "pending_id": t.pending_transaction_id,
                        "cat1": t.category_primary,
                        "cat2": t.category_detailed,
                    })
                })
                .collect();
            let written = sqlx::query(
                "INSERT INTO app.transactions AS t (household_id, connection_id, account_id, \
                   provider_transaction_id, raw_version, amount_minor, currency, date, \
                   authorized_date, name, merchant_name, pending, \
                   pending_provider_transaction_id, provider_category_primary, \
                   provider_category_detailed) \
                 SELECT $1, $2, a.id, r.id, \
                   (SELECT max(x.version) FROM app.transactions_raw x \
                    WHERE x.connection_id = $2 AND x.provider_transaction_id = r.id), \
                   r.amount, r.currency, r.date, r.authorized_date, r.name, r.merchant_name, \
                   r.pending, r.pending_id, r.cat1, r.cat2 \
                 FROM jsonb_to_recordset($3) AS r(id text, account text, amount bigint, \
                   currency text, date date, authorized_date date, name text, \
                   merchant_name text, pending boolean, pending_id text, cat1 text, cat2 text) \
                 JOIN app.accounts a ON a.connection_id = $2 AND a.provider_account_id = r.account \
                 ON CONFLICT (connection_id, provider_transaction_id) DO UPDATE SET \
                   account_id = EXCLUDED.account_id, raw_version = EXCLUDED.raw_version, \
                   amount_minor = EXCLUDED.amount_minor, currency = EXCLUDED.currency, \
                   date = EXCLUDED.date, authorized_date = EXCLUDED.authorized_date, \
                   name = EXCLUDED.name, merchant_name = EXCLUDED.merchant_name, \
                   pending = EXCLUDED.pending, \
                   pending_provider_transaction_id = EXCLUDED.pending_provider_transaction_id, \
                   provider_category_primary = EXCLUDED.provider_category_primary, \
                   provider_category_detailed = EXCLUDED.provider_category_detailed, \
                   enriched_version = NULL, updated_at = now()",
            )
            .bind(household_id)
            .bind(connection_id)
            .bind(Value::from(rows))
            .execute(&mut *tx)
            .await?
            .rows_affected();
            let expected = u64::try_from(upserts.len()).unwrap_or(u64::MAX);
            if written != expected {
                tracing::warn!(
                    written,
                    expected,
                    "some transactions reference an unknown account"
                );
            }
        }

        if !changes.removed.is_empty() {
            // Pending to posted (§10.2): the person's edits move to the posted
            // transaction before the pending one is deleted. Splits do not
            // move: only posted transactions can have them.
            sqlx::query(
                "UPDATE app.user_overrides o SET transaction_id = posted.id, updated_at = now() \
                 FROM app.transactions pend \
                 JOIN app.transactions posted ON posted.connection_id = pend.connection_id \
                   AND posted.pending_provider_transaction_id = pend.provider_transaction_id \
                 WHERE o.transaction_id = pend.id AND pend.connection_id = $1 \
                   AND pend.provider_transaction_id = ANY($2) \
                   AND NOT EXISTS (SELECT 1 FROM app.user_overrides x \
                                   WHERE x.transaction_id = posted.id)",
            )
            .bind(connection_id)
            .bind(&changes.removed)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE app.transactions_raw SET removed_at = now() \
                 WHERE connection_id = $1 AND provider_transaction_id = ANY($2) \
                   AND removed_at IS NULL",
            )
            .bind(connection_id)
            .bind(&changes.removed)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "DELETE FROM app.transactions \
                 WHERE connection_id = $1 AND provider_transaction_id = ANY($2)",
            )
            .bind(connection_id)
            .bind(&changes.removed)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            "UPDATE app.connections SET sync_cursor = $2, status = 'ok', error_code = NULL, \
               last_synced_at = now(), updated_at = now() WHERE id = $1",
        )
        .bind(connection_id)
        .bind(&changes.cursor)
        .execute(&mut *tx)
        .await?;

        let outcome = SyncOutcome {
            accounts: changes.accounts.len(),
            added: changes.added,
            modified: changes.modified,
            removed: changes.removed.len(),
        };
        if outcome.added + outcome.modified + outcome.removed > 0 {
            sqlx::query(
                "INSERT INTO app.outbox (topic, household_id, payload) VALUES ($1, $2, $3)",
            )
            .bind(TRANSACTIONS_CHANGED)
            .bind(household_id)
            .bind(json!({
                "connection_id": connection_id,
                "added": outcome.added,
                "modified": outcome.modified,
                "removed": outcome.removed,
            }))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(outcome)
    }

    async fn set_status(
        &self,
        household_id: Uuid,
        connection_id: Uuid,
        status: ConnectionStatus,
        error_code: Option<&str>,
    ) -> Result<()> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        sqlx::query(
            "UPDATE app.connections SET status = $2, error_code = $3, updated_at = now() \
             WHERE id = $1",
        )
        .bind(connection_id)
        .bind(status.as_str())
        .bind(error_code)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Stores a verified webhook and queues a job to process it. Fast: the
    /// API calls this on the request path (§10.2).
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn store_webhook(
        &self,
        item_id: Option<&str>,
        webhook_type: &str,
        webhook_code: &str,
        body: &Value,
    ) -> Result<Uuid> {
        let mut tx = self.pool.begin().await?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO app.webhook_events (provider, item_id, webhook_type, webhook_code, body) \
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(self.aggregator.provider())
        .bind(item_id)
        .bind(webhook_type)
        .bind(webhook_code)
        .bind(body)
        .fetch_one(&mut *tx)
        .await?;
        jobs::enqueue(
            &mut *tx,
            &NewJob::new(WEBHOOK_JOB, json!({ "event_id": id })),
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Loads a stored webhook: item ID, type, code, and body.
    ///
    /// # Errors
    ///
    /// [`SyncError::NotFound`] or a database error.
    pub async fn webhook_event(
        &self,
        event_id: Uuid,
    ) -> Result<(Option<String>, String, String, Value)> {
        let row: Option<(Option<String>, String, String, Value)> = sqlx::query_as(
            "SELECT item_id, webhook_type, webhook_code, body FROM app.webhook_events \
             WHERE id = $1",
        )
        .bind(event_id)
        .fetch_optional(&self.pool)
        .await?;
        row.ok_or(SyncError::NotFound)
    }

    /// Applies a webhook action to the connection for `item_id`, and marks
    /// the event processed. Unknown items are ignored (for example, after a
    /// connection was deleted).
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn apply_webhook(
        &self,
        event_id: Uuid,
        item_id: Option<&str>,
        action: &WebhookAction,
    ) -> Result<()> {
        let found: Option<(Uuid, Uuid)> = match item_id {
            Some(item) => {
                sqlx::query_as("SELECT id, household_id FROM app.find_connection_by_item($1, $2)")
                    .bind(self.aggregator.provider())
                    .bind(item)
                    .fetch_optional(&self.pool)
                    .await?
            }
            None => None,
        };
        if let Some((connection_id, household_id)) = found {
            match action {
                WebhookAction::Sync => {
                    let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
                    enqueue_sync(&mut tx, household_id, connection_id).await?;
                    tx.commit().await?;
                }
                WebhookAction::SetStatus { status, error_code } => {
                    self.set_status(household_id, connection_id, *status, error_code.as_deref())
                        .await?;
                }
                WebhookAction::Ignore => {}
            }
        } else {
            tracing::info!("webhook for an unknown item; ignored");
        }
        sqlx::query("UPDATE app.webhook_events SET processed_at = now() WHERE id = $1")
            .bind(event_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Queues a sync for each healthy connection that has not synced for
    /// `max_age` (the fallback poll, §10.2). Returns the number queued.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn enqueue_due_polls(&self, max_age: std::time::Duration) -> Result<usize> {
        let due: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, household_id FROM app.connections_due(make_interval(secs => $1))",
        )
        .bind(max_age.as_secs_f64())
        .fetch_all(&self.pool)
        .await?;
        for (connection_id, household_id) in &due {
            let mut tx = platform_db::tenant_tx(&self.pool, *household_id).await?;
            enqueue_sync(&mut tx, *household_id, *connection_id).await?;
            tx.commit().await?;
        }
        Ok(due.len())
    }
}

/// All changes from one sync, merged across pages. A transaction that
/// appears twice keeps its last form.
#[derive(Debug, Default)]
struct Changes {
    accounts: BTreeMap<String, ProviderAccount>,
    transactions: HashMap<String, ProviderTransaction>,
    added: usize,
    modified: usize,
    removed: Vec<String>,
    cursor: String,
}

impl Changes {
    fn merge(
        &mut self,
        accounts: Vec<ProviderAccount>,
        added: Vec<ProviderTransaction>,
        modified: Vec<ProviderTransaction>,
        removed: Vec<String>,
    ) {
        for a in accounts {
            self.accounts.insert(a.provider_account_id.clone(), a);
        }
        self.added += added.len();
        self.modified += modified.len();
        for t in added.into_iter().chain(modified) {
            self.removed.retain(|id| *id != t.provider_transaction_id);
            self.transactions
                .insert(t.provider_transaction_id.clone(), t);
        }
        for id in removed {
            self.transactions.remove(&id);
            self.removed.push(id);
        }
    }
}

fn connection_from(r: ConnectionRow) -> Connection {
    Connection {
        id: r.0,
        provider: r.1,
        institution_name: r.2,
        status: r.3,
        error_code: r.4,
        last_synced_at: r.5.map(jiff_sqlx::Timestamp::to_jiff),
        created_at: r.6.to_jiff(),
    }
}

async fn enqueue_sync(
    conn: &mut sqlx::PgConnection,
    household_id: Uuid,
    connection_id: Uuid,
) -> Result<(), sqlx::Error> {
    let key = connection_id.to_string();
    let mut job = NewJob::new(
        SYNC_JOB,
        serde_json::to_value(SyncJob {
            household_id,
            connection_id,
        })
        .unwrap_or_default(),
    );
    job.dedupe_key = Some(&key);
    jobs::enqueue(conn, &job).await?;
    Ok(())
}

async fn audit(
    tx: &mut platform_db::TenantTx,
    household_id: Uuid,
    user_id: Option<Uuid>,
    action: &str,
    detail: Value,
    request_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO app.audit_log (household_id, user_id, action, detail, request_id) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(household_id)
    .bind(user_id)
    .bind(action)
    .bind(detail)
    .bind(request_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
