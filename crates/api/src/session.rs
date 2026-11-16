//! The session cookie and the signed-in request context (design doc §12.1).

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use domain_identity::{RequestContext, SessionInfo};
use tower_http::request_id::RequestId;

use crate::AppState;
use crate::error::ApiError;

/// The session cookie. The `__Host-` prefix makes the browser require
/// `Secure`, `Path=/`, and no `Domain`, so a subdomain cannot set it.
pub const COOKIE: &str = "__Host-session";

/// The cookie lifetime: the absolute session limit.
const MAX_AGE_SECS: i64 = domain_identity::SESSION_ABSOLUTE_DAYS * 24 * 60 * 60;

/// A `Set-Cookie` value that stores the session token.
pub fn set_cookie(token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; Max-Age={MAX_AGE_SECS}; HttpOnly; Secure; SameSite=Strict"
    ))
    .unwrap_or_else(|_| clear_cookie())
}

/// A `Set-Cookie` value that removes the session cookie.
pub fn clear_cookie() -> HeaderValue {
    HeaderValue::from_static(
        "__Host-session=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Strict",
    )
}

/// The session token from the `Cookie` header, if any.
pub fn token_from(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value)
        .filter(|v| !v.is_empty())
}

/// Request details for the audit log.
pub fn context(headers: &HeaderMap, request_id: Option<&RequestId>) -> RequestContext {
    RequestContext {
        request_id: request_id
            .and_then(|id| id.header_value().to_str().ok())
            .map(str::to_owned),
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|ua| ua.chars().take(200).collect()),
    }
}

/// Requires a valid session. Adds [`SessionInfo`] to the request.
///
/// An unknown or expired token gets a 401 and a cookie that removes it.
pub async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = token_from(request.headers()) else {
        return ApiError::unauthenticated().into_response();
    };
    match state.identity.authenticate(token).await {
        Ok(Some(session)) => {
            request.extensions_mut().insert(session);
            next.run(request).await
        }
        Ok(None) => {
            let mut response = ApiError::unauthenticated().into_response();
            response
                .headers_mut()
                .insert(header::SET_COOKIE, clear_cookie());
            response
        }
        Err(e) => ApiError::from(e).into_response(),
    }
}

/// The signed-in session and the request context. Only routes behind
/// [`require_session`] can use it.
#[derive(Debug, Clone)]
pub struct Auth {
    /// The session.
    pub session: SessionInfo,
    /// Request details for the audit log.
    pub ctx: RequestContext,
}

impl Auth {
    /// Fails with `step_up_required` unless a passkey check happened in the
    /// last 5 minutes (§12.1).
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::step_up_required`].
    pub fn require_step_up(&self) -> Result<(), ApiError> {
        if self.session.step_up_valid(jiff::Timestamp::now()) {
            Ok(())
        } else {
            Err(ApiError::step_up_required())
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Auth {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let result = parts
            .extensions
            .get::<SessionInfo>()
            .cloned()
            .ok_or_else(ApiError::unauthenticated)
            .map(|session| Self {
                session,
                ctx: context(&parts.headers, parts.extensions.get::<RequestId>()),
            });
        std::future::ready(result)
    }
}

/// Request details for routes without a session.
#[derive(Debug, Clone)]
pub struct Ctx(pub RequestContext);

impl<S: Send + Sync> FromRequestParts<S> for Ctx {
    type Rejection = std::convert::Infallible;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Self(context(
            &parts.headers,
            parts.extensions.get::<RequestId>(),
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_session_cookie_among_others() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("a=1; __Host-session=tok; b=2"),
        );
        assert_eq!(token_from(&headers), Some("tok"));

        let mut headers = HeaderMap::new();
        headers.append(header::COOKIE, HeaderValue::from_static("__Host-session="));
        assert_eq!(token_from(&headers), None);
        assert_eq!(token_from(&HeaderMap::new()), None);
    }

    #[test]
    fn set_cookie_has_the_strict_attributes() {
        let v = set_cookie("abc");
        let v = v.to_str().unwrap_or_default();
        for part in [
            "__Host-session=abc",
            "HttpOnly",
            "Secure",
            "SameSite=Strict",
            "Path=/",
        ] {
            assert!(v.contains(part), "{v}");
        }
    }
}
