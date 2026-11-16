//! `api`: the SaveSpace HTTP API.
//!
//! Design doc §8.2 and §11. This crate holds the HTTP layer only. Domain
//! crates hold the logic.
//!
//! Routes:
//!
//! - `GET /health`: liveness. Does not use the database.
//! - `GET /ready`: readiness. Checks the database.
//! - `/v1/...`: the JSON API. `GET /v1/openapi.json` describes it.
//! - Everything else: the web app (static files), if `SAVESPACE_WEB_DIR` is set.
//!
//! Do not use paths that end in `z` (for example `/healthz`). Cloud Run
//! reserves some of them.

pub mod error;
pub mod guard;
pub mod idempotency;
pub mod ids;
pub mod routes;
pub mod session;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::extract::State;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY,
    STRICT_TRANSPORT_SECURITY, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use domain_identity::{Identity, IdentityConfig};
use platform_db::PgPool;
use serde::Serialize;
use tower_http::request_id::{
    MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use url::Url;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;

use crate::error::{ApiError, ErrorType};
use crate::guard::RateLimiter;

/// The response header that carries the request ID (design doc §11.4).
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("request-id");

/// The environment variable for the listen address.
pub const ADDR_ENV: &str = "SAVESPACE_API_ADDR";

/// The environment variable for the web app build directory.
pub const WEB_DIR_ENV: &str = "SAVESPACE_WEB_DIR";

/// The environment variable for the public origin of the app.
pub const PUBLIC_ORIGIN_ENV: &str = "SAVESPACE_PUBLIC_ORIGIN";

/// The environment variable for the one-time owner setup code (D12).
pub const SETUP_CODE_ENV: &str = "SAVESPACE_OWNER_SETUP_CODE";

/// The longest time `/ready` waits for the database.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// The Content Security Policy for the web app.
///
/// `wasm-unsafe-eval` lets the page compile `savespace-core` WASM. It does not
/// allow JavaScript `eval`. Plaid Link (M2) adds its script and frame origins.
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
    /// The origin people use in the browser. Passkeys are bound to its host.
    pub public_origin: Url,
    /// The one-time owner setup code. `None` disables setup.
    pub setup_code: Option<String>,
}

impl Config {
    /// Reads the configuration from the environment.
    ///
    /// - `SAVESPACE_API_ADDR`: listen address. The default is `127.0.0.1:8080`.
    /// - `DATABASE_URL`: required.
    /// - `SAVESPACE_WEB_DIR`: optional web app build directory.
    /// - `SAVESPACE_PUBLIC_ORIGIN`: the URL people open, for example
    ///   `https://savespace-api-….run.app`. The default is
    ///   `http://localhost:8080`.
    /// - `SAVESPACE_OWNER_SETUP_CODE`: optional one-time setup code.
    ///
    /// # Errors
    ///
    /// Returns an error if `SAVESPACE_API_ADDR` is not a valid socket address,
    /// `DATABASE_URL` is not set, or `SAVESPACE_PUBLIC_ORIGIN` is not a URL.
    pub fn from_env() -> anyhow::Result<Self> {
        let raw = std::env::var(ADDR_ENV).unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
        let addr = raw
            .parse()
            .map_err(|e| anyhow::anyhow!("{ADDR_ENV}={raw:?} is not a socket address: {e}"))?;
        let database_url = std::env::var(platform_db::DATABASE_URL_ENV)
            .with_context(|| format!("{} is not set", platform_db::DATABASE_URL_ENV))?;
        let web_dir = std::env::var_os(WEB_DIR_ENV).map(PathBuf::from);
        let origin =
            std::env::var(PUBLIC_ORIGIN_ENV).unwrap_or_else(|_| "http://localhost:8080".into());
        let public_origin = Url::parse(&origin)
            .with_context(|| format!("{PUBLIC_ORIGIN_ENV}={origin:?} is not a URL"))?;
        let setup_code = std::env::var(SETUP_CODE_ENV)
            .ok()
            .filter(|c| !c.trim().is_empty());
        Ok(Self {
            addr,
            database_url,
            web_dir,
            public_origin,
            setup_code,
        })
    }
}

/// Shared state for handlers.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The database pool (runtime role).
    pub db: PgPool,
    /// Sign-in and sessions.
    pub identity: Identity,
    /// The origin people use in the browser.
    pub public_origin: Arc<Url>,
    /// The limiter for sign-in routes.
    pub auth_limiter: RateLimiter,
}

impl AppState {
    /// Creates the state.
    ///
    /// # Errors
    ///
    /// Returns an error if the origin is not valid for WebAuthn.
    pub fn new(db: PgPool, public_origin: Url, setup_code: Option<String>) -> anyhow::Result<Self> {
        let identity = Identity::new(
            db.clone(),
            &IdentityConfig {
                origin: public_origin.clone(),
                setup_code,
            },
        )
        .context("identity configuration")?;
        Ok(Self {
            db,
            identity,
            public_origin: Arc::new(public_origin),
            // 20 sign-in requests per minute per client, 100 in all.
            auth_limiter: RateLimiter::new(20, 100, Duration::from_mins(1)),
        })
    }
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

async fn api_not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        ErrorType::NotFoundError,
        "route_not_found",
        "No API route matches this method and path.",
    )
}

/// The `OpenAPI` document.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "SaveSpace API",
        description = "Design doc §11. Paths are relative to /v1."
    ),
    servers((url = "/v1")),
    tags(
        (name = "auth", description = "Owner setup, passkey sign-in, and step-up checks."),
        (name = "me", description = "The signed-in person.")
    )
)]
struct ApiDoc;

/// The `/v1` route groups before middleware, and their `OpenAPI` document.
struct V1 {
    /// Sign-in routes that work without a session.
    public_auth: Router<AppState>,
    /// Sign-in routes that need a session (step-up checks).
    session_auth: Router<AppState>,
    /// Other routes that need a session.
    signed_in: Router<AppState>,
    doc: utoipa::openapi::OpenApi,
}

fn v1_parts() -> V1 {
    let (public_auth, mut doc) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(routes::auth::public_routes())
        .split_for_parts();
    let (session_auth, d) = routes::auth::session_routes().split_for_parts();
    doc.merge(d);
    let (signed_in, d) = routes::me::routes().split_for_parts();
    doc.merge(d);
    V1 {
        public_auth,
        session_auth,
        signed_in,
        doc,
    }
}

/// The `/v1` routes with their middleware, and the `OpenAPI` document.
fn v1(state: &AppState) -> (Router<AppState>, utoipa::openapi::OpenApi) {
    let parts = v1_parts();
    let rate_limit = from_fn_with_state(state.clone(), guard::auth_rate_limit);
    // Layers run outside in: require_session, then idempotency.
    let signed_in = parts
        .session_auth
        .layer(rate_limit.clone())
        .merge(parts.signed_in)
        .layer(from_fn_with_state(state.clone(), idempotency::idempotency))
        .layer(from_fn_with_state(state.clone(), session::require_session));
    let router = parts.public_auth.layer(rate_limit).merge(signed_in);
    (router, parts.doc)
}

/// The `OpenAPI` document (design doc §11.1). A test checks that
/// `clients/openapi.json` matches it.
pub fn openapi() -> utoipa::openapi::OpenApi {
    v1_parts().doc
}

/// Builds the HTTP router with all routes and middleware.
///
/// Tests call this function directly, without a network socket.
pub fn build_router(state: AppState, web_dir: Option<&Path>) -> Router {
    let (api, openapi) = v1(&state);
    let spec: Arc<str> = Arc::from(openapi.to_json().unwrap_or_default());
    let api = api
        .route(
            "/openapi.json",
            get(move || {
                let spec = Arc::clone(&spec);
                async move { ([(CONTENT_TYPE, "application/json")], spec.to_string()) }
            }),
        )
        .fallback(api_not_found)
        .layer(from_fn_with_state(state.clone(), guard::same_origin));

    // JSON responses can hold financial data. Never cache them.
    let json = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .nest("/v1", api)
        .layer(SetResponseHeaderLayer::if_not_present(
            CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .with_state(state.clone());

    let router = match web_dir {
        Some(dir) => json.merge(web_app(dir)),
        None => json,
    };

    let router = router
        .layer(from_fn(error::render_errors))
        .layer(from_fn_with_state(state, guard::canonical_host));

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
