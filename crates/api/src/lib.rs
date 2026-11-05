//! `api`: the SaveSpace HTTP API.
//!
//! Design doc §8.2 and §11. This crate holds the HTTP layer only. Domain
//! crates hold the logic.

use std::net::SocketAddr;

use axum::http::{HeaderName, Request};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::request_id::{
    MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::trace::TraceLayer;

/// The response header that carries the request ID (design doc §11.4).
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("request-id");

/// The environment variable for the listen address.
pub const ADDR_ENV: &str = "SAVESPACE_API_ADDR";

/// Runtime configuration for the API.
#[derive(Debug, Clone)]
pub struct Config {
    /// The socket address to listen on.
    pub addr: SocketAddr,
}

impl Config {
    /// Reads the configuration from the environment.
    ///
    /// `SAVESPACE_API_ADDR` sets the listen address. The default is
    /// `127.0.0.1:8080`.
    ///
    /// # Errors
    ///
    /// Returns an error if `SAVESPACE_API_ADDR` is not a valid socket address.
    pub fn from_env() -> anyhow::Result<Self> {
        let raw = std::env::var(ADDR_ENV).unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
        let addr = raw
            .parse()
            .map_err(|e| anyhow::anyhow!("{ADDR_ENV}={raw:?} is not a socket address: {e}"))?;
        Ok(Self { addr })
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

async fn healthz() -> Json<Health> {
    Json(Health {
        status: "ok",
        core_version: savespace_core::VERSION,
    })
}

/// Builds the HTTP router with all routes and middleware.
///
/// Tests call this function directly, without a network socket.
pub fn build_router() -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER))
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::new(REQUEST_ID_HEADER, MakeReqId))
}
