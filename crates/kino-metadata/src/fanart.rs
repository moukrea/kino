//! Fanart.tv v3 client (PRD §F-003).
//!
//! Fanart.tv exposes artwork via `webservice.fanart.tv/v3/{movies,tv}/<id>`.
//! Authentication is a single `api_key` query parameter; absence or a wrong
//! key yields a 401/403.
//!
//! [`FanartClient::test_credentials`] hits `/v3/movies/tt0111161` (Shawshank,
//! a stable known-good `IMDb` id) to confirm the key works without coupling
//! the test to any other catalog state.

use crate::error::Error;
use crate::http::{fetch_with_retry, HttpConfig};

/// Production Fanart.tv v3 base URL.
pub const FANART_BASE_URL: &str = "https://webservice.fanart.tv";

/// Known-good `IMDb` id used as the `test_credentials` probe.
const TEST_CREDENTIALS_IMDB_ID: &str = "tt0111161";

/// Fanart.tv v3 API client.
#[derive(Debug, Clone)]
pub struct FanartClient {
    key: String,
    base_url: String,
    config: HttpConfig,
    client: reqwest::Client,
}

impl FanartClient {
    /// Build a client pointed at the production Fanart.tv v3 endpoint.
    pub fn new(key: impl Into<String>) -> Result<Self, Error> {
        Self::with_options(key, HttpConfig::default(), FANART_BASE_URL.to_string())
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

    /// Verify the stored API key against a known-good movie endpoint.
    pub async fn test_credentials(&self) -> Result<(), Error> {
        let url = format!("{}/v3/movies/{}", self.base_url, TEST_CREDENTIALS_IMDB_ID);
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
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_credentials_sends_api_key_query_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/movies/tt0111161"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "The Shawshank Redemption",
                "imdb_id": "tt0111161"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            FanartClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        client.test_credentials().await.unwrap();
    }

    #[tokio::test]
    async fn test_credentials_reports_403() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/movies/tt0111161"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            FanartClient::with_options("nope", HttpConfig::for_test(), server.uri()).unwrap();
        let err = client.test_credentials().await.unwrap_err();
        match err {
            Error::Http { status, .. } => assert_eq!(status, 403),
            other => panic!("expected Http(403), got {other:?}"),
        }
    }
}
