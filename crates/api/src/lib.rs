//! `api`: the SaveSpace HTTP API.
//!
//! Design doc §8.2 and §11. This crate holds the HTTP layer only. Domain
//! crates hold the logic.
//!
//! Routes:
//!
//! - `GET /health`: liveness. Does not use the database.
//! - `GET /ready`: readiness. Checks the database.
//! - `/v1/...`: the JSON API (Phase 1).
//! - Everything else: the web app (static files), if `SAVESPACE_WEB_DIR` is set.
//!
//! Do not use paths that end in `z` (for example `/healthz`). Cloud Run
//! reserves some of them.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use axum::extract::State;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use platform_db::PgPool;
use serde::Serialize;
use tower_http::request_id::{
    MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

/// The response header that carries the request ID (design doc §11.4).
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("request-id");

/// The environment variable for the listen address.
pub const ADDR_ENV: &str = "SAVESPACE_API_ADDR";

/// The environment variable for the web app build directory.
pub const WEB_DIR_ENV: &str = "SAVESPACE_WEB_DIR";

/// The longest time `/ready` waits for the database.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// The Content Security Policy for the web app.
///
/// `wasm-unsafe-eval` lets the page compile `savespace-core` WASM. It does not
/// allow JavaScript `eval`. Plaid Link (Phase 1) adds its script and frame
/// origins here.
const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
    style-src 'self'; img-src 'self' data:; connect-src 'self'; \
    frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'";

/// Runtime configuration for the API.
#[derive(Debug, Clone)]
pub struct Config {
    /// The socket address to listen on.
    pub addr: SocketAddr,
    /// The PostgreSQL connection URL (runtime role).
    pub database_url: String,
    /// The web app build directory. If `None`, the API serves no static files.
    pub web_dir: Option<PathBuf>,
}

impl Config {
    /// Reads the configuration from the environment.
    ///
    /// - `SAVESPACE_API_ADDR`: listen address. The default is `127.0.0.1:8080`.
    /// - `DATABASE_URL`: required.
    /// - `SAVESPACE_WEB_DIR`: optional web app build directory.
    ///
    /// # Errors
    ///
    /// Returns an error if `SAVESPACE_API_ADDR` is not a valid socket address
    /// or `DATABASE_URL` is not set.
    pub fn from_env() -> anyhow::Result<Self> {
        let raw = std::env::var(ADDR_ENV).unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
        let addr = raw
            .parse()
            .map_err(|e| anyhow::anyhow!("{ADDR_ENV}={raw:?} is not a socket address: {e}"))?;
        let database_url = std::env::var(platform_db::DATABASE_URL_ENV)
            .with_context(|| format!("{} is not set", platform_db::DATABASE_URL_ENV))?;
        let web_dir = std::env::var_os(WEB_DIR_ENV).map(PathBuf::from);
        Ok(Self {
            addr,
            database_url,
            web_dir,
        })
    }
}

/// Shared state for handlers.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The database pool (runtime role).
    pub db: PgPool,
}

/// Makes request IDs in the API ID style: `req_` plus a `UUIDv7`.
#[derive(Debug, Clone, Copy, Default)]
struct MakeReqId;

impl MakeRequestId for MakeReqId {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = format!("req_{}", uuid::Uuid::now_v7().simple());
        id.parse().ok().map(RequestId::new)
    }
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    core_version: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        core_version: savespace_core::VERSION,
    })
}

#[derive(Debug, Serialize)]
struct Ready {
    status: &'static str,
}

async fn ready(State(state): State<AppState>) -> Response {
    match tokio::time::timeout(READY_TIMEOUT, platform_db::ping(&state.db)).await {
        Ok(Ok(())) => Json(Ready { status: "ok" }).into_response(),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "database not ready");
            unavailable()
        }
        Err(_) => {
            tracing::warn!("database ready check timed out");
            unavailable()
        }
    }
}

fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(Ready {
            status: "unavailable",
        }),
    )
        .into_response()
}

/// The error body from design doc §11.4.
#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Serialize)]
struct ErrorDetail {
    #[serde(rename = "type")]
    kind: &'static str,
    code: &'static str,
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

async fn api_not_found(request_id: Option<Extension<RequestId>>) -> Response {
    let request_id =
        request_id.and_then(|Extension(id)| id.header_value().to_str().ok().map(str::to_owned));
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: ErrorDetail {
                kind: "not_found_error",
                code: "route_not_found",
                message: "No API route matches this method and path.",
                request_id,
            },
        }),
    )
        .into_response()
}

/// Builds the HTTP router with all routes and middleware.
///
/// Tests call this function directly, without a network socket.
pub fn build_router(state: AppState, web_dir: Option<&Path>) -> Router {
    // JSON responses can hold financial data. Never cache them.
    let json = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .nest("/v1", Router::new().fallback(api_not_found))
        .layer(SetResponseHeaderLayer::if_not_present(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .with_state(state);

    let router = match web_dir {
        Some(dir) => json.merge(web_app(dir)),
        None => json,
    };

    with_security_headers(router)
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER))
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::new(REQUEST_ID_HEADER, MakeReqId))
}

/// Serves the Vite build. Unknown paths return `index.html` (client routing).
///
/// Vite puts hashed files in `assets/`. A hashed file never changes, so it is
/// cached for one year. Other files are checked on each load.
fn web_app(dir: &Path) -> Router {
    let assets = Router::new()
        .fallback_service(ServeDir::new(dir.join("assets")))
        .layer(SetResponseHeaderLayer::overriding(
            CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        ));
    let spa = Router::new()
        .fallback_service(ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html"))))
        .layer(SetResponseHeaderLayer::if_not_present(
            CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ));
    Router::new().nest("/assets", assets).merge(spa)
}

/// Security headers for every response (design doc §12).
const SECURITY_HEADERS: [(HeaderName, &str); 6] = [
    (CONTENT_SECURITY_POLICY, CSP),
    (
        STRICT_TRANSPORT_SECURITY,
        "max-age=63072000; includeSubDomains",
    ),
    (X_CONTENT_TYPE_OPTIONS, "nosniff"),
    (REFERRER_POLICY, "no-referrer"),
    (
        HeaderName::from_static("cross-origin-opener-policy"),
        "same-origin",
    ),
    (
        HeaderName::from_static("permissions-policy"),
        "camera=(), microphone=(), geolocation=()",
    ),
];

fn with_security_headers(router: Router) -> Router {
    SECURITY_HEADERS
        .into_iter()
        .fold(router, |router, (name, value)| {
            router.layer(SetResponseHeaderLayer::if_not_present(
                name,
                HeaderValue::from_static(value),
            ))
        })
}
