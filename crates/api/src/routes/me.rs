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
        .routes(routes!(get_me, update_me))
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
    /// The household currency: `USD` or `CAD`. Budgets and insights count
    /// transactions in this currency.
    pub currency: String,
    /// Calm mode: hide exact balances until the person taps.
    pub calm_mode: bool,
    /// Notification settings.
    pub notifications: NotificationSettings,
    /// Step-up actions are allowed until this time (RFC 3339). A time in the
    /// past means the person must confirm with a passkey first.
    #[schema(value_type = String, format = DateTime)]
    pub step_up_until: jiff::Timestamp,
    /// Registered passkeys.
    pub passkeys: Vec<Passkey>,
    /// Unused recovery codes.
    pub recovery_codes_left: i64,
}

/// Notification settings (design doc §10.4).
#[derive(Debug, Serialize, ToSchema)]
pub struct NotificationSettings {
    /// Quiet hours start, `HH:MM` local.
    pub quiet_start: String,
    /// Quiet hours end, `HH:MM` local.
    pub quiet_end: String,
    /// Notifications a day, 0 to 5. Concerns always send.
    pub daily_limit: i16,
    /// Insight kinds not sent as notifications: `concern`, `anticipation`,
    /// or `awareness`.
    pub muted_kinds: Vec<String>,
}

fn hh_mm(t: jiff::civil::Time) -> String {
    format!("{:02}:{:02}", t.hour(), t.minute())
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
        currency: me.currency,
        calm_mode: me.calm_mode,
        notifications: NotificationSettings {
            quiet_start: hh_mm(me.notifications.quiet_start),
            quiet_end: hh_mm(me.notifications.quiet_end),
            daily_limit: me.notifications.daily_limit,
            muted_kinds: me.notifications.muted_kinds,
        },
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

/// Settings. A field that is not in the body does not change.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateMeRequest {
    /// Calm mode: hide exact balances until the person taps.
    pub calm_mode: Option<bool>,
    /// An IANA time zone, for example `America/Chicago`.
    pub time_zone: Option<String>,
    /// Quiet hours start, `HH:MM`.
    pub quiet_start: Option<String>,
    /// Quiet hours end, `HH:MM`. Equal to the start turns quiet hours off.
    pub quiet_end: Option<String>,
    /// Notifications a day, 0 to 5. Concerns always send.
    pub notify_daily_limit: Option<i16>,
    /// Insight kinds not to send: `concern`, `anticipation`, `awareness`.
    pub notify_muted_kinds: Option<Vec<String>>,
    /// The household currency: `USD` or `CAD`.
    pub currency: Option<String>,
}

fn parse_time(value: &str, param: &'static str) -> Result<jiff::civil::Time, ApiError> {
    let bad = || ApiError::invalid("time_invalid", format!("{param} must be HH:MM.")).param(param);
    let (h, m) = value.split_once(':').ok_or_else(bad)?;
    if h.len() != 2 || m.len() != 2 {
        return Err(bad());
    }
    let (h, m): (i8, i8) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    jiff::civil::Time::new(h, m, 0, 0).map_err(|_| bad())
}

impl UpdateMeRequest {
    fn patch(self) -> Result<domain_identity::SettingsPatch, ApiError> {
        if let Some(zone) = &self.time_zone
            && (zone.len() > 64 || jiff::tz::TimeZone::get(zone).is_err())
        {
            return Err(
                ApiError::invalid("time_zone_invalid", "That time zone is not known.")
                    .param("time_zone"),
            );
        }
        if self
            .notify_daily_limit
            .is_some_and(|n| !(0..=5).contains(&n))
        {
            return Err(
                ApiError::invalid("limit_invalid", "Use 0 to 5.").param("notify_daily_limit")
            );
        }
        if let Some(kinds) = &self.notify_muted_kinds
            && (kinds.len() > 3
                || kinds
                    .iter()
                    .any(|k| !domain_notify::service::NOTIFY_KINDS.contains(&k.as_str())))
        {
            return Err(ApiError::invalid(
                "kinds_invalid",
                "Use concern, anticipation, or awareness.",
            )
            .param("notify_muted_kinds"));
        }
        if let Some(currency) = &self.currency
            && !domain_identity::HOUSEHOLD_CURRENCIES.contains(&currency.as_str())
        {
            return Err(ApiError::invalid("currency_invalid", "Use USD or CAD.").param("currency"));
        }
        Ok(domain_identity::SettingsPatch {
            currency: self.currency,
            calm_mode: self.calm_mode,
            time_zone: self.time_zone,
            quiet_start: self
                .quiet_start
                .as_deref()
                .map(|t| parse_time(t, "quiet_start"))
                .transpose()?,
            quiet_end: self
                .quiet_end
                .as_deref()
                .map(|t| parse_time(t, "quiet_end"))
                .transpose()?,
            daily_limit: self.notify_daily_limit,
            muted_kinds: self.notify_muted_kinds.map(|mut k| {
                k.sort();
                k.dedup();
                k
            }),
        })
    }
}

/// Changes settings.
#[utoipa::path(patch, path = "/me", tag = "me",
    request_body = UpdateMeRequest,
    responses((status = 200, body = Me), (status = "4XX", body = ErrorBody)))]
async fn update_me(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<UpdateMeRequest>,
) -> Result<Json<Me>, ApiError> {
    state
        .identity
        .update_settings(&auth.session, &body.patch()?)
        .await?;
    get_me(State(state), auth).await
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
