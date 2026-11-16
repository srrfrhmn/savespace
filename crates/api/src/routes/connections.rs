//! Bank connections, accounts, and transactions (design doc §10.1, §11.6).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use domain_sync::{SyncError, SyncService};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::error::{ApiError, ErrorBody, ErrorType, Json};
use crate::session::Auth;
use crate::{AppState, ids};

/// Routes that need a session.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_link_token))
        .routes(routes!(create_connection, list_connections))
        .routes(routes!(delete_connection))
        .routes(routes!(sync_connection))
        .routes(routes!(list_accounts))
        .routes(routes!(list_transactions))
}

/// The sync service, or 503 if Plaid is not configured on this server.
fn sync(state: &AppState) -> Result<&SyncService, ApiError> {
    state.sync.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorType::ApiError,
            "aggregator_unavailable",
            "Bank connections are not configured on this server.",
        )
    })
}

impl From<SyncError> for ApiError {
    fn from(e: SyncError) -> Self {
        match e {
            SyncError::NotFound => Self::not_found(),
            SyncError::Aggregator(a) if a.is_transient() => {
                tracing::warn!(error = %a, "aggregator unavailable");
                Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    ErrorType::ApiError,
                    "aggregator_unavailable",
                    "The bank connection service did not answer. Try again soon.",
                )
            }
            SyncError::Aggregator(a) => {
                tracing::warn!(error = %a, "aggregator error");
                Self::new(
                    StatusCode::BAD_GATEWAY,
                    ErrorType::ApiError,
                    "aggregator_error",
                    "The bank connection service returned an error.",
                )
            }
            other => {
                tracing::error!(error = %other, "sync error");
                Self::internal()
            }
        }
    }
}

/// Create a link token.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct LinkTokenRequest {
    /// Set to repair an existing connection (update mode).
    pub connection_id: Option<String>,
}

/// A link token for Plaid Link.
#[derive(Debug, Serialize, ToSchema)]
pub struct LinkToken {
    /// Pass to `Plaid.create({ token })`.
    pub link_token: String,
    /// When it expires (RFC 3339).
    pub expiration: Option<String>,
}

/// Connect accounts with the result of Plaid Link.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateConnectionRequest {
    /// The `public_token` from Plaid Link `onSuccess`.
    pub public_token: String,
    /// The institution from the Link metadata, for display.
    pub institution: Option<Institution>,
}

/// An institution.
#[derive(Debug, Deserialize, ToSchema)]
pub struct Institution {
    /// The provider's institution ID.
    pub id: String,
    /// The institution name.
    pub name: String,
}

/// A bank connection.
#[derive(Debug, Serialize, ToSchema)]
pub struct Connection {
    /// The connection ID (`conn_…`).
    pub id: String,
    /// The institution name.
    pub institution_name: Option<String>,
    /// `syncing`, `ok`, `login_required`, `pending_expiration`, `revoked`, or `error`.
    pub status: String,
    /// The provider error code of the last failure.
    pub error_code: Option<String>,
    /// The last successful sync (RFC 3339).
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_synced_at: Option<jiff::Timestamp>,
    /// When it was connected (RFC 3339).
    #[schema(value_type = String, format = DateTime)]
    pub created_at: jiff::Timestamp,
}

impl From<domain_sync::Connection> for Connection {
    fn from(c: domain_sync::Connection) -> Self {
        Self {
            id: ids::encode(ids::CONNECTION, c.id),
            institution_name: c.institution_name,
            status: c.status,
            error_code: c.error_code,
            last_synced_at: c.last_synced_at,
            created_at: c.created_at,
        }
    }
}

/// A list of connections.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConnectionList {
    /// The connections, oldest first.
    pub data: Vec<Connection>,
}

/// A bank or card account.
#[derive(Debug, Serialize, ToSchema)]
pub struct Account {
    /// The account ID (`acct_…`).
    pub id: String,
    /// The connection ID (`conn_…`).
    pub connection_id: String,
    /// The account name.
    pub name: String,
    /// The official name.
    pub official_name: Option<String>,
    /// The last digits of the account number.
    pub mask: Option<String>,
    /// For example `depository`, `credit`, `loan`.
    #[serde(rename = "type")]
    pub kind: String,
    /// For example `checking`, `savings`, `credit card`.
    pub subtype: Option<String>,
    /// ISO 4217 code.
    pub currency: String,
    /// The current balance in minor units.
    pub current_minor: Option<i64>,
    /// The available balance in minor units.
    pub available_minor: Option<i64>,
    /// The credit limit in minor units.
    pub limit_minor: Option<i64>,
    /// When the balances were read (RFC 3339).
    #[schema(value_type = Option<String>, format = DateTime)]
    pub balance_at: Option<jiff::Timestamp>,
}

/// A list of accounts.
#[derive(Debug, Serialize, ToSchema)]
pub struct AccountList {
    /// The accounts.
    pub data: Vec<Account>,
}

/// A transaction.
#[derive(Debug, Serialize, ToSchema)]
pub struct Transaction {
    /// The transaction ID (`txn_…`).
    pub id: String,
    /// The account ID (`acct_…`).
    pub account_id: String,
    /// The amount in minor units. Negative is money out.
    pub amount_minor: i64,
    /// ISO 4217 code.
    pub currency: String,
    /// The bank date (`YYYY-MM-DD`).
    #[schema(value_type = String, format = Date)]
    pub date: jiff::civil::Date,
    /// The description.
    pub name: String,
    /// The merchant, if known.
    pub merchant_name: Option<String>,
    /// True while pending.
    pub pending: bool,
    /// The provider's broad category.
    pub category_primary: Option<String>,
}

/// A page of transactions (§11.5).
#[derive(Debug, Serialize, ToSchema)]
pub struct TransactionList {
    /// Newest first.
    pub data: Vec<Transaction>,
    /// True if more transactions follow. Pass the last ID as `starting_after`.
    pub has_more: bool,
}

/// Paging for the transaction list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TransactionQuery {
    /// 1 to 100. The default is 50.
    pub limit: Option<u16>,
    /// The last transaction ID (`txn_…`) of the previous page.
    pub starting_after: Option<String>,
}

fn connection_id(value: &str) -> Result<uuid::Uuid, ApiError> {
    ids::decode(ids::CONNECTION, value).ok_or_else(ApiError::not_found)
}

/// The webhook URL for link tokens, or `None` when the public origin is
/// local (Plaid cannot reach it).
fn webhook_url(state: &AppState) -> Option<String> {
    let origin = &state.public_origin;
    (origin.host_str() != Some("localhost") && origin.scheme() == "https")
        .then(|| format!("{}/v1/webhooks/plaid", crate::guard::origin_of(origin)))
}

/// Creates a Plaid Link token. Needs step-up (§12.1: a new connection).
#[utoipa::path(post, path = "/link_tokens", tag = "connections",
    request_body = LinkTokenRequest,
    responses((status = 200, body = LinkToken), (status = "4XX", body = ErrorBody),
              (status = 503, body = ErrorBody)))]
async fn create_link_token(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<LinkTokenRequest>,
) -> Result<Json<LinkToken>, ApiError> {
    auth.require_step_up()?;
    let sync = sync(&state)?;
    let connection = body
        .connection_id
        .as_deref()
        .map(connection_id)
        .transpose()?;
    let token = sync
        .create_link_token(
            auth.session.household_id,
            &ids::encode(ids::USER, auth.session.user_id),
            webhook_url(&state).as_deref(),
            connection,
        )
        .await?;
    Ok(Json(LinkToken {
        link_token: token.link_token,
        expiration: token.expiration,
    }))
}

/// Connects accounts: exchanges the public token and starts the first sync.
#[utoipa::path(post, path = "/connections", tag = "connections",
    request_body = CreateConnectionRequest,
    responses((status = 201, body = Connection), (status = "4XX", body = ErrorBody),
              (status = 503, body = ErrorBody)))]
async fn create_connection(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<CreateConnectionRequest>,
) -> Result<(StatusCode, Json<Connection>), ApiError> {
    let sync = sync(&state)?;
    if body.public_token.trim().is_empty() || body.public_token.len() > 500 {
        return Err(
            ApiError::invalid("public_token_invalid", "public_token is not valid.")
                .param("public_token"),
        );
    }
    let institution = body
        .institution
        .as_ref()
        .map(|i| (truncate(&i.id, 100), truncate(&i.name, 200)));
    let connection = sync
        .create_connection(
            auth.session.household_id,
            auth.session.user_id,
            body.public_token.trim(),
            institution
                .as_ref()
                .map(|(id, name)| (id.as_str(), name.as_str())),
            auth.ctx.request_id.as_deref(),
        )
        .await?;
    state.worker.kick().await;
    Ok((StatusCode::CREATED, Json(connection.into())))
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// Lists bank connections and their health.
#[utoipa::path(get, path = "/connections", tag = "connections",
    responses((status = 200, body = ConnectionList), (status = 401, body = ErrorBody)))]
async fn list_connections(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<ConnectionList>, ApiError> {
    let Some(sync) = state.sync.as_ref() else {
        return Ok(Json(ConnectionList { data: Vec::new() }));
    };
    let data = sync
        .list_connections(auth.session.household_id)
        .await?
        .into_iter()
        .map(Connection::from)
        .collect();
    Ok(Json(ConnectionList { data }))
}

/// Starts a sync now.
#[utoipa::path(post, path = "/connections/{id}/sync", tag = "connections",
    params(("id" = String, Path, description = "The connection ID (`conn_…`).")),
    responses((status = 202), (status = "4XX", body = ErrorBody)))]
async fn sync_connection(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let sync = sync(&state)?;
    sync.request_sync(auth.session.household_id, connection_id(&id)?)
        .await?;
    state.worker.kick().await;
    Ok(StatusCode::ACCEPTED)
}

/// Removes a connection at the bank and deletes its accounts and
/// transactions. Needs step-up.
#[utoipa::path(delete, path = "/connections/{id}", tag = "connections",
    params(("id" = String, Path, description = "The connection ID (`conn_…`).")),
    responses((status = 204), (status = "4XX", body = ErrorBody)))]
async fn delete_connection(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    auth.require_step_up()?;
    let sync = sync(&state)?;
    sync.delete_connection(
        auth.session.household_id,
        auth.session.user_id,
        connection_id(&id)?,
        auth.ctx.request_id.as_deref(),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Lists accounts with their latest balances.
#[utoipa::path(get, path = "/accounts", tag = "connections",
    responses((status = 200, body = AccountList), (status = 401, body = ErrorBody)))]
async fn list_accounts(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<AccountList>, ApiError> {
    let Some(sync) = state.sync.as_ref() else {
        return Ok(Json(AccountList { data: Vec::new() }));
    };
    let data = sync
        .list_accounts(auth.session.household_id)
        .await?
        .into_iter()
        .map(|a| Account {
            id: ids::encode(ids::ACCOUNT, a.id),
            connection_id: ids::encode(ids::CONNECTION, a.connection_id),
            name: a.name,
            official_name: a.official_name,
            mask: a.mask,
            kind: a.kind,
            subtype: a.subtype,
            currency: a.currency,
            current_minor: a.current_minor,
            available_minor: a.available_minor,
            limit_minor: a.limit_minor,
            balance_at: a.balance_at,
        })
        .collect();
    Ok(Json(AccountList { data }))
}

/// Lists transactions, newest first. M3 adds filters and search.
#[utoipa::path(get, path = "/transactions", tag = "connections",
    params(TransactionQuery),
    responses((status = 200, body = TransactionList), (status = "4XX", body = ErrorBody)))]
async fn list_transactions(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<TransactionQuery>,
) -> Result<Json<TransactionList>, ApiError> {
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::invalid("limit_invalid", "limit must be 1 to 100.").param("limit"));
    }
    let Some(sync) = state.sync.as_ref() else {
        return Ok(Json(TransactionList {
            data: Vec::new(),
            has_more: false,
        }));
    };
    let starting_after = query
        .starting_after
        .as_deref()
        .map(|s| {
            ids::decode(ids::TRANSACTION, s).ok_or_else(|| {
                ApiError::invalid("cursor_invalid", "starting_after is not a transaction ID.")
                    .param("starting_after")
            })
        })
        .transpose()?;
    // Ask for one more to know whether more follow.
    let mut rows = sync
        .list_transactions(auth.session.household_id, limit + 1, starting_after)
        .await?;
    let has_more = rows.len() > usize::from(limit);
    rows.truncate(usize::from(limit));
    let data = rows
        .into_iter()
        .map(|t| Transaction {
            id: ids::encode(ids::TRANSACTION, t.id),
            account_id: ids::encode(ids::ACCOUNT, t.account_id),
            amount_minor: t.amount_minor,
            currency: t.currency,
            date: t.date,
            name: t.name,
            merchant_name: t.merchant_name,
            pending: t.pending,
            category_primary: t.category_primary,
        })
        .collect();
    Ok(Json(TransactionList { data, has_more }))
}
