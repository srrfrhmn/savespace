//! API errors in the shape from design doc §11.4.
//!
//! A handler returns [`ApiError`]. The response carries the error as an
//! extension, and [`render_errors`] writes the JSON body with the request ID,
//! so every error body has the same `request_id` as the `Request-Id` header.

use std::borrow::Cow;

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use domain_identity::IdentityError;
use serde::Serialize;
use tower_http::request_id::RequestId;
use utoipa::ToSchema;

/// The error `type` values from §11.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    /// The request is not valid.
    InvalidRequestError,
    /// The person is not signed in, or the sign-in check failed.
    AuthenticationError,
    /// The person may not do this, or must confirm with a passkey first.
    PermissionError,
    /// The resource does not exist.
    NotFoundError,
    /// Too many requests.
    RateLimitError,
    /// A server error.
    ApiError,
}

/// An API error.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ApiError {
    /// The error class.
    #[serde(rename = "type")]
    pub kind: ErrorType,
    /// A stable, machine-readable code, for example `step_up_required`.
    pub code: Cow<'static, str>,
    /// A sentence for a developer. The web app shows its own text.
    pub message: Cow<'static, str>,
    /// The request field that caused the error, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<Cow<'static, str>>,
    /// The request ID, the same as the `Request-Id` header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip)]
    status: StatusCode,
}

/// The JSON body of every error response.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// The error.
    pub error: ApiError,
}

impl ApiError {
    /// A new error.
    pub fn new(
        status: StatusCode,
        kind: ErrorType,
        code: impl Into<Cow<'static, str>>,
        message: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            kind,
            code: code.into(),
            message: message.into(),
            param: None,
            request_id: None,
            status,
        }
    }

    /// Names the request field that caused the error.
    #[must_use]
    pub fn param(mut self, param: impl Into<Cow<'static, str>>) -> Self {
        self.param = Some(param.into());
        self
    }

    /// A 400 `invalid_request_error`.
    pub fn invalid(code: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ErrorType::InvalidRequestError,
            code,
            message,
        )
    }

    /// A 401: no valid session.
    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ErrorType::AuthenticationError,
            "session_required",
            "Sign in to continue.",
        )
    }

    /// A 403: the action needs a recent passkey check (§12.1).
    pub fn step_up_required() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            ErrorType::PermissionError,
            "step_up_required",
            "Confirm with your passkey, then try again.",
        )
    }

    /// A 404 for a resource.
    pub fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            ErrorType::NotFoundError,
            "resource_not_found",
            "The resource does not exist.",
        )
    }

    /// A 429.
    pub fn rate_limited() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorType::RateLimitError,
            "rate_limited",
            "Too many requests. Wait a minute, then try again.",
        )
    }

    /// A 500. Log the cause before you return this.
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorType::ApiError,
            "internal",
            "Something went wrong on our side. Try again later.",
        )
    }

    /// The HTTP status.
    pub fn status(&self) -> StatusCode {
        self.status
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = self.status.into_response();
        response.extensions_mut().insert(self);
        response
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        let code = match rejection {
            JsonRejection::MissingJsonContentType(_) => "content_type_invalid",
            JsonRejection::JsonSyntaxError(_) => "json_invalid",
            JsonRejection::JsonDataError(_) => "body_invalid",
            _ => "body_unreadable",
        };
        let status = if code == "content_type_invalid" {
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        } else {
            StatusCode::BAD_REQUEST
        };
        Self::new(
            status,
            ErrorType::InvalidRequestError,
            code,
            rejection.body_text(),
        )
    }
}

impl From<IdentityError> for ApiError {
    fn from(e: IdentityError) -> Self {
        use ErrorType::{AuthenticationError, InvalidRequestError, NotFoundError, PermissionError};
        let (status, kind, code) = match &e {
            IdentityError::SetupUnavailable => {
                (StatusCode::NOT_FOUND, NotFoundError, "setup_unavailable")
            }
            IdentityError::InvalidSetupCode => {
                return Self::new(
                    StatusCode::FORBIDDEN,
                    PermissionError,
                    "setup_code_invalid",
                    e.to_string(),
                )
                .param("setup_code");
            }
            IdentityError::AlreadySetUp => {
                (StatusCode::CONFLICT, InvalidRequestError, "already_set_up")
            }
            IdentityError::ChallengeExpired => (
                StatusCode::BAD_REQUEST,
                InvalidRequestError,
                "challenge_expired",
            ),
            IdentityError::PasskeyRejected => (
                StatusCode::UNAUTHORIZED,
                AuthenticationError,
                "passkey_rejected",
            ),
            IdentityError::UnknownPasskey => (
                StatusCode::UNAUTHORIZED,
                AuthenticationError,
                "passkey_unknown",
            ),
            IdentityError::InvalidRecoveryCode => (
                StatusCode::UNAUTHORIZED,
                AuthenticationError,
                "recovery_code_invalid",
            ),
            IdentityError::LastPasskey => {
                (StatusCode::CONFLICT, InvalidRequestError, "last_passkey")
            }
            IdentityError::NotFound => return Self::not_found(),
            IdentityError::Db(_)
            | IdentityError::State(_)
            | IdentityError::Config(_)
            | IdentityError::Crypto(_) => {
                tracing::error!(error = %e, "identity error");
                return Self::internal();
            }
        };
        Self::new(status, kind, code, e.to_string())
    }
}

impl From<platform_db::DbError> for ApiError {
    fn from(e: platform_db::DbError) -> Self {
        tracing::error!(error = %e, "database error");
        Self::internal()
    }
}

/// A JSON body extractor that rejects with an [`ApiError`].
#[derive(Debug, Clone, Copy, Default, FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct Json<T>(pub T);

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// Writes the JSON body for [`ApiError`] responses, with the request ID.
pub async fn render_errors(request: Request, next: Next) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .map(str::to_owned);
    let mut response = next.run(request).await;
    let Some(mut error) = response.extensions_mut().remove::<ApiError>() else {
        return response;
    };
    error.request_id = request_id;
    let status = error.status;
    let body = serde_json::to_vec(&ErrorBody { error }).unwrap_or_default();
    let (mut parts, _) = response.into_parts();
    parts.status = status;
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, axum::body::Body::from(body))
}
