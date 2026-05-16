//! Trakt v2 client (PRD §F-003).
//!
//! Trakt application auth uses two headers: `trakt-api-key` (the application
//! key from `settings.trakt_api_key`) and `trakt-api-version: 2`. OAuth tokens
//! are not needed for the read-only catalog calls F-004 will use.

use crate::error::Error;
use crate::http::{fetch_with_retry, HttpConfig};

/// Production Trakt v2 base URL.
pub const TRAKT_BASE_URL: &str = "https://api.trakt.tv";

/// Trakt v2 application API client.
#[derive(Debug, Clone)]
pub struct TraktClient {
    key: String,
    base_url: String,
    config: HttpConfig,
    client: reqwest::Client,
}

impl TraktClient {
    /// Build a client pointed at the production Trakt v2 endpoint.
    pub fn new(key: impl Into<String>) -> Result<Self, Error> {
        Self::with_options(key, HttpConfig::default(), TRAKT_BASE_URL.to_string())
    }

    /// Build a client with explicit `HttpConfig` and base URL (for tests
    /// and future cache wiring).
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

    /// Verify the stored API key against `/genres/movies` — a public
    /// read-only endpoint that nonetheless requires both Trakt application
    /// headers, so the call fails fast on a bad key.
    pub async fn test_credentials(&self) -> Result<(), Error> {
        let url = format!("{}/genres/movies", self.base_url);
        fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .header("trakt-api-version", "2")
                    .header("trakt-api-key", self.key.as_str())
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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_credentials_sends_required_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/genres/movies"))
            .and(header("trakt-api-version", "2"))
            .and(header("trakt-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        client.test_credentials().await.unwrap();
    }

    #[tokio::test]
    async fn test_credentials_reports_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/genres/movies"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TraktClient::with_options("nope", HttpConfig::for_test(), server.uri()).unwrap();
        let err = client.test_credentials().await.unwrap_err();
        match err {
            Error::Http { status, .. } => assert_eq!(status, 401),
            other => panic!("expected Http(401), got {other:?}"),
        }
    }
}
