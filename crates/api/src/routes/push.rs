//! Web Push subscriptions (design doc D13, §10.4).

use axum::extract::State;
use axum::http::StatusCode;
use domain_notify::NotifyError;
use domain_notify::webpush::Keys;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::error::{ApiError, ErrorBody, ErrorType, Json};
use crate::session::Auth;

/// Routes that need a session.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(push_key))
        .routes(routes!(subscribe, unsubscribe))
        .routes(routes!(test_push))
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        ErrorType::ApiError,
        "push_unavailable",
        "Notifications are not available on this server.",
    )
}

impl From<NotifyError> for ApiError {
    fn from(e: NotifyError) -> Self {
        match e {
            NotifyError::InvalidSubscription => ApiError::invalid(
                "subscription_invalid",
                "This browser's push subscription is not supported.",
            ),
            NotifyError::Conflict => ApiError::new(
                StatusCode::CONFLICT,
                ErrorType::InvalidRequestError,
                "subscription_conflict",
                "This browser is already subscribed for another account.",
            ),
            NotifyError::Unavailable => unavailable(),
            NotifyError::Db(e) => {
                tracing::error!(error = %e, "notify error");
                ApiError::internal()
            }
        }
    }
}

/// The application server key.
#[derive(Debug, Serialize, ToSchema)]
pub struct PushKey {
    /// Pass to `pushManager.subscribe({ applicationServerKey })`. Base64url.
    pub public_key: String,
}

/// Gets the VAPID public key.
#[utoipa::path(get, path = "/push/key", tag = "me",
    responses((status = 200, body = PushKey), (status = "4XX", body = ErrorBody),
              (status = 503, body = ErrorBody)))]
async fn push_key(State(state): State<AppState>, _auth: Auth) -> Result<Json<PushKey>, ApiError> {
    let key = state.notify.public_key().ok_or_else(unavailable)?;
    Ok(Json(PushKey {
        public_key: key.to_owned(),
    }))
}

/// The keys of a push subscription.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SubscriptionKeys {
    /// Base64url.
    pub p256dh: String,
    /// Base64url.
    pub auth: String,
}

/// A browser push subscription (`PushSubscription.toJSON()`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct Subscription {
    /// The push service URL.
    pub endpoint: String,
    /// The keys.
    pub keys: SubscriptionKeys,
}

/// Saves this browser's push subscription.
#[utoipa::path(post, path = "/push/subscriptions", tag = "me",
    request_body = Subscription,
    responses((status = 204), (status = "4XX", body = ErrorBody)))]
async fn subscribe(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<Subscription>,
) -> Result<StatusCode, ApiError> {
    let keys = Keys::from_base64url(&body.keys.p256dh, &body.keys.auth)
        .map_err(|_| NotifyError::InvalidSubscription)?;
    state
        .notify
        .subscribe(
            auth.session.household_id,
            auth.session.user_id,
            &body.endpoint,
            &keys,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Which subscription to remove.
#[derive(Debug, Deserialize, ToSchema)]
pub struct Unsubscribe {
    /// The push service URL.
    pub endpoint: String,
}

/// Removes this browser's push subscription.
#[utoipa::path(delete, path = "/push/subscriptions", tag = "me",
    request_body = Unsubscribe,
    responses((status = 204), (status = "4XX", body = ErrorBody)))]
async fn unsubscribe(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<Unsubscribe>,
) -> Result<StatusCode, ApiError> {
    state
        .notify
        .unsubscribe(auth.session.household_id, &body.endpoint)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The result of a test push.
#[derive(Debug, Serialize, ToSchema)]
pub struct TestPush {
    /// How many browsers accepted the test notification.
    pub delivered: usize,
}

/// Sends a test notification to the person's browsers.
#[utoipa::path(post, path = "/push/test", tag = "me",
    responses((status = 200, body = TestPush), (status = "4XX", body = ErrorBody),
              (status = 503, body = ErrorBody)))]
async fn test_push(State(state): State<AppState>, auth: Auth) -> Result<Json<TestPush>, ApiError> {
    let delivered = state
        .notify
        .send_test(auth.session.household_id, auth.session.user_id)
        .await?;
    Ok(Json(TestPush { delivered }))
}
