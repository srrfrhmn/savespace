//! Sign-in routes: owner setup, passkey sign-in, recovery codes, sign-out,
//! and the step-up check (design doc §12.1, D12).

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};

use crate::error::{ApiError, ErrorBody, Json};
use crate::session::{Auth, Ctx, clear_cookie, set_cookie};
use crate::{AppState, ids};

/// Routes that work without a session.
pub fn public_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(setup_status))
        .routes(routes!(setup_start))
        .routes(routes!(setup_finish))
        .routes(routes!(login_start))
        .routes(routes!(login_finish))
        .routes(routes!(recover))
        .routes(routes!(logout))
}

/// Routes that need a session.
pub fn session_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(verify_start))
        .routes(routes!(verify_finish))
}

/// Whether owner setup can start.
#[derive(Debug, Serialize, ToSchema)]
pub struct SetupStatus {
    /// True if the server has a setup code and no owner exists yet.
    pub setup_open: bool,
}

/// Start owner setup.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetupStartRequest {
    /// The one-time setup code from Secret Manager (`owner-setup-code`).
    pub setup_code: String,
    /// The name to show in the app. 1 to 100 characters.
    pub display_name: String,
}

/// A WebAuthn challenge.
#[derive(Debug, Serialize, ToSchema)]
pub struct Challenge {
    /// Send this ID back with the authenticator response. Valid for 5 minutes.
    pub challenge_id: String,
    /// Options for `navigator.credentials.create()` or `.get()`, in the
    /// WebAuthn JSON form (binary fields are base64url).
    #[schema(value_type = Object)]
    pub options: Value,
}

impl From<domain_identity::Challenge> for Challenge {
    fn from(c: domain_identity::Challenge) -> Self {
        Self {
            challenge_id: ids::encode(ids::CHALLENGE, c.challenge_id),
            options: c.options,
        }
    }
}

/// Finish owner setup.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetupFinishRequest {
    /// The ID from the start response.
    pub challenge_id: String,
    /// The result of `navigator.credentials.create()`, in JSON form.
    #[schema(value_type = Object)]
    pub credential: RegisterPublicKeyCredential,
    /// A name for this passkey, for example "Laptop". 1 to 100 characters.
    pub passkey_name: String,
}

/// Recovery codes. Shown once. Each code works one time.
#[derive(Debug, Serialize, ToSchema)]
pub struct RecoveryCodes {
    /// Ten codes in the form `XXXX-XXXX-XXXX`.
    pub recovery_codes: Vec<String>,
}

/// Finish a passkey check.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AssertionRequest {
    /// The ID from the start response.
    pub challenge_id: String,
    /// The result of `navigator.credentials.get()`, in JSON form.
    #[schema(value_type = Object)]
    pub credential: PublicKeyCredential,
}

/// Sign in with a recovery code.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RecoverRequest {
    /// A recovery code. Case, spaces, and dashes do not matter.
    pub code: String,
}

/// The result of a step-up check.
#[derive(Debug, Serialize, ToSchema)]
pub struct StepUp {
    /// Step-up actions are allowed until this time (RFC 3339).
    #[schema(value_type = String, format = DateTime)]
    pub step_up_until: jiff::Timestamp,
}

/// Checks a person-supplied name: trimmed, 1 to 100 characters, no control
/// characters.
///
/// # Errors
///
/// Returns `name_invalid` for the field.
pub fn clean_name(field: &'static str, value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    let count = value.chars().count();
    if count == 0 || count > 100 || value.chars().any(char::is_control) {
        return Err(ApiError::invalid(
            "name_invalid",
            format!("{field} must have 1 to 100 characters."),
        )
        .param(field));
    }
    Ok(value.to_owned())
}

/// Parses a challenge ID from a request body.
///
/// # Errors
///
/// Returns `challenge_expired` for an ID that cannot be valid.
pub fn challenge_id(value: &str) -> Result<uuid::Uuid, ApiError> {
    ids::decode(ids::CHALLENGE, value).ok_or_else(|| {
        ApiError::invalid(
            "challenge_expired",
            "The sign-in request expired; start again.",
        )
        .param("challenge_id")
    })
}

fn with_session_cookie(token: &str, response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, set_cookie(token));
    response
}

/// Reports whether owner setup can start.
#[utoipa::path(get, path = "/auth/setup", tag = "auth",
    responses((status = 200, body = SetupStatus)))]
async fn setup_status(State(state): State<AppState>) -> Result<Json<SetupStatus>, ApiError> {
    let status = state.identity.setup_status().await?;
    Ok(Json(SetupStatus {
        setup_open: status.setup_open,
    }))
}

/// Starts owner setup. Checks the setup code and returns passkey options.
#[utoipa::path(post, path = "/auth/setup/start", tag = "auth",
    request_body = SetupStartRequest,
    responses((status = 200, body = Challenge), (status = "4XX", body = ErrorBody)))]
async fn setup_start(
    State(state): State<AppState>,
    Json(body): Json<SetupStartRequest>,
) -> Result<Json<Challenge>, ApiError> {
    let display_name = clean_name("display_name", &body.display_name)?;
    let challenge = state
        .identity
        .start_setup(&body.setup_code, &display_name)
        .await?;
    Ok(Json(challenge.into()))
}

/// Finishes owner setup. Signs the owner in and returns recovery codes.
#[utoipa::path(post, path = "/auth/setup/finish", tag = "auth",
    request_body = SetupFinishRequest,
    responses((status = 201, body = RecoveryCodes), (status = "4XX", body = ErrorBody)))]
async fn setup_finish(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<SetupFinishRequest>,
) -> Result<Response, ApiError> {
    let passkey_name = clean_name("passkey_name", &body.passkey_name)?;
    let id = challenge_id(&body.challenge_id)?;
    let result = state
        .identity
        .finish_setup(id, &body.credential, &passkey_name, &ctx)
        .await?;
    Ok(with_session_cookie(
        &result.session.token,
        (
            StatusCode::CREATED,
            Json(RecoveryCodes {
                recovery_codes: result.recovery_codes,
            }),
        ),
    ))
}

/// Starts passkey sign-in.
#[utoipa::path(post, path = "/auth/login/start", tag = "auth",
    responses((status = 200, body = Challenge)))]
async fn login_start(State(state): State<AppState>) -> Result<Json<Challenge>, ApiError> {
    Ok(Json(state.identity.start_login().await?.into()))
}

/// Finishes passkey sign-in. Sets the session cookie.
#[utoipa::path(post, path = "/auth/login/finish", tag = "auth",
    request_body = AssertionRequest,
    responses((status = 204), (status = "4XX", body = ErrorBody)))]
async fn login_finish(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<AssertionRequest>,
) -> Result<Response, ApiError> {
    let id = challenge_id(&body.challenge_id)?;
    let session = state
        .identity
        .finish_login(id, &body.credential, &ctx)
        .await?;
    Ok(with_session_cookie(&session.token, StatusCode::NO_CONTENT))
}

/// Signs in with a recovery code. The code works once. The new session can
/// add a passkey right away.
#[utoipa::path(post, path = "/auth/recover", tag = "auth",
    request_body = RecoverRequest,
    responses((status = 204), (status = "4XX", body = ErrorBody)))]
async fn recover(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Json(body): Json<RecoverRequest>,
) -> Result<Response, ApiError> {
    let session = state
        .identity
        .login_with_recovery_code(&body.code, &ctx)
        .await?;
    Ok(with_session_cookie(&session.token, StatusCode::NO_CONTENT))
}

/// Signs out. Works without a valid session, so a stale cookie is removed.
#[utoipa::path(post, path = "/auth/logout", tag = "auth",
    responses((status = 204)))]
async fn logout(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    headers: axum::http::HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(token) = crate::session::token_from(&headers)
        && let Some(session) = state.identity.authenticate(token).await?
    {
        state.identity.logout(&session, &ctx).await?;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, clear_cookie());
    Ok(response)
}

/// Starts a passkey check for step-up actions.
#[utoipa::path(post, path = "/auth/verify/start", tag = "auth",
    responses((status = 200, body = Challenge), (status = 401, body = ErrorBody)))]
async fn verify_start(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Challenge>, ApiError> {
    Ok(Json(
        state.identity.start_verify(&auth.session).await?.into(),
    ))
}

/// Finishes a step-up check.
#[utoipa::path(post, path = "/auth/verify/finish", tag = "auth",
    request_body = AssertionRequest,
    responses((status = 200, body = StepUp), (status = "4XX", body = ErrorBody)))]
async fn verify_finish(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<AssertionRequest>,
) -> Result<Json<StepUp>, ApiError> {
    let id = challenge_id(&body.challenge_id)?;
    let session = state
        .identity
        .finish_verify(&auth.session, id, &body.credential, &auth.ctx)
        .await?;
    Ok(Json(StepUp {
        step_up_until: session.step_up_until(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_name_trims_and_limits() {
        assert_eq!(clean_name("n", "  Sam ").ok().as_deref(), Some("Sam"));
        assert!(clean_name("n", "   ").is_err());
        assert!(clean_name("n", &"a".repeat(101)).is_err());
        assert!(clean_name("n", &"é".repeat(100)).is_ok());
        assert!(clean_name("n", "a\u{0007}b").is_err());
    }
}
