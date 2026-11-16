//! `platform-observability`: logs and traces for SaveSpace services.
//!
//! Design doc §13.2. Phase 0 sets up `tracing` output. Phase 1 adds
//! OpenTelemetry export and the PII scrubber (doc §12.4).

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

/// The environment variable that selects the log format: `gcp`, `json`, or `pretty`.
pub const LOG_FORMAT_ENV: &str = "SAVESPACE_LOG_FORMAT";

/// The default filter if `RUST_LOG` is not set.
const DEFAULT_FILTER: &str = "info,tower_http=info";

/// Installs the global `tracing` subscriber for `service`.
///
/// - `RUST_LOG` sets the filter. The default is `info`.
/// - `SAVESPACE_LOG_FORMAT=gcp` writes one JSON object per line in the Google
///   Cloud Logging format (`severity`, `message`). Production uses this format.
/// - `SAVESPACE_LOG_FORMAT=json` writes one JSON object per line.
/// - Any other value writes human-readable output (development).
///
/// Call this function once, at the start of `main`. A second call has no effect.
pub fn init(service: &'static str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let format = std::env::var(LOG_FORMAT_ENV)
        .unwrap_or_default()
        .to_ascii_lowercase();

    let registry = tracing_subscriber::registry().with(filter);
    let result = match format.as_str() {
        "gcp" => registry.with(tracing_stackdriver::layer()).try_init(),
        "json" => registry
            .with(fmt::layer().json().with_current_span(true))
            .try_init(),
        _ => registry.with(fmt::layer()).try_init(),
    };
    if result.is_ok() {
        tracing::info!(service, "observability initialized");
    }
}
