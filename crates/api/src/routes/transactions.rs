//! Transactions, categories, and categorization rules (design doc §11.6).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use domain_categorize::{CategorizeError, Direction, RuleMatch};
use domain_ledger::{CategoryFilter, LedgerError, NewSplit, TransactionFilter, TransactionPatch};
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::error::{ApiError, ErrorBody, Json};
use crate::session::Auth;
use crate::{AppState, ids};

/// Routes that need a session.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_transactions))
        .routes(routes!(get_transaction, update_transaction))
        .routes(routes!(set_splits))
        .routes(routes!(list_categories))
        .routes(routes!(list_rules, create_rule))
        .routes(routes!(delete_rule))
}

impl From<LedgerError> for ApiError {
    fn from(e: LedgerError) -> Self {
        match e {
            LedgerError::NotFound => Self::not_found(),
            LedgerError::Invalid {
                code,
                message,
                param,
            } => Self::invalid(code, message).param(param),
            LedgerError::Db(e) => {
                tracing::error!(error = %e, "ledger error");
                Self::internal()
            }
        }
    }
}

impl From<CategorizeError> for ApiError {
    fn from(e: CategorizeError) -> Self {
        match e {
            CategorizeError::NotFound => Self::not_found(),
            CategorizeError::InvalidRule(e) => {
                Self::invalid("rule_invalid", e.to_string()).param("match")
            }
            CategorizeError::TooManyRules => Self::invalid(
                "too_many_rules",
                "You have the most rules SaveSpace allows. Delete one first.",
            ),
            CategorizeError::Db(e) => {
                tracing::error!(error = %e, "categorize error");
                Self::internal()
            }
        }
    }
}

/// `absent` is `None`, `null` is `Some(None)`.
#[expect(clippy::option_option, reason = "PATCH: absent and null differ")]
fn double<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

fn decode(
    prefix: &str,
    value: &str,
    code: &'static str,
    param: &'static str,
) -> Result<uuid::Uuid, ApiError> {
    ids::decode(prefix, value)
        .ok_or_else(|| ApiError::invalid(code, format!("{param} is not a valid ID.")).param(param))
}

fn category_id(value: &str) -> Result<uuid::Uuid, ApiError> {
    decode(ids::CATEGORY, value, "category_invalid", "category_id")
}

fn transaction_id(value: &str) -> Result<uuid::Uuid, ApiError> {
    ids::decode(ids::TRANSACTION, value).ok_or_else(ApiError::not_found)
}

/// One part of a split.
#[derive(Debug, Serialize, ToSchema)]
pub struct Split {
    /// The split ID (`spl_…`).
    pub id: String,
    /// The category ID (`cat_…`).
    pub category_id: String,
    /// Same sign as the transaction.
    pub amount_minor: i64,
}

/// A transaction with its category and the person's edits.
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
    /// The bank description.
    pub name: String,
    /// The merchant ID (`mer_…`), if SaveSpace found one.
    pub merchant_id: Option<String>,
    /// The merchant name to show.
    pub merchant_name: Option<String>,
    /// True while pending.
    pub pending: bool,
    /// The category ID (`cat_…`).
    pub category_id: Option<String>,
    /// Where the category came from: `person`, `rule`, or `provider`.
    pub category_source: Option<String>,
    /// The person's note.
    pub note: Option<String>,
    /// Hidden from lists, budgets, and insights.
    pub hidden: bool,
    /// The parts, if split. Empty if not.
    pub splits: Vec<Split>,
}

impl From<domain_ledger::Transaction> for Transaction {
    fn from(t: domain_ledger::Transaction) -> Self {
        Self {
            id: ids::encode(ids::TRANSACTION, t.id),
            account_id: ids::encode(ids::ACCOUNT, t.account_id),
            amount_minor: t.amount_minor,
            currency: t.currency,
            date: t.date,
            name: t.name,
            merchant_id: t.merchant_id.map(|m| ids::encode(ids::MERCHANT, m)),
            merchant_name: t.merchant_name,
            pending: t.pending,
            category_id: t.category_id.map(|c| ids::encode(ids::CATEGORY, c)),
            category_source: t.category_source,
            note: t.note,
            hidden: t.hidden,
            splits: t
                .splits
                .into_iter()
                .map(|s| Split {
                    id: ids::encode(ids::SPLIT, s.id),
                    category_id: ids::encode(ids::CATEGORY, s.category_id),
                    amount_minor: s.amount_minor,
                })
                .collect(),
        }
    }
}

/// A page of transactions (§11.5).
#[derive(Debug, Serialize, ToSchema)]
pub struct TransactionList {
    /// Newest first.
    pub data: Vec<Transaction>,
    /// True if more transactions follow. Pass the last ID as `starting_after`.
    pub has_more: bool,
}

/// Filters and paging for the transaction list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TransactionQuery {
    /// 1 to 100. The default is 50.
    pub limit: Option<u16>,
    /// The last transaction ID (`txn_…`) of the previous page.
    pub starting_after: Option<String>,
    /// One account (`acct_…`).
    pub account_id: Option<String>,
    /// A category (`cat_…`), or `none` for transactions without a category.
    pub category: Option<String>,
    /// On or after this date (`YYYY-MM-DD`).
    pub from: Option<String>,
    /// On or before this date (`YYYY-MM-DD`).
    pub to: Option<String>,
    /// Text in the description, merchant, or note.
    pub q: Option<String>,
    /// Also list hidden transactions. The default is false.
    pub include_hidden: Option<bool>,
}

fn date(value: Option<&str>, param: &'static str) -> Result<Option<jiff::civil::Date>, ApiError> {
    value
        .map(|v| {
            v.parse().map_err(|_| {
                ApiError::invalid("date_invalid", format!("{param} must be YYYY-MM-DD."))
                    .param(param)
            })
        })
        .transpose()
}

impl TransactionQuery {
    fn filter(&self) -> Result<TransactionFilter, ApiError> {
        let category = match self.category.as_deref() {
            None => CategoryFilter::Any,
            Some("none") => CategoryFilter::Uncategorized,
            Some(c) => {
                CategoryFilter::Id(decode(ids::CATEGORY, c, "category_invalid", "category")?)
            }
        };
        if self.q.as_deref().is_some_and(|q| q.chars().count() > 100) {
            return Err(ApiError::invalid("q_invalid", "Use up to 100 characters.").param("q"));
        }
        Ok(TransactionFilter {
            account_id: self
                .account_id
                .as_deref()
                .map(|a| decode(ids::ACCOUNT, a, "account_invalid", "account_id"))
                .transpose()?,
            category,
            from: date(self.from.as_deref(), "from")?,
            to: date(self.to.as_deref(), "to")?,
            search: self.q.clone(),
            include_hidden: self.include_hidden.unwrap_or(false),
        })
    }
}

/// Lists transactions, newest first, with filters and search.
#[utoipa::path(get, path = "/transactions", tag = "transactions",
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
    let filter = query.filter()?;
    let starting_after = query
        .starting_after
        .as_deref()
        .map(|s| decode(ids::TRANSACTION, s, "cursor_invalid", "starting_after"))
        .transpose()?;
    // Ask for one more to know whether more follow.
    let mut rows = state
        .ledger
        .list_transactions(
            auth.session.household_id,
            &filter,
            limit + 1,
            starting_after,
        )
        .await?;
    let has_more = rows.len() > usize::from(limit);
    rows.truncate(usize::from(limit));
    Ok(Json(TransactionList {
        data: rows.into_iter().map(Transaction::from).collect(),
        has_more,
    }))
}

/// Gets one transaction.
#[utoipa::path(get, path = "/transactions/{id}", tag = "transactions",
    params(("id" = String, Path, description = "The transaction ID (`txn_…`).")),
    responses((status = 200, body = Transaction), (status = "4XX", body = ErrorBody)))]
async fn get_transaction(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Result<Json<Transaction>, ApiError> {
    let t = state
        .ledger
        .get_transaction(auth.session.household_id, transaction_id(&id)?)
        .await?;
    Ok(Json(t.into()))
}

/// The person's edits. A field that is not in the body does not change.
/// `null` clears it.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateTransactionRequest {
    /// The category ID (`cat_…`). `null` gives the automatic category back.
    #[serde(default, deserialize_with = "double")]
    #[schema(value_type = Option<String>, nullable)]
    pub category_id: Option<Option<String>>,
    /// The merchant name to show, up to 100 characters.
    #[serde(default, deserialize_with = "double")]
    #[schema(value_type = Option<String>, nullable)]
    pub merchant_name: Option<Option<String>>,
    /// A note, up to 500 characters.
    #[serde(default, deserialize_with = "double")]
    #[schema(value_type = Option<String>, nullable)]
    pub note: Option<Option<String>>,
    /// Hide from lists, budgets, and insights.
    pub hidden: Option<bool>,
}

/// Edits the category, merchant name, note, or hidden state.
#[utoipa::path(patch, path = "/transactions/{id}", tag = "transactions",
    params(("id" = String, Path, description = "The transaction ID (`txn_…`).")),
    request_body = UpdateTransactionRequest,
    responses((status = 200, body = Transaction), (status = "4XX", body = ErrorBody)))]
async fn update_transaction(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<UpdateTransactionRequest>,
) -> Result<Json<Transaction>, ApiError> {
    let id = transaction_id(&id)?;
    let patch = TransactionPatch {
        category_id: body
            .category_id
            .map(|c| c.as_deref().map(category_id).transpose())
            .transpose()?,
        merchant_name: body.merchant_name,
        note: body.note,
        hidden: body.hidden,
    };
    let t = state
        .ledger
        .update_transaction(auth.session.household_id, id, patch)
        .await?;
    Ok(Json(t.into()))
}

/// One new split part.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SplitInput {
    /// The category ID (`cat_…`).
    pub category_id: String,
    /// Same sign as the transaction, not zero.
    pub amount_minor: i64,
}

/// The split parts. They must add up to the transaction amount.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SplitsRequest {
    /// 2 to 10 parts, or none to remove the split.
    pub splits: Vec<SplitInput>,
}

/// Replaces the split of a posted transaction.
#[utoipa::path(put, path = "/transactions/{id}/splits", tag = "transactions",
    params(("id" = String, Path, description = "The transaction ID (`txn_…`).")),
    request_body = SplitsRequest,
    responses((status = 200, body = Transaction), (status = "4XX", body = ErrorBody)))]
async fn set_splits(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<SplitsRequest>,
) -> Result<Json<Transaction>, ApiError> {
    let id = transaction_id(&id)?;
    if body.splits.len() > domain_ledger::MAX_SPLITS {
        return Err(ApiError::invalid("splits_count", "Use 2 to 10 parts.").param("splits"));
    }
    let parts = body
        .splits
        .iter()
        .map(|p| {
            Ok(NewSplit {
                category_id: category_id(&p.category_id)?,
                amount_minor: p.amount_minor,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let t = state
        .ledger
        .set_splits(auth.session.household_id, id, &parts)
        .await?;
    Ok(Json(t.into()))
}

/// A category.
#[derive(Debug, Serialize, ToSchema)]
pub struct Category {
    /// The category ID (`cat_…`).
    pub id: String,
    /// The name.
    pub name: String,
    /// `fixed`, `flex`, `non_monthly`, `income`, or `transfer`.
    pub kind: String,
    /// Health, giving, and similar. SaveSpace is more careful with these.
    pub sensitive: bool,
}

/// The categories, in display order.
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryList {
    /// The categories.
    pub data: Vec<Category>,
}

/// Lists categories.
#[utoipa::path(get, path = "/categories", tag = "transactions",
    responses((status = 200, body = CategoryList), (status = 401, body = ErrorBody)))]
async fn list_categories(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<CategoryList>, ApiError> {
    let data = state
        .categorize
        .list_categories(auth.session.household_id)
        .await?
        .into_iter()
        .map(|c| Category {
            id: ids::encode(ids::CATEGORY, c.id),
            name: c.name,
            kind: c.kind,
            sensitive: c.sensitive,
        })
        .collect();
    Ok(Json(CategoryList { data }))
}

/// What a rule matches. Set exactly one of `merchant_id` and
/// `name_contains`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleMatchBody {
    /// A merchant ID (`mer_…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merchant_id: Option<String>,
    /// Text in the description or merchant name, 2 to 100 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    /// `in` (money in) or `out` (money out). Absent means both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub direction: Option<Direction>,
}

/// A categorization rule.
#[derive(Debug, Serialize, ToSchema)]
pub struct Rule {
    /// The rule ID (`rule_…`).
    pub id: String,
    /// What it matches.
    #[serde(rename = "match")]
    pub matcher: RuleMatchBody,
    /// The category it sets (`cat_…`).
    pub category_id: String,
    /// The merchant name, for a merchant rule.
    pub merchant_name: Option<String>,
    /// How many transactions it categorizes now, without those where the
    /// person set the category.
    pub transactions: i64,
    /// When it was made (RFC 3339).
    #[schema(value_type = String, format = DateTime)]
    pub created_at: jiff::Timestamp,
}

impl From<domain_categorize::Rule> for Rule {
    fn from(r: domain_categorize::Rule) -> Self {
        Self {
            id: ids::encode(ids::RULE, r.id),
            matcher: RuleMatchBody {
                merchant_id: r.matcher.merchant_id.map(|m| ids::encode(ids::MERCHANT, m)),
                name_contains: r.matcher.name_contains,
                direction: r.matcher.direction,
            },
            category_id: ids::encode(ids::CATEGORY, r.category_id),
            merchant_name: r.merchant_name,
            transactions: r.transactions,
            created_at: r.created_at,
        }
    }
}

/// The rules, highest priority first.
#[derive(Debug, Serialize, ToSchema)]
pub struct RuleList {
    /// The rules.
    pub data: Vec<Rule>,
}

/// Create a rule.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRuleRequest {
    /// What it matches.
    #[serde(rename = "match")]
    pub matcher: RuleMatchBody,
    /// The category to set (`cat_…`).
    pub category_id: String,
}

/// Lists categorization rules.
#[utoipa::path(get, path = "/rules", tag = "transactions",
    responses((status = 200, body = RuleList), (status = 401, body = ErrorBody)))]
async fn list_rules(State(state): State<AppState>, auth: Auth) -> Result<Json<RuleList>, ApiError> {
    let data = state
        .categorize
        .list_rules(auth.session.household_id)
        .await?
        .into_iter()
        .map(Rule::from)
        .collect();
    Ok(Json(RuleList { data }))
}

/// Creates a rule. It applies to past and future transactions, and replaces
/// a rule with the same match. The person's own edits still win.
#[utoipa::path(post, path = "/rules", tag = "transactions",
    request_body = CreateRuleRequest,
    responses((status = 201, body = Rule), (status = "4XX", body = ErrorBody)))]
async fn create_rule(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<CreateRuleRequest>,
) -> Result<(StatusCode, Json<Rule>), ApiError> {
    let matcher = RuleMatch {
        merchant_id: body
            .matcher
            .merchant_id
            .as_deref()
            .map(|m| decode(ids::MERCHANT, m, "merchant_invalid", "match.merchant_id"))
            .transpose()?,
        name_contains: body.matcher.name_contains,
        direction: body.matcher.direction,
    };
    let rule = state
        .categorize
        .create_rule(
            auth.session.household_id,
            matcher,
            category_id(&body.category_id)?,
        )
        .await
        .map_err(|e| match e {
            CategorizeError::NotFound => ApiError::invalid(
                "rule_target_invalid",
                "That category or merchant does not exist.",
            ),
            other => other.into(),
        })?;
    Ok((StatusCode::CREATED, Json(rule.into())))
}

/// Deletes a rule. Its transactions get the next matching rule or the
/// automatic category.
#[utoipa::path(delete, path = "/rules/{id}", tag = "transactions",
    params(("id" = String, Path, description = "The rule ID (`rule_…`).")),
    responses((status = 204), (status = "4XX", body = ErrorBody)))]
async fn delete_rule(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = ids::decode(ids::RULE, &id).ok_or_else(ApiError::not_found)?;
    state
        .categorize
        .delete_rule(auth.session.household_id, id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
