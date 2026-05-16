//! TMDB v3 client (PRD §F-003).
//!
//! TMDB serves trending, search, find, movie, tv, and configuration. This
//! session ships only the credential-test endpoint; the catalog-fetching
//! methods land with F-004 (trending) and F-010 (title detail).
//!
//! Authentication: the v3 `api_key` is passed as a query parameter on every
//! request. Stored in `settings.tmdb_api_key` (see [`crate::TMDB_API_KEY`]).

use crate::error::Error;
use crate::http::{fetch_with_retry, HttpConfig};

/// Production TMDB base URL.
pub const TMDB_BASE_URL: &str = "https://api.themoviedb.org";

/// TMDB v3 API client.
#[derive(Debug, Clone)]
pub struct TmdbClient {
    key: String,
    base_url: String,
    config: HttpConfig,
    client: reqwest::Client,
}

impl TmdbClient {
    /// Build a client pointed at the production TMDB v3 endpoint.
    pub fn new(key: impl Into<String>) -> Result<Self, Error> {
        Self::with_options(key, HttpConfig::default(), TMDB_BASE_URL.to_string())
    }

    /// Build a client with an explicit `HttpConfig` and base URL. Used by
    /// tests (wiremock) and by future cache wiring (F-004+) that needs to
    /// inject a longer timeout.
    pub fn with_options(
        key: impl Into<String>,
        config: HttpConfig,
        base_url: String,
    ) -> Result<Self, Error> {
        let client = config.build_client()?;
        Ok(Self {
            key: key.into(),
            base_url,
            config,
            client,
        })
    }

    /// Verify the stored API key against `/3/configuration`. PRD §F-003
    /// requires this for every provider.
    pub async fn test_credentials(&self) -> Result<(), Error> {
        let url = format!("{}/3/configuration", self.base_url);
        fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str())])
            },
            &self.config,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;
    use wiremock::matchers::{header_regex, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config_zero_backoff() -> HttpConfig {
        HttpConfig::for_test()
    }

    #[tokio::test]
    async fn test_credentials_happy_path_sends_api_key_and_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .and(query_param("api_key", "test-key"))
            .and(header_regex(
                "user-agent",
                r"^kino/[\w\.\-\+]+ \(\+https?://[^)]+\)$",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"images": {}})))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        client.test_credentials().await.unwrap();
    }

    #[tokio::test]
    async fn test_credentials_retries_on_429_then_succeeds() {
        let server = MockServer::start().await;
        // First call returns 429; subsequent calls return 200.
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .with_priority(2)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", config_zero_backoff(), server.uri()).unwrap();
        client.test_credentials().await.unwrap();
    }

    #[tokio::test]
    async fn test_credentials_retries_on_500_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .with_priority(2)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", config_zero_backoff(), server.uri()).unwrap();
        client.test_credentials().await.unwrap();
    }

    #[tokio::test]
    async fn test_credentials_returns_error_after_exhausted_retries() {
        let server = MockServer::start().await;
        // 4 attempts (1 initial + 3 retries) all return 500.
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .expect(4)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", config_zero_backoff(), server.uri()).unwrap();
        let err = client.test_credentials().await.unwrap_err();
        match err {
            Error::Http { status, body } => {
                assert_eq!(status, 500);
                assert!(body.contains("boom"));
            }
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_credentials_returns_error_on_timeout_exhausted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(500)))
            .expect(4)
            .mount(&server)
            .await;

        let config = HttpConfig {
            timeout: Duration::from_millis(50),
            backoff: vec![Duration::ZERO; 3],
            ..HttpConfig::default()
        };
        let client = TmdbClient::with_options("test-key", config, server.uri()).unwrap();
        let err = client.test_credentials().await.unwrap_err();
        assert!(
            matches!(err, Error::Network(_)),
            "expected Network/timeout error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_credentials_does_not_retry_on_401() {
        let server = MockServer::start().await;
        // Exactly one attempt — 4xx other than 429 must not retry.
        Mock::given(method("GET"))
            .and(path("/3/configuration"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .expect(1)
            .mount(&server)
            .await;

        let client = TmdbClient::with_options("nope", config_zero_backoff(), server.uri()).unwrap();
        let err = client.test_credentials().await.unwrap_err();
        match err {
            Error::Http { status, .. } => assert_eq!(status, 401),
            other => panic!("expected Http error, got {other:?}"),
        }
    }
}
