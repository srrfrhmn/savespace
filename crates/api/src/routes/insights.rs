//! Insights, feedback, the weekly reflection, and check-ins (design doc §7,
//! §6.1, §11.6).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use domain_insights::service::{Feedback, Reason, View};
use domain_insights::{InsightError, StoredInsight};
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
        .routes(routes!(list_insights))
        .routes(routes!(send_feedback))
        .routes(routes!(list_mutes))
        .routes(routes!(unmute))
        .routes(routes!(get_reflection))
        .routes(routes!(create_check_in, list_check_ins))
}

impl From<InsightError> for ApiError {
    fn from(e: InsightError) -> Self {
        match e {
            InsightError::NotFound => Self::not_found(),
            InsightError::NotesUnavailable => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorType::ApiError,
                "notes_unavailable",
                "Notes are not available on this server. Save the check-in without a note.",
            ),
            other => {
                tracing::error!(error = %other, "insight error");
                Self::internal()
            }
        }
    }
}

/// One fact behind an insight.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct InsightFact {
    /// A stable key, for example `wtd_spend_minor`.
    pub key: String,
    /// A label for the person.
    pub label: String,
    /// The value as the text shows it.
    pub value: String,
}

/// An insight.
#[derive(Debug, Serialize, ToSchema)]
pub struct Insight {
    /// The insight ID (`ins_…`).
    pub id: String,
    /// The rule, for example `spend.week_below_usual`.
    pub rule_id: String,
    /// `reassurance`, `awareness`, `anticipation`, `suggestion`, or `concern`.
    pub kind: String,
    /// The text.
    pub text: String,
    /// The facts the text uses ("Why am I seeing this?").
    pub facts: Vec<InsightFact>,
    /// The transaction (`txn_…`), for insights about one transaction.
    pub transaction_id: Option<String>,
    /// When it was made (RFC 3339).
    #[schema(value_type = String, format = DateTime)]
    pub created_at: jiff::Timestamp,
}

impl From<StoredInsight> for Insight {
    fn from(i: StoredInsight) -> Self {
        Self {
            id: ids::encode(ids::INSIGHT, i.id),
            rule_id: i.rule_id,
            kind: i.kind.as_str().to_owned(),
            text: i.text,
            facts: serde_json::from_value(i.facts).unwrap_or_default(),
            transaction_id: i.transaction_id.map(|t| ids::encode(ids::TRANSACTION, t)),
            created_at: i.created_at,
        }
    }
}

/// Insights for a view.
#[derive(Debug, Serialize, ToSchema)]
pub struct InsightList {
    /// At most 3, besides extra concerns (§7.1). Concerns first.
    pub data: Vec<Insight>,
}

/// Which view.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct InsightQuery {
    /// `home` (the default), or a transaction ID (`txn_…`) for the insights
    /// about one transaction.
    pub transaction_id: Option<String>,
}

/// Lists the current insights for a view.
#[utoipa::path(get, path = "/insights", tag = "insights",
    params(InsightQuery),
    responses((status = 200, body = InsightList), (status = "4XX", body = ErrorBody)))]
async fn list_insights(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<InsightQuery>,
) -> Result<Json<InsightList>, ApiError> {
    let view = match query.transaction_id.as_deref() {
        None => View::Home,
        Some(t) => View::Transaction(ids::decode(ids::TRANSACTION, t).ok_or_else(|| {
            ApiError::invalid("transaction_invalid", "transaction_id is not valid.")
                .param("transaction_id")
        })?),
    };
    let data = state
        .insights
        .list(auth.session.household_id, view)
        .await?
        .into_iter()
        .map(Insight::from)
        .collect();
    Ok(Json(InsightList { data }))
}

/// "This is wrong" or "Not helpful".
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackRequest {
    /// `wrong` or `not_helpful`.
    pub kind: String,
    /// For `wrong`: `amount`, `merchant`, `category`, `date`, or `other`.
    pub reason: Option<String>,
}

/// The result of feedback.
#[derive(Debug, Serialize, ToSchema)]
pub struct FeedbackResult {
    /// True if this rule is now muted (3 "Not helpful" in 30 days).
    pub muted: bool,
}

/// Sends feedback. The insight is hidden at once.
#[utoipa::path(post, path = "/insights/{id}/feedback", tag = "insights",
    params(("id" = String, Path, description = "The insight ID (`ins_…`).")),
    request_body = FeedbackRequest,
    responses((status = 200, body = FeedbackResult), (status = "4XX", body = ErrorBody)))]
async fn send_feedback(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<FeedbackRequest>,
) -> Result<Json<FeedbackResult>, ApiError> {
    let id = ids::decode(ids::INSIGHT, &id).ok_or_else(ApiError::not_found)?;
    let feedback = match body.kind.as_str() {
        "not_helpful" => Feedback::NotHelpful,
        "wrong" => Feedback::Wrong(
            body.reason
                .as_deref()
                .map_or(Some(Reason::Other), Reason::parse)
                .ok_or_else(|| {
                    ApiError::invalid(
                        "reason_invalid",
                        "reason must be amount, merchant, category, date, or other.",
                    )
                    .param("reason")
                })?,
        ),
        _ => {
            return Err(ApiError::invalid(
                "feedback_invalid",
                "kind must be wrong or not_helpful.",
            )
            .param("kind"));
        }
    };
    let muted = state
        .insights
        .feedback(auth.session.household_id, id, feedback)
        .await?;
    Ok(Json(FeedbackResult { muted }))
}

/// Muted rules.
#[derive(Debug, Serialize, ToSchema)]
pub struct MuteList {
    /// Rule IDs.
    pub data: Vec<String>,
}

/// Lists the rules the person muted.
#[utoipa::path(get, path = "/insights/mutes", tag = "insights",
    responses((status = 200, body = MuteList), (status = 401, body = ErrorBody)))]
async fn list_mutes(State(state): State<AppState>, auth: Auth) -> Result<Json<MuteList>, ApiError> {
    let data = state.insights.mutes(auth.session.household_id).await?;
    Ok(Json(MuteList { data }))
}

/// Unmutes a rule.
#[utoipa::path(delete, path = "/insights/mutes/{rule_id}", tag = "insights",
    params(("rule_id" = String, Path, description = "The rule ID.")),
    responses((status = 204), (status = "4XX", body = ErrorBody)))]
async fn unmute(
    State(state): State<AppState>,
    auth: Auth,
    Path(rule_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if rule_id.len() > 64 {
        return Err(ApiError::not_found());
    }
    state
        .insights
        .unmute(auth.session.household_id, &rule_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// One reflection card.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReflectionCard {
    /// `positive`, `neutral`, or `higher`. The last card is never `higher`.
    pub tone: String,
    /// The text.
    pub text: String,
}

/// The weekly reflection.
#[derive(Debug, Serialize, ToSchema)]
pub struct Reflection {
    /// The Monday of the week (`YYYY-MM-DD`).
    #[schema(value_type = String, format = Date)]
    pub week_start: jiff::civil::Date,
    /// True while the week is not over.
    pub partial: bool,
    /// At most 5 cards. The last one is a positive or neutral fact.
    pub cards: Vec<ReflectionCard>,
}

/// Gets the weekly reflection for the week that contains a date.
#[utoipa::path(get, path = "/reflections/{week}", tag = "insights",
    params(("week" = String, Path, description = "Any date in the week, `YYYY-MM-DD`.")),
    responses((status = 200, body = Reflection), (status = "4XX", body = ErrorBody)))]
async fn get_reflection(
    State(state): State<AppState>,
    auth: Auth,
    Path(week): Path<String>,
) -> Result<Json<Reflection>, ApiError> {
    let date: jiff::civil::Date = week.parse().map_err(|_| {
        ApiError::invalid("week_invalid", "The week must be a date, YYYY-MM-DD.").param("week")
    })?;
    if !(2000..=2100).contains(&date.year()) {
        return Err(ApiError::invalid("week_invalid", "The week is out of range.").param("week"));
    }
    let (week_start, partial, cards) = state
        .insights
        .reflection(auth.session.household_id, date)
        .await?;
    Ok(Json(Reflection {
        week_start,
        partial,
        cards: cards
            .into_iter()
            .map(|c| ReflectionCard {
                tone: match c.tone {
                    domain_insights::reflection::Tone::Positive => "positive",
                    domain_insights::reflection::Tone::Neutral => "neutral",
                    domain_insights::reflection::Tone::Higher => "higher",
                }
                .to_owned(),
                text: c.text,
            })
            .collect(),
    }))
}

/// A check-in.
#[derive(Debug, Serialize, ToSchema)]
pub struct CheckIn {
    /// The check-in ID (`chk_…`).
    pub id: String,
    /// 1 to 5.
    pub score: i16,
    /// The note.
    pub note: Option<String>,
    /// The day (`YYYY-MM-DD`) in the person's time zone.
    #[schema(value_type = String, format = Date)]
    pub date: jiff::civil::Date,
    /// When it was saved (RFC 3339).
    #[schema(value_type = String, format = DateTime)]
    pub created_at: jiff::Timestamp,
}

impl From<domain_insights::checkin::CheckIn> for CheckIn {
    fn from(c: domain_insights::checkin::CheckIn) -> Self {
        Self {
            id: ids::encode(ids::CHECK_IN, c.id),
            score: c.score,
            note: c.note,
            date: c.date,
            created_at: c.created_at,
        }
    }
}

/// "How do you feel about money today?"
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCheckInRequest {
    /// 1 to 5.
    pub score: i16,
    /// An optional note, up to 500 characters. Stored encrypted.
    pub note: Option<String>,
}

/// Saves a check-in.
#[utoipa::path(post, path = "/check_ins", tag = "insights",
    request_body = CreateCheckInRequest,
    responses((status = 201, body = CheckIn), (status = "4XX", body = ErrorBody),
              (status = 503, body = ErrorBody)))]
async fn create_check_in(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<CreateCheckInRequest>,
) -> Result<(StatusCode, Json<CheckIn>), ApiError> {
    if !(1..=5).contains(&body.score) {
        return Err(ApiError::invalid("score_invalid", "score must be 1 to 5.").param("score"));
    }
    let note = body
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    if note.is_some_and(|n| {
        n.chars().count() > domain_insights::checkin::MAX_NOTE
            || n.chars().any(|c| c.is_control() && c != '\n')
    }) {
        return Err(ApiError::invalid("note_invalid", "Use up to 500 characters.").param("note"));
    }
    let check_in = state
        .check_ins
        .create(
            auth.session.household_id,
            auth.session.user_id,
            body.score,
            note,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(check_in.into())))
}

/// Check-ins, newest first.
#[derive(Debug, Serialize, ToSchema)]
pub struct CheckInList {
    /// The latest 30.
    pub data: Vec<CheckIn>,
}

/// Lists the person's latest check-ins.
#[utoipa::path(get, path = "/check_ins", tag = "insights",
    responses((status = 200, body = CheckInList), (status = 401, body = ErrorBody)))]
async fn list_check_ins(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<CheckInList>, ApiError> {
    let data = state
        .check_ins
        .list(auth.session.household_id, auth.session.user_id, 30)
        .await?
        .into_iter()
        .map(CheckIn::from)
        .collect();
    Ok(Json(CheckInList { data }))
}
