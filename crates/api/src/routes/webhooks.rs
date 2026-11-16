//! Webhook ingress (design doc §10.2).
//!
//! Verify, store, queue, and answer fast. No other work happens on the
//! request path. These routes have no session and no `Origin` check: the
//! signature is the authentication.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::error::{ApiError, ErrorBody, ErrorType};

/// Webhook routes.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(plaid_webhook))
}

fn invalid_webhook() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        ErrorType::AuthenticationError,
        "webhook_invalid",
        "The webhook signature is not valid.",
    )
}

/// Receives a Plaid webhook. Plaid signs it with the `Plaid-Verification`
/// header (ES256 JWT).
#[utoipa::path(post, path = "/webhooks/plaid", tag = "webhooks",
    request_body(content = Object, description = "The Plaid webhook body."),
    responses((status = 200), (status = 401, body = ErrorBody), (status = 503, body = ErrorBody)))]
async fn plaid_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let (Some(sync), Some(plaid)) = (state.sync.as_ref(), state.plaid.as_ref()) else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorType::ApiError,
            "aggregator_unavailable",
            "Bank connections are not configured on this server.",
        ));
    };
    let Some(jwt) = headers
        .get("plaid-verification")
        .and_then(|v| v.to_str().ok())
    else {
        tracing::warn!("webhook without a verification header");
        return Err(invalid_webhook());
    };
    if let Err(e) = plaid
        .verify_webhook(jwt, &body, jiff::Timestamp::now().as_second())
        .await
    {
        return Err(match e {
            // Plaid retries a webhook that does not get a 200.
            aggregator_plaid::WebhookError::Fetch(f) => {
                tracing::warn!(error = %f, "cannot fetch the webhook key");
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    ErrorType::ApiError,
                    "aggregator_unavailable",
                    "Try again later.",
                )
            }
            other => {
                tracing::warn!(error = %other, "webhook rejected");
                invalid_webhook()
            }
        });
    }
    let value: Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError::invalid("body_invalid", "The body is not JSON."))?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let (Some(webhook_type), Some(webhook_code)) = (text("webhook_type"), text("webhook_code"))
    else {
        return Err(ApiError::invalid(
            "body_invalid",
            "webhook_type and webhook_code are required.",
        ));
    };
    let item_id = text("item_id");
    sync.store_webhook(item_id.as_deref(), &webhook_type, &webhook_code, &value)
        .await?;
    tracing::info!(%webhook_type, %webhook_code, "webhook stored");
    state.worker.kick().await;
    Ok(StatusCode::OK)
}
