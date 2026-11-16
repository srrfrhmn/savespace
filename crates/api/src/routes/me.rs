//! The signed-in person: profile, passkeys, and recovery codes.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use webauthn_rs::prelude::RegisterPublicKeyCredential;

use crate::error::{ApiError, ErrorBody, Json};
use crate::routes::auth::{Challenge, RecoveryCodes, challenge_id, clean_name};
use crate::session::Auth;
use crate::{AppState, ids};

/// Routes that need a session.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_me))
        .routes(routes!(add_passkey_start))
        .routes(routes!(add_passkey_finish))
        .routes(routes!(delete_passkey))
        .routes(routes!(regenerate_recovery_codes))
}

/// The signed-in person.
#[derive(Debug, Serialize, ToSchema)]
pub struct Me {
    /// The user ID (`usr_…`).
    pub id: String,
    /// The household ID (`hh_…`).
    pub household_id: String,
    /// The display name.
    pub display_name: String,
    /// The IANA time zone, for example `America/New_York`.
    pub time_zone: String,
    /// Calm mode: hide exact balances until the person taps.
    pub calm_mode: bool,
    /// Step-up actions are allowed until this time (RFC 3339). A time in the
    /// past means the person must confirm with a passkey first.
    #[schema(value_type = String, format = DateTime)]
    pub step_up_until: jiff::Timestamp,
    /// Registered passkeys.
    pub passkeys: Vec<Passkey>,
    /// Unused recovery codes.
    pub recovery_codes_left: i64,
}

/// A registered passkey.
#[derive(Debug, Serialize, ToSchema)]
pub struct Passkey {
    /// The passkey ID (`pk_…`).
    pub id: String,
    /// The name the person gave it.
    pub name: String,
    /// When it was added (RFC 3339).
    #[schema(value_type = String, format = DateTime)]
    pub created_at: jiff::Timestamp,
    /// The last use (RFC 3339), if any.
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_used_at: Option<jiff::Timestamp>,
}

/// Finish adding a passkey.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AddPasskeyRequest {
    /// The ID from the start response.
    pub challenge_id: String,
    /// The result of `navigator.credentials.create()`, in JSON form.
    #[schema(value_type = Object)]
    pub credential: RegisterPublicKeyCredential,
    /// A name for this passkey. 1 to 100 characters.
    pub name: String,
}

/// A new passkey.
#[derive(Debug, Serialize, ToSchema)]
pub struct PasskeyCreated {
    /// The passkey ID (`pk_…`).
    pub id: String,
}

/// Gets the signed-in person.
#[utoipa::path(get, path = "/me", tag = "me",
    responses((status = 200, body = Me), (status = 401, body = ErrorBody)))]
async fn get_me(State(state): State<AppState>, auth: Auth) -> Result<Json<Me>, ApiError> {
    let me = state.identity.me(&auth.session).await?;
    Ok(Json(Me {
        id: ids::encode(ids::USER, me.user_id),
        household_id: ids::encode(ids::HOUSEHOLD, me.household_id),
        display_name: me.display_name,
        time_zone: me.time_zone,
        calm_mode: me.calm_mode,
        step_up_until: me.step_up_until,
        passkeys: me
            .passkeys
            .into_iter()
            .map(|p| Passkey {
                id: ids::encode(ids::PASSKEY, p.id),
                name: p.name,
                created_at: p.created_at,
                last_used_at: p.last_used_at,
            })
            .collect(),
        recovery_codes_left: me.recovery_codes_left,
    }))
}

/// Starts adding a passkey. Needs step-up.
#[utoipa::path(post, path = "/me/passkeys/start", tag = "me",
    responses((status = 200, body = Challenge), (status = "4XX", body = ErrorBody)))]
async fn add_passkey_start(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<Json<Challenge>, ApiError> {
    auth.require_step_up()?;
    Ok(Json(
        state
            .identity
            .start_add_passkey(&auth.session)
            .await?
            .into(),
    ))
}

/// Finishes adding a passkey. Needs step-up.
#[utoipa::path(post, path = "/me/passkeys/finish", tag = "me",
    request_body = AddPasskeyRequest,
    responses((status = 201, body = PasskeyCreated), (status = "4XX", body = ErrorBody)))]
async fn add_passkey_finish(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<AddPasskeyRequest>,
) -> Result<(StatusCode, Json<PasskeyCreated>), ApiError> {
    auth.require_step_up()?;
    let name = clean_name("name", &body.name)?;
    let id = challenge_id(&body.challenge_id)?;
    let passkey_id = state
        .identity
        .finish_add_passkey(&auth.session, id, &body.credential, &name, &auth.ctx)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(PasskeyCreated {
            id: ids::encode(ids::PASSKEY, passkey_id),
        }),
    ))
}

/// Deletes a passkey. The last passkey cannot be deleted. Needs step-up.
#[utoipa::path(delete, path = "/me/passkeys/{id}", tag = "me",
    params(("id" = String, Path, description = "The passkey ID (`pk_…`).")),
    responses((status = 204), (status = "4XX", body = ErrorBody)))]
async fn delete_passkey(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    auth.require_step_up()?;
    let id = ids::decode(ids::PASSKEY, &id).ok_or_else(ApiError::not_found)?;
    state
        .identity
        .delete_passkey(&auth.session, id, &auth.ctx)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Replaces the recovery codes with a new set. Needs step-up.
#[utoipa::path(post, path = "/me/recovery_codes", tag = "me",
    responses((status = 201, body = RecoveryCodes), (status = "4XX", body = ErrorBody)))]
async fn regenerate_recovery_codes(
    State(state): State<AppState>,
    auth: Auth,
) -> Result<(StatusCode, Json<RecoveryCodes>), ApiError> {
    auth.require_step_up()?;
    let recovery_codes = state
        .identity
        .regenerate_recovery_codes(&auth.session, &auth.ctx)
        .await?;
    Ok((StatusCode::CREATED, Json(RecoveryCodes { recovery_codes })))
}
