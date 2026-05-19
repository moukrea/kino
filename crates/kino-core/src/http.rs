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

use reqwest::{header, Client, RequestBuilder, Response, StatusCode};
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
///
/// Callers that want to round-trip an `ETag` / `If-None-Match` for cache
/// revalidation (PRD §F-003) should use [`fetch_with_etag`] instead.
pub async fn fetch_with_retry<F>(build: F, config: &HttpConfig) -> Result<Response, HttpError>
where
    F: Fn() -> RequestBuilder,
{
    match fetch_with_etag(build, None, config).await? {
        FetchOutcome::Fresh { response, .. } => Ok(response),
        // `prior_etag = None` means the server cannot legally produce a 304
        // (no `If-None-Match` is sent). Treat it as defensive: surface as an
        // `Http { status: 304, ... }` so callers don't silently lose the
        // response.
        FetchOutcome::NotModified => Err(HttpError::Http {
            status: 304,
            body: "unexpected 304 without prior etag".to_string(),
        }),
    }
}

/// Outcome of a successful cache-aware fetch ([`fetch_with_etag`]).
///
/// PRD §F-003 locks "`ETag` handled where the provider supports it; stored
/// in `response_cache.etag`". A caller that has a prior cached row passes
/// its stored `ETag` as `prior_etag`; the server may reply with `304 Not
/// Modified`
/// (which surfaces as [`FetchOutcome::NotModified`]) and the caller re-uses
/// the cached payload while refreshing its expiry. A fresh `2xx` response
/// surfaces as [`FetchOutcome::Fresh`] with the optional `ETag` header
/// parsed out for the next write to `response_cache.etag`.
pub enum FetchOutcome {
    /// Server confirmed the cached payload is still current. The caller
    /// should re-use its existing cache row and refresh `expires_at`.
    NotModified,
    /// Server returned a fresh body. The caller should consume `response`
    /// (e.g. `.json()`, `.text()`) and persist `etag` alongside the new
    /// payload.
    Fresh {
        response: Response,
        etag: Option<String>,
    },
}

/// Send a request with optional `If-None-Match` revalidation, retrying on
/// 5xx / 429 / transient transport errors. PRD §F-003 `ETag` round-trip.
///
/// Behavior:
/// - When `prior_etag` is `Some`, an `If-None-Match: <etag>` header is added
///   to every attempt's request.
/// - A `2xx` response yields [`FetchOutcome::Fresh`] with the parsed `ETag`
///   header (if the server sent one).
/// - A `304 Not Modified` response yields [`FetchOutcome::NotModified`] and
///   does NOT trigger retry — it is the cache-hit success path.
/// - `5xx`, `429`, and transient transport errors retry per `config.backoff`
///   exactly as [`fetch_with_retry`] does.
///
/// The closure is called once per attempt so the [`RequestBuilder`] doesn't
/// have to be `Clone`.
pub async fn fetch_with_etag<F>(
    build: F,
    prior_etag: Option<&str>,
    config: &HttpConfig,
) -> Result<FetchOutcome, HttpError>
where
    F: Fn() -> RequestBuilder,
{
    let mut backoff_iter = config.backoff.iter().copied();
    loop {
        let request = match prior_etag {
            Some(etag) => build().header(header::IF_NONE_MATCH, etag),
            None => build(),
        };
        let send_result = request.send().await;
        let pending = match send_result {
            Ok(r) if r.status() == StatusCode::NOT_MODIFIED => {
                return Ok(FetchOutcome::NotModified);
            }
            Ok(r) if r.status().is_success() => {
                let etag = extract_etag(&r);
                return Ok(FetchOutcome::Fresh { response: r, etag });
            }
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

/// Parse the `ETag` response header into an owned string. Returns `None` if
/// the header is absent or contains bytes that aren't valid UTF-8. The value
/// is returned verbatim — including any surrounding quotes or `W/` weak
/// prefix — because RFC 7232 requires the next `If-None-Match` to echo it
/// byte-for-byte.
fn extract_etag(response: &Response) -> Option<String> {
    response
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config_zero_backoff() -> HttpConfig {
        HttpConfig::for_test()
    }

    #[tokio::test]
    async fn fetch_with_etag_no_prior_sends_no_if_none_match_and_returns_etag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"abc123\"")
                    .set_body_string("hello"),
            )
            .expect(1)
            .mount(&server)
            .await;
        // Assert no If-None-Match header reaches the server when prior is None.
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header_exists("if-none-match"))
            .respond_with(ResponseTemplate::new(599))
            .expect(0)
            .with_priority(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let url = format!("{}/x", server.uri());
        let outcome = fetch_with_etag(|| client.get(&url), None, &config_zero_backoff())
            .await
            .expect("fetch ok");
        match outcome {
            FetchOutcome::Fresh { response, etag } => {
                assert_eq!(etag.as_deref(), Some("\"abc123\""));
                let body = response.text().await.unwrap();
                assert_eq!(body, "hello");
            }
            FetchOutcome::NotModified => panic!("expected Fresh"),
        }
    }

    #[tokio::test]
    async fn fetch_with_etag_prior_etag_sends_if_none_match_and_304_yields_not_modified() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header("if-none-match", "\"abc123\""))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let url = format!("{}/x", server.uri());
        let outcome = fetch_with_etag(
            || client.get(&url),
            Some("\"abc123\""),
            &config_zero_backoff(),
        )
        .await
        .expect("fetch ok");
        assert!(matches!(outcome, FetchOutcome::NotModified));
    }

    #[tokio::test]
    async fn fetch_with_etag_prior_etag_with_changed_resource_yields_fresh_with_new_etag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .and(header("if-none-match", "\"old\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"new\"")
                    .set_body_string("updated"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let url = format!("{}/x", server.uri());
        let outcome = fetch_with_etag(|| client.get(&url), Some("\"old\""), &config_zero_backoff())
            .await
            .expect("fetch ok");
        match outcome {
            FetchOutcome::Fresh { response, etag } => {
                assert_eq!(etag.as_deref(), Some("\"new\""));
                let body = response.text().await.unwrap();
                assert_eq!(body, "updated");
            }
            FetchOutcome::NotModified => panic!("expected Fresh"),
        }
    }

    #[tokio::test]
    async fn fetch_with_etag_does_not_retry_on_304() {
        // PRD §F-003: 304 is the cache-hit success path, not a transient
        // failure. The wiremock `expect(1)` enforces no retry.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let url = format!("{}/x", server.uri());
        let outcome = fetch_with_etag(|| client.get(&url), Some("\"abc\""), &config_zero_backoff())
            .await
            .expect("fetch ok");
        assert!(matches!(outcome, FetchOutcome::NotModified));
    }

    #[tokio::test]
    async fn fetch_with_etag_retries_on_500_then_returns_fresh() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"v1\"")
                    .set_body_string("ok"),
            )
            .expect(1)
            .with_priority(2)
            .mount(&server)
            .await;

        let client = Client::new();
        let url = format!("{}/x", server.uri());
        let outcome = fetch_with_etag(|| client.get(&url), Some("\"v0\""), &config_zero_backoff())
            .await
            .expect("fetch ok");
        match outcome {
            FetchOutcome::Fresh { etag, .. } => assert_eq!(etag.as_deref(), Some("\"v1\"")),
            FetchOutcome::NotModified => panic!("expected Fresh"),
        }
    }

    #[tokio::test]
    async fn fetch_with_etag_missing_response_etag_yields_none() {
        // Fanart.tv and similar providers may not send an ETag header on
        // every endpoint. The infrastructure tolerates absence (PRD §F-003:
        // "ETag handled where the provider supports it" — providers that
        // don't simply leave the column NULL).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(200).set_body_string("body"))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let url = format!("{}/x", server.uri());
        let outcome = fetch_with_etag(|| client.get(&url), None, &config_zero_backoff())
            .await
            .expect("fetch ok");
        match outcome {
            FetchOutcome::Fresh { etag, .. } => assert!(etag.is_none()),
            FetchOutcome::NotModified => panic!("expected Fresh"),
        }
    }

    #[tokio::test]
    async fn fetch_with_retry_back_compat_treats_304_without_prior_as_http_error() {
        // Defensive: the back-compat wrapper sends no If-None-Match, so a
        // server that nonetheless replies 304 is surfaced as an HTTP error
        // (not silently lost). Real providers do not do this.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let url = format!("{}/x", server.uri());
        let err = fetch_with_retry(|| client.get(&url), &config_zero_backoff())
            .await
            .unwrap_err();
        match err {
            HttpError::Http { status, .. } => assert_eq!(status, 304),
            HttpError::Network(_) => panic!("expected HttpError::Http(304)"),
        }
    }
}
