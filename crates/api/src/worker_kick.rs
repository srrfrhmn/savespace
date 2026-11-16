//! Starts the worker job right away when urgent work is queued (D15).
//!
//! The MVP worker is a Cloud Run job that runs every hour. After a new
//! connection or a webhook, the person should not wait an hour, so the API
//! asks the Cloud Run Admin API to run the job now. The api service account
//! has `roles/run.invoker` on that one job (Terraform).
//!
//! A kick is best effort: a failure is logged, and the hourly run picks up
//! the work. Kicks within 20 seconds of the last one are skipped, because a
//! running worker drains the whole queue.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// The environment variable with the job's full name:
/// `projects/<project>/locations/<region>/jobs/<job>`.
pub const WORKER_JOB_ENV: &str = "SAVESPACE_WORKER_JOB";

const DEBOUNCE: Duration = Duration::from_secs(20);
const METADATA_TOKEN_URL: &str =
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token";

/// Starts the worker job.
#[derive(Clone, Debug, Default)]
pub struct WorkerKick {
    inner: Option<Arc<Inner>>,
}

#[derive(Debug)]
struct Inner {
    http: reqwest::Client,
    job: String,
    last: Mutex<Option<Instant>>,
    token: Mutex<Option<(String, Instant)>>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

impl WorkerKick {
    /// No kicks. Local development runs `worker run`, which polls.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Reads `SAVESPACE_WORKER_JOB`. Empty or unset disables kicks.
    pub fn from_env() -> Self {
        let Some(job) = std::env::var(WORKER_JOB_ENV)
            .ok()
            .filter(|j| !j.trim().is_empty())
        else {
            return Self::disabled();
        };
        let Ok(http) = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        else {
            tracing::warn!("cannot build the HTTP client; worker kicks are off");
            return Self::disabled();
        };
        Self {
            inner: Some(Arc::new(Inner {
                http,
                job: job.trim().to_owned(),
                last: Mutex::new(None),
                token: Mutex::new(None),
            })),
        }
    }

    /// Starts the worker job, unless one started in the last 20 seconds.
    /// Never fails: errors are logged.
    pub async fn kick(&self) {
        let Some(inner) = &self.inner else {
            return;
        };
        {
            let Ok(mut last) = inner.last.lock() else {
                return;
            };
            if last.is_some_and(|t| t.elapsed() < DEBOUNCE) {
                return;
            }
            *last = Some(Instant::now());
        }
        if let Err(e) = inner.run_job().await {
            tracing::warn!(error = %e, "cannot start the worker job; the hourly run will do the work");
        } else {
            tracing::info!("worker job started");
        }
    }
}

impl Inner {
    async fn access_token(&self) -> Result<String, String> {
        if let Ok(cached) = self.token.lock()
            && let Some((token, expires)) = cached.as_ref()
            && Instant::now() < *expires
        {
            return Ok(token.clone());
        }
        let response: TokenResponse = self
            .http
            .get(METADATA_TOKEN_URL)
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .map_err(|e| format!("metadata token: {e}"))?
            .error_for_status()
            .map_err(|e| format!("metadata token: {e}"))?
            .json()
            .await
            .map_err(|e| format!("metadata token: {e}"))?;
        // Refresh one minute early.
        let expires = Instant::now() + Duration::from_secs(response.expires_in.saturating_sub(60));
        if let Ok(mut cached) = self.token.lock() {
            *cached = Some((response.access_token.clone(), expires));
        }
        Ok(response.access_token)
    }

    async fn run_job(&self) -> Result<(), String> {
        let token = self.access_token().await?;
        let url = format!("https://run.googleapis.com/v2/{}:run", self.job);
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| format!("run job: {e}"))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("run job: status {}", response.status()))
        }
    }
}
