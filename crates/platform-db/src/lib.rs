//! `platform-db`: the PostgreSQL pool, migrations, and the job queue.
//!
//! Design doc §8.6 and §9. Phase 1 adds RLS session setup and the outbox relay.

pub mod jobs;

use std::time::Duration;

pub use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The environment variable that holds the connection URL.
pub const DATABASE_URL_ENV: &str = "DATABASE_URL";

/// The migrations in `crates/platform-db/migrations`, embedded at compile time.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// Settings for a connection pool.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// The `application_name` that PostgreSQL shows for each connection.
    pub application_name: &'static str,
    /// The maximum number of open connections.
    pub max_connections: u32,
    /// How long a caller waits for a connection. A serverless database can
    /// take a few seconds to start, so keep this above 5 s.
    pub acquire_timeout: Duration,
}

impl PoolConfig {
    /// Default settings for `application_name`.
    pub fn new(application_name: &'static str) -> Self {
        Self {
            application_name,
            max_connections: 5,
            acquire_timeout: Duration::from_secs(10),
        }
    }
}

/// Creates a pool that opens connections on first use.
///
/// The process starts even when the database is not available. Readiness
/// checks ([`ping`]) report the database state.
///
/// Idle connections close after 60 s. This lets a serverless database scale
/// to zero.
///
/// # Errors
///
/// Returns an error if `url` is not a valid PostgreSQL connection URL.
pub fn connect_lazy(url: &str, config: &PoolConfig) -> Result<PgPool, sqlx::Error> {
    let options: PgConnectOptions = url.parse()?;
    let options = options.application_name(config.application_name);
    Ok(PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(0)
        .acquire_timeout(config.acquire_timeout)
        .idle_timeout(Some(Duration::from_secs(60)))
        .max_lifetime(Some(Duration::from_mins(30)))
        .connect_lazy_with(options))
}

/// Runs `SELECT 1` to check that the database accepts queries.
///
/// # Errors
///
/// Returns an error if the pool cannot get a connection or the query fails.
pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1").execute(pool).await.map(|_| ())
}

/// Applies all pending migrations. Migrations are forward-only (doc §9.3).
///
/// # Errors
///
/// Returns an error if a migration fails or an applied migration was changed.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}
