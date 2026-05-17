//! Workspace-wide HTTP machinery (PRD §F-003, §F-007).
//!
//! The locked retry policy ("3 retries with backoff `[1s, 2s, 4s]` on 5xx,
//! 429, and transient transport errors") and the locked User-Agent string
//! (PRD §F-003) are honored uniformly by every crate that talks to an
//! external HTTP service: metadata providers (`kino-metadata`), Stremio
//! addons (`kino-addons`), and any future caller that needs the same
//! semantics.
//!
//! The module was originally introduced in `kino-metadata::http` (Session
//! 004); F-007 (Session 008) lifted it into `kino-core` so the addon
//! protocol client could share it without an inverted dependency. See
//! ADR-055.

use std::time::Duration;

use reqwest::{Client, RequestBuilder, Response, StatusCode};
use tracing::debug;

use crate::constants::{HTTP_RETRY_BACKOFF_S, HTTP_TIMEOUT_S};

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

/// Errors that can surface from [`fetch_with_retry`] or
/// [`HttpConfig::build_client`].
///
/// Per-crate error types convert from this via `From<HttpError>` —
/// `kino-metadata::Error` and `kino-addons::AddonError` both implement that
/// bridge so callers can use `?` uniformly.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// Underlying transport failure (connect, timeout, TLS, request build).
    /// The retry policy converts transient instances of this into a final
    /// `Network` only after the backoff schedule is exhausted.
    #[error("network: {0}")]
    Network(#[from] reqwest::Error),

    /// The remote returned a non-2xx status that the retry policy did not
    /// recover from. `body` is the response body trimmed to a sensible size.
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
}

impl HttpError {
    /// Build an [`HttpError::Http`] from a status code and response body.
    /// Long bodies are truncated to 512 bytes on a UTF-8 char boundary.
    pub fn http_status(status: StatusCode, body: String) -> Self {
        const MAX_BODY: usize = 512;
        let mut body = body;
        if body.len() > MAX_BODY {
            let mut end = MAX_BODY;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            body.truncate(end);
            body.push('…');
        }
        Self::Http {
            status: status.as_u16(),
            body,
        }
    }
}

/// HTTP behavior knobs. Defaults are PRD-locked; tests construct a
/// shorter-backoff variant via [`HttpConfig::for_test`].
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
    /// User-Agent applied. Each provider/addon client owns one of these.
    pub fn build_client(&self) -> Result<Client, HttpError> {
        Client::builder()
            .user_agent(&self.user_agent)
            .timeout(self.timeout)
            .build()
            .map_err(HttpError::from)
    }

    /// Test-only constructor: zero backoffs to keep retry tests fast. Public
    /// so test crates outside `kino-core` (`kino-metadata`, `kino-addons`)
    /// can use the same helper.
    #[must_use]
    pub fn for_test() -> Self {
        Self {
            user_agent: USER_AGENT.to_string(),
            timeout: Duration::from_millis(500),
            backoff: vec![Duration::ZERO; 3],
        }
    }
}

/// Send a request, retrying on 5xx, 429, and transient transport errors
/// according to `config.backoff`. On terminal failure returns
/// [`HttpError::Http`] or [`HttpError::Network`].
///
/// The closure is called once per attempt so the [`RequestBuilder`] doesn't
/// have to be `Clone` — callers can freely use `query` / `json` / `header`
/// methods that consume the builder.
pub async fn fetch_with_retry<F>(build: F, config: &HttpConfig) -> Result<Response, HttpError>
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
            Err(e) => return Err(HttpError::from(e)),
        };
        let Some(delay) = backoff_iter.next() else {
            return match pending {
                PendingRetry::Status(r) => Err(http_error(r).await),
                PendingRetry::Network(e) => Err(HttpError::from(e)),
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

async fn http_error(r: Response) -> HttpError {
    let status = r.status();
    let body = r.text().await.unwrap_or_default();
    HttpError::http_status(status, body)
}
