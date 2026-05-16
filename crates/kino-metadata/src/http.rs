//! Shared HTTP machinery for every metadata provider (PRD §F-003).
//!
//! Lives outside the per-provider modules so the locked retry policy and
//! User-Agent string are honored uniformly. Every provider builds its
//! `reqwest::Client` from an [`HttpConfig`] and sends requests through
//! [`fetch_with_retry`].

use std::time::Duration;

use kino_core::constants::{HTTP_RETRY_BACKOFF_S, HTTP_TIMEOUT_S};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use tracing::debug;

use crate::error::Error;

/// User-Agent string sent on every outbound request. PRD §F-003 locks the
/// shape to `kino/<version> (+<repo_url>)`. The version and repo URL come
/// from the workspace Cargo metadata at compile time, so a release bump
/// flows through automatically.
pub const USER_AGENT: &str = concat!(
    "kino/",
    env!("CARGO_PKG_VERSION"),
    " (+",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);

/// HTTP behavior knobs. Defaults are PRD-locked; tests construct a
/// shorter-backoff variant to keep test latency low.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// User-Agent header sent on every request. Default: [`USER_AGENT`].
    pub user_agent: String,
    /// Per-request timeout. Default: PRD §8 `HTTP_TIMEOUT_S` (10s).
    pub timeout: Duration,
    /// Sleeps inserted BEFORE retries 1..=N. PRD §F-003 / §8: `[1s, 2s, 4s]`
    /// so the request is attempted up to four times (1 initial + 3 retries).
    pub backoff: Vec<Duration>,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            user_agent: USER_AGENT.to_string(),
            timeout: Duration::from_secs(HTTP_TIMEOUT_S),
            backoff: HTTP_RETRY_BACKOFF_S
                .iter()
                .map(|s| Duration::from_secs(*s))
                .collect(),
        }
    }
}

impl HttpConfig {
    /// Build a fresh `reqwest::Client` with this config's timeout and
    /// User-Agent applied. Each provider client owns one of these.
    pub fn build_client(&self) -> Result<Client, Error> {
        Client::builder()
            .user_agent(&self.user_agent)
            .timeout(self.timeout)
            .build()
            .map_err(Error::from)
    }

    /// Test-only constructor: zero backoffs to keep retry tests fast.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            user_agent: USER_AGENT.to_string(),
            timeout: Duration::from_millis(500),
            backoff: vec![Duration::ZERO; 3],
        }
    }
}

/// Send a request, retrying on 5xx, 429, and transient transport errors
/// according to `config.backoff`. On terminal failure returns [`Error::Http`]
/// or [`Error::Network`].
///
/// The closure is called once per attempt so the [`RequestBuilder`] doesn't
/// have to be `Clone` — callers can freely use `query` / `json` / `header`
/// methods that consume the builder.
pub async fn fetch_with_retry<F>(build: F, config: &HttpConfig) -> Result<Response, Error>
where
    F: Fn() -> RequestBuilder,
{
    let mut backoff_iter = config.backoff.iter().copied();
    loop {
        let send_result = build().send().await;
        let pending = match send_result {
            Ok(r) if r.status().is_success() => return Ok(r),
            Ok(r) if should_retry_status(r.status()) => PendingRetry::Status(r),
            Ok(r) => return Err(http_error(r).await),
            Err(e) if is_transient_error(&e) => PendingRetry::Network(e),
            Err(e) => return Err(Error::from(e)),
        };
        let Some(delay) = backoff_iter.next() else {
            return match pending {
                PendingRetry::Status(r) => Err(http_error(r).await),
                PendingRetry::Network(e) => Err(Error::from(e)),
            };
        };
        match &pending {
            PendingRetry::Status(r) => {
                debug!(status = %r.status(), ?delay, "retrying transient HTTP status");
            }
            PendingRetry::Network(e) => {
                debug!(error = %e, ?delay, "retrying transient HTTP error");
            }
        }
        tokio::time::sleep(delay).await;
    }
}

/// Outcome of a single attempt that needs to wait out a backoff before
/// retrying. Holds onto the request artifact so the final error (after the
/// backoff is exhausted) carries the real status / network detail.
enum PendingRetry {
    Status(Response),
    Network(reqwest::Error),
}

fn should_retry_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn is_transient_error(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}

async fn http_error(r: Response) -> Error {
    let status = r.status();
    let body = r.text().await.unwrap_or_default();
    Error::http_status(status, body)
}
