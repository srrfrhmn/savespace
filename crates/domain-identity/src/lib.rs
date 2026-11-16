//! `domain-identity`: users, households, sessions, and passkeys.
//!
//! Design doc §12.1 and §12.2, decision D12:
//!
//! - Sign-in uses passkeys (WebAuthn, discoverable credentials).
//! - The first passkey needs a one-time setup code. After the owner exists,
//!   setup is closed.
//! - Ten single-use recovery codes replace an email fallback.
//! - Sessions are random tokens. Only the SHA-256 hash is stored. Limits:
//!   30 days idle, 90 days absolute.
//! - Step-up actions need a passkey check (or recovery code) in the last
//!   5 minutes.

pub mod token;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, Passkey, PasskeyAuthentication, PasskeyRegistration,
    PublicKeyCredential, RegisterPublicKeyCredential, WebauthnError,
};
use webauthn_rs::{Webauthn, WebauthnBuilder};

/// The absolute session limit (design doc §12.1).
pub const SESSION_ABSOLUTE_DAYS: i64 = 90;

/// How long a passkey check counts for step-up actions.
pub const STEP_UP_WINDOW: jiff::SignedDuration = jiff::SignedDuration::from_mins(5);

/// Errors from identity operations.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// Setup is not configured on this server.
    #[error("owner setup is not available")]
    SetupUnavailable,
    /// The setup code does not match.
    #[error("the setup code is not correct")]
    InvalidSetupCode,
    /// An owner already exists.
    #[error("the owner already exists")]
    AlreadySetUp,
    /// The challenge does not exist, expired, or was used.
    #[error("the sign-in request expired; start again")]
    ChallengeExpired,
    /// The authenticator response failed verification.
    #[error("the passkey check failed")]
    PasskeyRejected,
    /// The credential is not registered.
    #[error("this passkey is not registered")]
    UnknownPasskey,
    /// The recovery code is not valid or was used.
    #[error("the recovery code is not valid")]
    InvalidRecoveryCode,
    /// The person tried to delete the last passkey.
    #[error("the last passkey cannot be deleted")]
    LastPasskey,
    /// The resource does not exist in this household.
    #[error("not found")]
    NotFound,
    /// A database error.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// Stored state could not be read. This is a bug.
    #[error("stored state is not valid: {0}")]
    State(#[from] serde_json::Error),
    /// The WebAuthn configuration is not valid.
    #[error("webauthn configuration: {0}")]
    Config(WebauthnError),
}

type PasskeyRow = (
    Uuid,
    String,
    jiff_sqlx::Timestamp,
    Option<jiff_sqlx::Timestamp>,
);

type Result<T, E = IdentityError> = std::result::Result<T, E>;

/// Request details for the audit log and the session record.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// The request ID (`req_…`).
    pub request_id: Option<String>,
    /// The `User-Agent` header, cut to 200 characters.
    pub user_agent: Option<String>,
}

/// A signed-in session.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// The session row ID.
    pub session_id: Uuid,
    /// The household. All tenant queries use it.
    pub household_id: Uuid,
    /// The signed-in person.
    pub user_id: Uuid,
    /// The last passkey or recovery code check.
    pub verified_at: jiff::Timestamp,
}

impl SessionInfo {
    /// The time until which step-up actions are allowed.
    pub fn step_up_until(&self) -> jiff::Timestamp {
        self.verified_at
            .checked_add(STEP_UP_WINDOW)
            .unwrap_or(self.verified_at)
    }

    /// True if a step-up action is allowed at `now`.
    pub fn step_up_valid(&self, now: jiff::Timestamp) -> bool {
        now < self.step_up_until()
    }
}

/// A new session. The token goes to the client once and is never stored.
#[derive(Debug)]
pub struct NewSession {
    /// The session token for the cookie.
    pub token: String,
    /// The session details.
    pub info: SessionInfo,
}

/// The result of owner setup.
#[derive(Debug)]
pub struct SetupResult {
    /// The new session.
    pub session: NewSession,
    /// The recovery codes. Show them once.
    pub recovery_codes: Vec<String>,
}

/// A WebAuthn challenge for the client. `options` goes to
/// `navigator.credentials.create()` or `.get()`.
#[derive(Debug, Serialize)]
pub struct Challenge {
    /// Send this ID back with the authenticator response.
    pub challenge_id: Uuid,
    /// The WebAuthn options (JSON, base64url binary fields).
    pub options: Value,
}

/// Setup state for the welcome screen.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SetupStatus {
    /// True if setup can start: the server has a setup code and no owner exists.
    pub setup_open: bool,
}

/// The signed-in person and the security settings.
#[derive(Debug, Serialize)]
pub struct Me {
    /// The user ID.
    pub user_id: Uuid,
    /// The household ID.
    pub household_id: Uuid,
    /// The display name.
    pub display_name: String,
    /// The IANA time zone.
    pub time_zone: String,
    /// Calm mode (design doc §6.1).
    pub calm_mode: bool,
    /// Step-up actions are allowed until this time.
    pub step_up_until: jiff::Timestamp,
    /// Registered passkeys.
    pub passkeys: Vec<PasskeySummary>,
    /// Unused recovery codes.
    pub recovery_codes_left: i64,
}

/// A registered passkey, without key material.
#[derive(Debug, Serialize)]
pub struct PasskeySummary {
    /// The passkey ID.
    pub id: Uuid,
    /// The name the person gave it.
    pub name: String,
    /// When it was added.
    pub created_at: jiff::Timestamp,
    /// The last sign-in with it.
    pub last_used_at: Option<jiff::Timestamp>,
}

/// Identity configuration.
#[derive(Debug, Clone)]
pub struct IdentityConfig {
    /// The public origin of the web app, for example
    /// `https://savespace-api-…run.app`. The WebAuthn relying party ID is its host.
    pub origin: Url,
    /// The one-time owner setup code. `None` disables setup.
    pub setup_code: Option<String>,
}

/// The identity service.
#[derive(Clone)]
pub struct Identity {
    pool: PgPool,
    webauthn: Arc<Webauthn>,
    setup_code: Option<Arc<str>>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("setup_code", &self.setup_code.as_ref().map(|_| "[set]"))
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SetupState {
    reg: PasskeyRegistration,
    user_id: Uuid,
    household_id: Uuid,
    display_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AddPasskeyState {
    reg: PasskeyRegistration,
}

#[derive(Debug, Serialize, Deserialize)]
struct LoginState {
    auth: DiscoverableAuthentication,
}

#[derive(Debug, Serialize, Deserialize)]
struct VerifyState {
    auth: PasskeyAuthentication,
}

#[derive(Debug, Clone, Copy)]
enum Purpose {
    Setup,
    Login,
    AddPasskey,
    Verify,
}

impl Purpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Login => "login",
            Self::AddPasskey => "add_passkey",
            Self::Verify => "verify",
        }
    }
}

impl Identity {
    /// Creates the service.
    ///
    /// # Errors
    ///
    /// Returns an error if the origin has no host or WebAuthn rejects it.
    pub fn new(pool: PgPool, config: &IdentityConfig) -> Result<Self> {
        let rp_id = config
            .origin
            .host_str()
            .ok_or(IdentityError::Config(WebauthnError::Configuration))?;
        let webauthn = WebauthnBuilder::new(rp_id, &config.origin)
            .map_err(IdentityError::Config)?
            .rp_name("SaveSpace")
            // The Vite dev server runs on another port than the API.
            .allow_any_port(rp_id == "localhost")
            .build()
            .map_err(IdentityError::Config)?;
        Ok(Self {
            pool,
            webauthn: Arc::new(webauthn),
            setup_code: config
                .setup_code
                .as_deref()
                .filter(|c| !c.is_empty())
                .map(Arc::from),
        })
    }

    /// Reports whether owner setup can start.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn setup_status(&self) -> Result<SetupStatus> {
        let setup_open = self.setup_code.is_some() && !self.owner_exists().await?;
        Ok(SetupStatus { setup_open })
    }

    async fn owner_exists(&self) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT app.owner_exists()")
            .fetch_one(&self.pool)
            .await?)
    }

    fn check_setup_code(&self, code: &str) -> Result<()> {
        let expected = self
            .setup_code
            .as_deref()
            .ok_or(IdentityError::SetupUnavailable)?;
        if bool::from(expected.trim().as_bytes().ct_eq(code.trim().as_bytes())) {
            Ok(())
        } else {
            Err(IdentityError::InvalidSetupCode)
        }
    }

    /// Starts owner setup: checks the setup code and returns passkey
    /// creation options.
    ///
    /// # Errors
    ///
    /// [`IdentityError::SetupUnavailable`], [`IdentityError::InvalidSetupCode`],
    /// [`IdentityError::AlreadySetUp`], or a database error.
    pub async fn start_setup(&self, setup_code: &str, display_name: &str) -> Result<Challenge> {
        self.check_setup_code(setup_code)?;
        if self.owner_exists().await? {
            return Err(IdentityError::AlreadySetUp);
        }
        let user_id = Uuid::now_v7();
        let (ccr, reg) = self
            .webauthn
            .start_passkey_registration(user_id, display_name, display_name, None)
            .map_err(|_| IdentityError::PasskeyRejected)?;
        let state = SetupState {
            reg,
            user_id,
            household_id: Uuid::now_v7(),
            display_name: display_name.to_owned(),
        };
        let challenge_id = store_challenge(
            &self.pool,
            Purpose::Setup,
            &serde_json::to_value(&state)?,
            None,
        )
        .await?;
        Ok(Challenge {
            challenge_id,
            options: require_resident_key(serde_json::to_value(&ccr)?),
        })
    }

    /// Finishes owner setup: verifies the new passkey, creates the owner,
    /// the household, recovery codes, and a session, in one transaction.
    ///
    /// # Errors
    ///
    /// [`IdentityError::ChallengeExpired`], [`IdentityError::PasskeyRejected`],
    /// [`IdentityError::AlreadySetUp`], or a database error.
    pub async fn finish_setup(
        &self,
        challenge_id: Uuid,
        credential: &RegisterPublicKeyCredential,
        passkey_name: &str,
        ctx: &RequestContext,
    ) -> Result<SetupResult> {
        let state: SetupState =
            take_challenge(&self.pool, challenge_id, Purpose::Setup, None).await?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(credential, &state.reg)
            .map_err(|e| {
                tracing::info!(error = %e, "setup passkey rejected");
                IdentityError::PasskeyRejected
            })?;

        let mut tx = platform_db::tenant_tx(&self.pool, state.household_id).await?;
        let created: bool = sqlx::query_scalar("SELECT app.create_owner($1, $2, $3)")
            .bind(state.user_id)
            .bind(state.household_id)
            .bind(&state.display_name)
            .fetch_one(&mut *tx)
            .await?;
        if !created {
            return Err(IdentityError::AlreadySetUp);
        }
        insert_passkey(
            &mut tx,
            state.household_id,
            state.user_id,
            &passkey,
            passkey_name,
        )
        .await?;
        let recovery_codes =
            replace_recovery_codes(&mut tx, state.household_id, state.user_id).await?;
        let session = create_session(&mut tx, state.household_id, state.user_id, ctx).await?;
        audit(
            &mut tx,
            Some(state.household_id),
            Some(state.user_id),
            "owner.setup",
            serde_json::json!({}),
            ctx,
        )
        .await?;
        tx.commit().await?;
        Ok(SetupResult {
            session,
            recovery_codes,
        })
    }

    /// Starts passkey sign-in. The browser shows the passkeys for this site.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn start_login(&self) -> Result<Challenge> {
        let (rcr, auth) = self
            .webauthn
            .start_discoverable_authentication()
            .map_err(|_| IdentityError::PasskeyRejected)?;
        let state = LoginState { auth };
        let challenge_id = store_challenge(
            &self.pool,
            Purpose::Login,
            &serde_json::to_value(&state)?,
            None,
        )
        .await?;
        let mut options = serde_json::to_value(&rcr)?;
        // The web app asks after a button press, not through autofill.
        if let Some(map) = options.as_object_mut() {
            map.remove("mediation");
        }
        Ok(Challenge {
            challenge_id,
            options,
        })
    }

    /// Finishes passkey sign-in and creates a session.
    ///
    /// # Errors
    ///
    /// [`IdentityError::ChallengeExpired`], [`IdentityError::UnknownPasskey`],
    /// [`IdentityError::PasskeyRejected`], or a database error.
    pub async fn finish_login(
        &self,
        challenge_id: Uuid,
        credential: &PublicKeyCredential,
        ctx: &RequestContext,
    ) -> Result<NewSession> {
        let state: LoginState =
            take_challenge(&self.pool, challenge_id, Purpose::Login, None).await?;
        let (user_id, credential_id) = self
            .webauthn
            .identify_discoverable_authentication(credential)
            .map_err(|_| IdentityError::PasskeyRejected)?;

        let row: Option<(Uuid, Uuid, Uuid, Value)> =
            sqlx::query_as("SELECT id, household_id, user_id, passkey FROM app.find_passkey($1)")
                .bind(credential_id)
                .fetch_optional(&self.pool)
                .await?;
        let Some((passkey_id, household_id, stored_user, passkey_json)) = row else {
            audit_failure(&self.pool, "login.unknown_passkey", ctx).await;
            return Err(IdentityError::UnknownPasskey);
        };
        if stored_user != user_id {
            audit_failure(&self.pool, "login.user_mismatch", ctx).await;
            return Err(IdentityError::PasskeyRejected);
        }
        let mut passkey: Passkey = serde_json::from_value(passkey_json)?;
        let result = self
            .webauthn
            .finish_discoverable_authentication_v2(credential, state.auth, &passkey)
            .map_err(|e| {
                tracing::info!(error = %e, "login passkey rejected");
                IdentityError::PasskeyRejected
            });
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                audit_failure(&self.pool, "login.rejected", ctx).await;
                return Err(e);
            }
        };

        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let updated = passkey.update_credential(&result).unwrap_or(false);
        sqlx::query(
            "UPDATE app.passkeys SET last_used_at = now(), \
             passkey = CASE WHEN $2 THEN $3 ELSE passkey END WHERE id = $1",
        )
        .bind(passkey_id)
        .bind(updated)
        .bind(serde_json::to_value(&passkey)?)
        .execute(&mut *tx)
        .await?;
        let session = create_session(&mut tx, household_id, user_id, ctx).await?;
        audit(
            &mut tx,
            Some(household_id),
            Some(user_id),
            "login.passkey",
            serde_json::json!({ "passkey_id": passkey_id }),
            ctx,
        )
        .await?;
        tx.commit().await?;
        Ok(session)
    }

    /// Signs in with a recovery code. The code works once. The session can
    /// do step-up actions, so the person can add a new passkey.
    ///
    /// # Errors
    ///
    /// [`IdentityError::InvalidRecoveryCode`] or a database error.
    pub async fn login_with_recovery_code(
        &self,
        code: &str,
        ctx: &RequestContext,
    ) -> Result<NewSession> {
        let Some(normalized) = token::normalize_recovery_code(code) else {
            audit_failure(&self.pool, "login.recovery_code_invalid", ctx).await;
            return Err(IdentityError::InvalidRecoveryCode);
        };
        let row: Option<(Uuid, Uuid)> =
            sqlx::query_as("SELECT household_id, user_id FROM app.use_recovery_code($1)")
                .bind(token::hash_recovery_code(&normalized))
                .fetch_optional(&self.pool)
                .await?;
        let Some((household_id, user_id)) = row else {
            audit_failure(&self.pool, "login.recovery_code_invalid", ctx).await;
            return Err(IdentityError::InvalidRecoveryCode);
        };
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let session = create_session(&mut tx, household_id, user_id, ctx).await?;
        audit(
            &mut tx,
            Some(household_id),
            Some(user_id),
            "login.recovery_code",
            serde_json::json!({}),
            ctx,
        )
        .await?;
        tx.commit().await?;
        Ok(session)
    }

    /// Finds the session for a token. Returns `None` if the token is unknown
    /// or expired.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn authenticate(&self, token: &str) -> Result<Option<SessionInfo>> {
        let row: Option<(Uuid, Uuid, Uuid, jiff_sqlx::Timestamp)> = sqlx::query_as(
            "SELECT id, household_id, user_id, verified_at FROM app.find_session($1)",
        )
        .bind(token::hash(token))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(session_id, household_id, user_id, verified_at)| SessionInfo {
                session_id,
                household_id,
                user_id,
                verified_at: verified_at.to_jiff(),
            },
        ))
    }

    /// Ends a session.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn logout(&self, session: &SessionInfo, ctx: &RequestContext) -> Result<()> {
        let mut tx = platform_db::tenant_tx(&self.pool, session.household_id).await?;
        sqlx::query("DELETE FROM app.sessions WHERE id = $1")
            .bind(session.session_id)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut tx,
            Some(session.household_id),
            Some(session.user_id),
            "logout",
            serde_json::json!({}),
            ctx,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The signed-in person and the security settings.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn me(&self, session: &SessionInfo) -> Result<Me> {
        let mut tx = platform_db::tenant_tx(&self.pool, session.household_id).await?;
        let (display_name, time_zone, calm_mode): (String, String, bool) = sqlx::query_as(
            "SELECT display_name, time_zone, calm_mode FROM app.users WHERE id = $1",
        )
        .bind(session.user_id)
        .fetch_one(&mut *tx)
        .await?;
        let rows: Vec<PasskeyRow> = sqlx::query_as(
            "SELECT id, name, created_at, last_used_at FROM app.passkeys \
             WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(session.user_id)
        .fetch_all(&mut *tx)
        .await?;
        let passkeys = rows
            .into_iter()
            .map(|(id, name, created_at, last_used_at)| PasskeySummary {
                id,
                name,
                created_at: created_at.to_jiff(),
                last_used_at: last_used_at.map(jiff_sqlx::Timestamp::to_jiff),
            })
            .collect();
        let recovery_codes_left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM app.recovery_codes WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(session.user_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Me {
            user_id: session.user_id,
            household_id: session.household_id,
            display_name,
            time_zone,
            calm_mode,
            step_up_until: session.step_up_until(),
            passkeys,
            recovery_codes_left,
        })
    }

    /// Changes the person's display settings. `None` leaves a value as it is.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn update_settings(
        &self,
        session: &SessionInfo,
        calm_mode: Option<bool>,
    ) -> Result<()> {
        let mut tx = platform_db::tenant_tx(&self.pool, session.household_id).await?;
        sqlx::query("UPDATE app.users SET calm_mode = coalesce($2, calm_mode) WHERE id = $1")
            .bind(session.user_id)
            .bind(calm_mode)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn user_passkeys(&self, session: &SessionInfo) -> Result<Vec<Passkey>> {
        let mut tx = platform_db::tenant_tx(&self.pool, session.household_id).await?;
        let rows: Vec<(Value,)> =
            sqlx::query_as("SELECT passkey FROM app.passkeys WHERE user_id = $1")
                .bind(session.user_id)
                .fetch_all(&mut *tx)
                .await?;
        tx.commit().await?;
        rows.into_iter()
            .map(|(v,)| serde_json::from_value(v).map_err(IdentityError::from))
            .collect()
    }

    /// Starts a passkey check for a step-up action.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn start_verify(&self, session: &SessionInfo) -> Result<Challenge> {
        let passkeys = self.user_passkeys(session).await?;
        let (rcr, auth) = self
            .webauthn
            .start_passkey_authentication(&passkeys)
            .map_err(|_| IdentityError::PasskeyRejected)?;
        let state = VerifyState { auth };
        let challenge_id = store_challenge(
            &self.pool,
            Purpose::Verify,
            &serde_json::to_value(&state)?,
            Some(session),
        )
        .await?;
        Ok(Challenge {
            challenge_id,
            options: serde_json::to_value(&rcr)?,
        })
    }

    /// Finishes a step-up check. Step-up actions are then allowed for
    /// [`STEP_UP_WINDOW`].
    ///
    /// # Errors
    ///
    /// [`IdentityError::ChallengeExpired`], [`IdentityError::PasskeyRejected`],
    /// or a database error.
    pub async fn finish_verify(
        &self,
        session: &SessionInfo,
        challenge_id: Uuid,
        credential: &PublicKeyCredential,
        ctx: &RequestContext,
    ) -> Result<SessionInfo> {
        let state: VerifyState =
            take_challenge(&self.pool, challenge_id, Purpose::Verify, Some(session)).await?;
        let result = self
            .webauthn
            .finish_passkey_authentication(credential, &state.auth)
            .map_err(|e| {
                tracing::info!(error = %e, "step-up passkey rejected");
                IdentityError::PasskeyRejected
            })?;

        let mut tx = platform_db::tenant_tx(&self.pool, session.household_id).await?;
        let row: Option<(Uuid, Value)> = sqlx::query_as(
            "SELECT id, passkey FROM app.passkeys WHERE user_id = $1 AND credential_id = $2",
        )
        .bind(session.user_id)
        .bind(result.cred_id().as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        let (passkey_id, passkey_json) = row.ok_or(IdentityError::UnknownPasskey)?;
        let mut passkey: Passkey = serde_json::from_value(passkey_json)?;
        let updated = passkey.update_credential(&result).unwrap_or(false);
        sqlx::query(
            "UPDATE app.passkeys SET last_used_at = now(), \
             passkey = CASE WHEN $2 THEN $3 ELSE passkey END WHERE id = $1",
        )
        .bind(passkey_id)
        .bind(updated)
        .bind(serde_json::to_value(&passkey)?)
        .execute(&mut *tx)
        .await?;
        let verified_at: jiff_sqlx::Timestamp = sqlx::query_scalar(
            "UPDATE app.sessions SET verified_at = now() WHERE id = $1 RETURNING verified_at",
        )
        .bind(session.session_id)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut tx,
            Some(session.household_id),
            Some(session.user_id),
            "step_up.passkey",
            serde_json::json!({ "passkey_id": passkey_id }),
            ctx,
        )
        .await?;
        tx.commit().await?;
        Ok(SessionInfo {
            verified_at: verified_at.to_jiff(),
            ..session.clone()
        })
    }

    /// Starts adding a passkey. The caller checks step-up first.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn start_add_passkey(&self, session: &SessionInfo) -> Result<Challenge> {
        let passkeys = self.user_passkeys(session).await?;
        let exclude = passkeys.iter().map(|p| p.cred_id().clone()).collect();
        let me = self.me(session).await?;
        let (ccr, reg) = self
            .webauthn
            .start_passkey_registration(
                session.user_id,
                &me.display_name,
                &me.display_name,
                Some(exclude),
            )
            .map_err(|_| IdentityError::PasskeyRejected)?;
        let state = AddPasskeyState { reg };
        let challenge_id = store_challenge(
            &self.pool,
            Purpose::AddPasskey,
            &serde_json::to_value(&state)?,
            Some(session),
        )
        .await?;
        Ok(Challenge {
            challenge_id,
            options: require_resident_key(serde_json::to_value(&ccr)?),
        })
    }

    /// Finishes adding a passkey. The caller checks step-up first.
    ///
    /// # Errors
    ///
    /// [`IdentityError::ChallengeExpired`], [`IdentityError::PasskeyRejected`],
    /// or a database error.
    pub async fn finish_add_passkey(
        &self,
        session: &SessionInfo,
        challenge_id: Uuid,
        credential: &RegisterPublicKeyCredential,
        name: &str,
        ctx: &RequestContext,
    ) -> Result<Uuid> {
        let state: AddPasskeyState =
            take_challenge(&self.pool, challenge_id, Purpose::AddPasskey, Some(session)).await?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(credential, &state.reg)
            .map_err(|e| {
                tracing::info!(error = %e, "new passkey rejected");
                IdentityError::PasskeyRejected
            })?;
        let mut tx = platform_db::tenant_tx(&self.pool, session.household_id).await?;
        let id = insert_passkey(
            &mut tx,
            session.household_id,
            session.user_id,
            &passkey,
            name,
        )
        .await?;
        audit(
            &mut tx,
            Some(session.household_id),
            Some(session.user_id),
            "passkey.add",
            serde_json::json!({ "passkey_id": id }),
            ctx,
        )
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Deletes a passkey. The last passkey cannot be deleted. The caller
    /// checks step-up first.
    ///
    /// # Errors
    ///
    /// [`IdentityError::NotFound`], [`IdentityError::LastPasskey`], or a
    /// database error.
    pub async fn delete_passkey(
        &self,
        session: &SessionInfo,
        passkey_id: Uuid,
        ctx: &RequestContext,
    ) -> Result<()> {
        let mut tx = platform_db::tenant_tx(&self.pool, session.household_id).await?;
        // Lock the person's passkeys so two deletes cannot remove the last two.
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM app.passkeys WHERE user_id = $1 FOR UPDATE")
                .bind(session.user_id)
                .fetch_all(&mut *tx)
                .await?;
        if !ids.contains(&passkey_id) {
            return Err(IdentityError::NotFound);
        }
        if ids.len() == 1 {
            return Err(IdentityError::LastPasskey);
        }
        sqlx::query("DELETE FROM app.passkeys WHERE id = $1")
            .bind(passkey_id)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut tx,
            Some(session.household_id),
            Some(session.user_id),
            "passkey.delete",
            serde_json::json!({ "passkey_id": passkey_id }),
            ctx,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replaces all recovery codes with a new set. The caller checks step-up
    /// first.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub async fn regenerate_recovery_codes(
        &self,
        session: &SessionInfo,
        ctx: &RequestContext,
    ) -> Result<Vec<String>> {
        let mut tx = platform_db::tenant_tx(&self.pool, session.household_id).await?;
        let codes = replace_recovery_codes(&mut tx, session.household_id, session.user_id).await?;
        audit(
            &mut tx,
            Some(session.household_id),
            Some(session.user_id),
            "recovery_codes.regenerate",
            serde_json::json!({}),
            ctx,
        )
        .await?;
        tx.commit().await?;
        Ok(codes)
    }
}

/// Deletes expired challenges, sessions, and idempotency keys. Returns the
/// number of rows deleted.
///
/// # Errors
///
/// Returns a database error.
pub async fn delete_expired(pool: &PgPool) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT app.delete_expired()")
        .fetch_one(pool)
        .await?)
}

/// Asks the authenticator for a discoverable credential (a passkey), so that
/// sign-in works without a user name. webauthn-rs leaves this optional.
fn require_resident_key(mut options: Value) -> Value {
    if let Some(selection) = options
        .pointer_mut("/publicKey/authenticatorSelection")
        .and_then(Value::as_object_mut)
    {
        selection.insert("residentKey".into(), Value::from("required"));
        selection.insert("requireResidentKey".into(), Value::from(true));
    }
    options
}

async fn store_challenge(
    pool: &PgPool,
    purpose: Purpose,
    state: &Value,
    session: Option<&SessionInfo>,
) -> Result<Uuid> {
    Ok(sqlx::query_scalar(
        "INSERT INTO app.auth_challenges (purpose, state, session_id) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(purpose.as_str())
    .bind(state)
    .bind(session.map(|s| s.session_id))
    .fetch_one(pool)
    .await?)
}

/// Deletes and returns a challenge. A challenge works once.
async fn take_challenge<T: serde::de::DeserializeOwned>(
    pool: &PgPool,
    id: Uuid,
    purpose: Purpose,
    session: Option<&SessionInfo>,
) -> Result<T> {
    let state: Option<Value> = sqlx::query_scalar(
        "DELETE FROM app.auth_challenges \
         WHERE id = $1 AND purpose = $2 AND session_id IS NOT DISTINCT FROM $3 \
           AND expires_at > now() \
         RETURNING state",
    )
    .bind(id)
    .bind(purpose.as_str())
    .bind(session.map(|s| s.session_id))
    .fetch_optional(pool)
    .await?;
    let state = state.ok_or(IdentityError::ChallengeExpired)?;
    Ok(serde_json::from_value(state)?)
}

async fn insert_passkey(
    tx: &mut platform_db::TenantTx,
    household_id: Uuid,
    user_id: Uuid,
    passkey: &Passkey,
    name: &str,
) -> Result<Uuid> {
    Ok(sqlx::query_scalar(
        "INSERT INTO app.passkeys (household_id, user_id, credential_id, passkey, name) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(household_id)
    .bind(user_id)
    .bind(passkey.cred_id().as_slice())
    .bind(serde_json::to_value(passkey)?)
    .bind(name)
    .fetch_one(&mut **tx)
    .await?)
}

async fn replace_recovery_codes(
    tx: &mut platform_db::TenantTx,
    household_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<String>> {
    sqlx::query("DELETE FROM app.recovery_codes WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    let codes: Vec<String> = (0..token::RECOVERY_CODE_COUNT)
        .map(|_| token::new_recovery_code())
        .collect();
    let hashes: Vec<Vec<u8>> = codes
        .iter()
        .filter_map(|c| token::normalize_recovery_code(c))
        .map(|n| token::hash_recovery_code(&n))
        .collect();
    sqlx::query(
        "INSERT INTO app.recovery_codes (household_id, user_id, code_hash) \
         SELECT $1, $2, h FROM unnest($3::bytea[]) AS h",
    )
    .bind(household_id)
    .bind(user_id)
    .bind(&hashes)
    .execute(&mut **tx)
    .await?;
    Ok(codes)
}

async fn create_session(
    tx: &mut platform_db::TenantTx,
    household_id: Uuid,
    user_id: Uuid,
    ctx: &RequestContext,
) -> Result<NewSession> {
    let token = token::new_session_token();
    let (session_id, verified_at): (Uuid, jiff_sqlx::Timestamp) = sqlx::query_as(
        "INSERT INTO app.sessions (household_id, user_id, token_hash, expires_at, user_agent) \
         VALUES ($1, $2, $3, now() + make_interval(days => $4), $5) \
         RETURNING id, verified_at",
    )
    .bind(household_id)
    .bind(user_id)
    .bind(token::hash(&token))
    .bind(i32::try_from(SESSION_ABSOLUTE_DAYS).unwrap_or(90))
    .bind(ctx.user_agent.as_deref())
    .fetch_one(&mut **tx)
    .await?;
    Ok(NewSession {
        token,
        info: SessionInfo {
            session_id,
            household_id,
            user_id,
            verified_at: verified_at.to_jiff(),
        },
    })
}

async fn audit(
    tx: &mut platform_db::TenantTx,
    household_id: Option<Uuid>,
    user_id: Option<Uuid>,
    action: &str,
    detail: Value,
    ctx: &RequestContext,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO app.audit_log (household_id, user_id, action, detail, request_id) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(household_id)
    .bind(user_id)
    .bind(action)
    .bind(detail)
    .bind(ctx.request_id.as_deref())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Records a failed sign-in. A failure to write is logged, not returned,
/// so the person sees the real error.
async fn audit_failure(pool: &PgPool, action: &str, ctx: &RequestContext) {
    let result = sqlx::query("INSERT INTO app.audit_log (action, request_id) VALUES ($1, $2)")
        .bind(action)
        .bind(ctx.request_id.as_deref())
        .execute(pool)
        .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, action, "cannot write the audit log");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_up_window_is_five_minutes() {
        let verified_at = jiff::Timestamp::from_second(1_800_000_000).unwrap_or_default();
        let s = SessionInfo {
            session_id: Uuid::nil(),
            household_id: Uuid::nil(),
            user_id: Uuid::nil(),
            verified_at,
        };
        let four = verified_at
            .checked_add(jiff::SignedDuration::from_mins(4))
            .unwrap_or_default();
        let six = verified_at
            .checked_add(jiff::SignedDuration::from_mins(6))
            .unwrap_or_default();
        assert!(s.step_up_valid(four));
        assert!(!s.step_up_valid(six));
    }

    #[test]
    fn require_resident_key_sets_both_fields() {
        let options = serde_json::json!({
            "publicKey": { "authenticatorSelection": { "residentKey": "discouraged", "requireResidentKey": false } }
        });
        let out = require_resident_key(options);
        assert_eq!(
            out["publicKey"]["authenticatorSelection"]["residentKey"],
            "required"
        );
        assert_eq!(
            out["publicKey"]["authenticatorSelection"]["requireResidentKey"],
            true
        );
    }
}
