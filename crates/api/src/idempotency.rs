//! The `Idempotency-Key` middleware (design doc §11.3).
//!
//! Runs on signed-in routes, after [`crate::session::require_session`].
//! Only POST requests with the header take part. The stored response has the
//! status, the body, and the content type. Other headers are not replayed.

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use domain_identity::SessionInfo;
use platform_db::idempotency::{self, Begin};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::{ApiError, ErrorType};

/// The request header.
pub const HEADER: HeaderName = HeaderName::from_static("idempotency-key");

/// Set on a replayed response.
pub const REPLAYED: HeaderName = HeaderName::from_static("idempotent-replayed");

/// The largest request body this middleware reads.
const MAX_REQUEST: usize = 256 * 1024;
/// The largest response body it stores.
const MAX_RESPONSE: usize = 1024 * 1024;

/// The middleware.
pub async fn idempotency(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if request.method() != Method::POST {
        return next.run(request).await;
    }
    let Some(key) = request.headers().get(&HEADER).cloned() else {
        return next.run(request).await;
    };
    let Some(key) = key.to_str().ok().filter(|k| (1..=255).contains(&k.len())) else {
        return ApiError::invalid(
            "idempotency_key_invalid",
            "The Idempotency-Key header must have 1 to 255 visible ASCII characters.",
        )
        .param("Idempotency-Key")
        .into_response();
    };
    let key = key.to_owned();
    let Some(household_id) = request
        .extensions()
        .get::<SessionInfo>()
        .map(|s| s.household_id)
    else {
        return ApiError::unauthenticated().into_response();
    };

    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, MAX_REQUEST).await else {
        return ApiError::invalid("body_too_large", "The request body is too large.")
            .into_response();
    };
    let mut hasher = Sha256::new();
    hasher.update(parts.method.as_str().as_bytes());
    hasher.update(b" ");
    hasher.update(parts.uri.path().as_bytes());
    hasher.update(b"\n");
    hasher.update(&body);
    let hash = hasher.finalize().to_vec();

    match idempotency::begin(&state.db, household_id, &key, &hash).await {
        Err(e) => return ApiError::from(e).into_response(),
        Ok(Begin::Mismatch) => {
            return ApiError::new(
                StatusCode::CONFLICT,
                ErrorType::InvalidRequestError,
                "idempotency_key_reused",
                "This Idempotency-Key was used for a different request.",
            )
            .param("Idempotency-Key")
            .into_response();
        }
        Ok(Begin::InProgress) => {
            return ApiError::new(
                StatusCode::CONFLICT,
                ErrorType::InvalidRequestError,
                "idempotency_request_in_progress",
                "A request with this Idempotency-Key is still running. Try again soon.",
            )
            .into_response();
        }
        Ok(Begin::Replay {
            status,
            body,
            content_type,
        }) => return replay(status, body, content_type.as_deref()),
        Ok(Begin::New) => {}
    }

    let response = next.run(Request::from_parts(parts, Body::from(body))).await;
    record(&state, household_id, &key, response).await
}

/// Stores the response for the key, or releases the key after a server error.
async fn record(
    state: &AppState,
    household_id: uuid::Uuid,
    key: &str,
    response: Response,
) -> Response {
    let (parts, body) = response.into_parts();
    let status = parts.status;
    if status.is_server_error() {
        release(state, household_id, key).await;
        return Response::from_parts(parts, body);
    }
    let Ok(bytes) = to_bytes(body, MAX_RESPONSE).await else {
        release(state, household_id, key).await;
        return ApiError::internal().into_response();
    };
    // An ApiError response has no body yet. render_errors runs outside this
    // middleware, so store the rendered form of the error here.
    let (stored, content_type) = match parts.extensions.get::<ApiError>() {
        Some(e) => (
            serde_json::to_vec(&crate::error::ErrorBody { error: e.clone() }).unwrap_or_default(),
            Some("application/json".to_owned()),
        ),
        None => (
            bytes.to_vec(),
            parts
                .headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        ),
    };
    if let Err(e) = idempotency::finish(
        &state.db,
        household_id,
        key,
        status.as_u16(),
        &stored,
        content_type.as_deref(),
    )
    .await
    {
        tracing::warn!(error = %e, "cannot store an idempotent response");
    }
    Response::from_parts(parts, Body::from(bytes))
}

async fn release(state: &AppState, household_id: uuid::Uuid, key: &str) {
    if let Err(e) = idempotency::abandon(&state.db, household_id, key).await {
        tracing::warn!(error = %e, "cannot release an idempotency key");
    }
}

fn replay(status: u16, body: Vec<u8>, content_type: Option<&str>) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    if let Some(ct) = content_type.and_then(|ct| HeaderValue::from_str(ct).ok()) {
        headers.insert(header::CONTENT_TYPE, ct);
    }
    headers.insert(REPLAYED, HeaderValue::from_static("true"));
    response
}
