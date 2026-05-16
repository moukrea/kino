//! TVDB v4 client (PRD §F-003).
//!
//! TVDB v4 requires a token-exchange: `POST /v4/login` with the API key in
//! the request body returns a short-lived Bearer token. Subsequent catalog
//! calls (added in F-004+) attach that token as `Authorization: Bearer <t>`.
//! [`TvdbClient::test_credentials`] performs the login and reports the
//! outcome — that's the only call F-003 makes, so no token caching is
//! needed yet.

use crate::error::Error;
use crate::http::{fetch_with_retry, HttpConfig};

/// Production TVDB v4 base URL.
pub const TVDB_BASE_URL: &str = "https://api4.thetvdb.com";

/// TVDB v4 API client.
#[derive(Debug, Clone)]
pub struct TvdbClient {
    key: String,
    base_url: String,
    config: HttpConfig,
    client: reqwest::Client,
}

impl TvdbClient {
    /// Build a client pointed at the production TVDB v4 endpoint.
    pub fn new(key: impl Into<String>) -> Result<Self, Error> {
        Self::with_options(key, HttpConfig::default(), TVDB_BASE_URL.to_string())
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

    /// Verify the stored API key by performing the `POST /v4/login`
    /// exchange. A 200 means the key is accepted by TVDB.
    pub async fn test_credentials(&self) -> Result<(), Error> {
        let url = format!("{}/v4/login", self.base_url);
        let body = serde_json::json!({ "apikey": self.key });
        fetch_with_retry(|| self.client.post(&url).json(&body), &self.config).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_credentials_posts_apikey_in_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .and(body_json(json!({"apikey": "test-key"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": { "token": "tok" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TvdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        client.test_credentials().await.unwrap();
    }

    #[tokio::test]
    async fn test_credentials_reports_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TvdbClient::with_options("nope", HttpConfig::for_test(), server.uri()).unwrap();
        let err = client.test_credentials().await.unwrap_err();
        match err {
            Error::Http { status, .. } => assert_eq!(status, 401),
            other => panic!("expected Http(401), got {other:?}"),
        }
    }
}
