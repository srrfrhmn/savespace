//! Request guards: canonical host, origin check, and rate limits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use url::Url;

use crate::AppState;
use crate::error::{ApiError, ErrorType};

/// The origin string (`scheme://host[:port]`) of a URL.
pub fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Redirects requests for another host name to the public origin.
///
/// Cloud Run serves a service on two `run.app` host names. Passkeys work on
/// one host name only (the relying party ID), so the app uses one. Health
/// checks work on any host.
pub async fn canonical_host(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let public = &state.public_origin;
    let Some(public_host) = public.host_str() else {
        return next.run(request).await;
    };
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_owned());
    let path = request.uri().path();
    let exempt = path == "/health" || path == "/ready" || public_host == "localhost";
    match host {
        Some(host) if !exempt && !host.eq_ignore_ascii_case(public_host) => {
            let target = request.uri().path_and_query().map_or("/", |pq| pq.as_str());
            Redirect::permanent(&format!("{}{target}", origin_of(public))).into_response()
        }
        _ => next.run(request).await,
    }
}

/// Rejects state-changing API requests from other sites (CSRF defense).
///
/// The session cookie is `SameSite=Strict`, and this check is the second
/// layer: POST, PUT, PATCH, and DELETE must send an `Origin` header that
/// equals the public origin. Browsers always send it on these methods.
pub async fn same_origin(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if is_safe(request.method()) || origin_matches(request.headers(), &state.public_origin) {
        return next.run(request).await;
    }
    ApiError::new(
        StatusCode::FORBIDDEN,
        ErrorType::PermissionError,
        "origin_invalid",
        "The request origin is not allowed.",
    )
    .into_response()
}

fn is_safe(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn origin_matches(headers: &HeaderMap, public: &Url) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .is_some_and(|o| o == origin_of(public))
}

/// A fixed-window rate limiter for one process. The MVP runs one instance
/// at most (design doc §8.8), so memory is enough.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<Windows>>,
    per_client: u32,
    total: u32,
    window: Duration,
}

#[derive(Debug, Default)]
struct Windows {
    started: Option<Instant>,
    total: u32,
    clients: HashMap<String, u32>,
}

impl RateLimiter {
    /// Allows `per_client` requests per client and `total` requests in all,
    /// in each `window`.
    pub fn new(per_client: u32, total: u32, window: Duration) -> Self {
        Self {
            inner: Arc::default(),
            per_client,
            total,
            window,
        }
    }

    /// Counts one request. Returns false if a limit is reached.
    pub fn check(&self, client: &str, now: Instant) -> bool {
        let Ok(mut w) = self.inner.lock() else {
            return false;
        };
        if w.started
            .is_none_or(|s| now.duration_since(s) >= self.window)
        {
            *w = Windows {
                started: Some(now),
                ..Windows::default()
            };
        }
        if w.total >= self.total {
            return false;
        }
        let count = w.clients.entry(client.to_owned()).or_insert(0);
        if *count >= self.per_client {
            return false;
        }
        *count += 1;
        w.total += 1;
        true
    }
}

/// The client address. Cloud Run's front end appends the address it sees to
/// `X-Forwarded-For`, so the last entry is the one a client cannot forge.
pub fn client_key(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .map_or_else(|| "unknown".to_owned(), |v| v.trim().to_owned())
}

/// Limits sign-in requests (design doc §12.1).
pub async fn auth_rate_limit(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !is_safe(request.method())
        && !state
            .auth_limiter
            .check(&client_key(request.headers()), Instant::now())
    {
        tracing::warn!(path = %request.uri().path(), "auth rate limit reached");
        return ApiError::rate_limited().into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_counts_per_client_and_total() {
        let limiter = RateLimiter::new(2, 3, Duration::from_mins(1));
        let t0 = Instant::now();
        assert!(limiter.check("a", t0));
        assert!(limiter.check("a", t0));
        assert!(!limiter.check("a", t0));
        assert!(limiter.check("b", t0));
        assert!(!limiter.check("c", t0), "total limit");
        let later = t0 + Duration::from_mins(1);
        assert!(limiter.check("a", later), "new window");
    }

    #[test]
    fn client_key_uses_the_last_forwarded_address() {
        let mut headers = HeaderMap::new();
        assert_eq!(client_key(&headers), "unknown");
        headers.insert(
            "x-forwarded-for",
            "1.1.1.1, 203.0.113.9"
                .parse()
                .unwrap_or_else(|_| unreachable!()),
        );
        assert_eq!(client_key(&headers), "203.0.113.9");
    }
}
