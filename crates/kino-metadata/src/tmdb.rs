//! TMDB v3 client (PRD §F-003, §F-004).
//!
//! TMDB serves trending, search, find, movie, tv, and configuration. This
//! module exposes the credential-test endpoint (F-003) and the weekly
//! trending catalogs consumed by the F-004 aggregator. Detail / search land
//! with F-010 and F-011.
//!
//! Authentication: the v3 `api_key` is passed as a query parameter on every
//! request. Stored in `settings.tmdb_api_key` (see [`crate::TMDB_API_KEY`]).

use serde::Deserialize;

use crate::error::Error;
use crate::http::{fetch_with_retry, HttpConfig};
use crate::trending::ProviderItem;
use kino_core::title::{TitleKind, TitleSummary};

/// Production TMDB base URL.
pub const TMDB_BASE_URL: &str = "https://api.themoviedb.org";

/// Maximum trending items the F-004 aggregator asks for per provider.
const TRENDING_LIMIT: usize = 100;

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

    /// Fetch the trending movies for the past week (PRD §F-004 step 1).
    ///
    /// `locale` is the BCP-47 language tag forwarded to TMDB's `language`
    /// query parameter so titles come back localized when possible. TMDB's
    /// `language` only affects the `title` / `overview` fields; the ranking
    /// itself is global. Items come back already ranked by trending position
    /// (best first); the aggregator assigns `rank = 0` to the first.
    pub async fn trending_movies(&self, locale: &str) -> Result<Vec<ProviderItem>, Error> {
        self.fetch_trending("movie", locale, TitleKind::Movie).await
    }

    /// Fetch the trending TV shows for the past week (PRD §F-004 step 1).
    /// See [`trending_movies`](Self::trending_movies) for the locale rules.
    pub async fn trending_shows(&self, locale: &str) -> Result<Vec<ProviderItem>, Error> {
        self.fetch_trending("tv", locale, TitleKind::Series).await
    }

    async fn fetch_trending(
        &self,
        media_type: &'static str,
        locale: &str,
        kind: TitleKind,
    ) -> Result<Vec<ProviderItem>, Error> {
        let url = format!("{}/3/trending/{media_type}/week", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str()), ("language", locale)])
            },
            &self.config,
        )
        .await?;
        let body: TrendingResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb trending {media_type}: {e}")))?;
        Ok(body
            .results
            .into_iter()
            .take(TRENDING_LIMIT)
            .enumerate()
            .map(|(rank, item)| item.into_provider(kind, rank))
            .collect())
    }
}

/// Shape of the `/3/trending/{type}/week` response, narrowed to the fields
/// F-004 cares about. Unknown fields are ignored by serde so TMDB schema
/// growth doesn't break us.
#[derive(Debug, Deserialize)]
struct TrendingResponse {
    results: Vec<TrendingItem>,
}

#[derive(Debug, Deserialize)]
struct TrendingItem {
    id: u64,
    /// Movies use `title`; TV uses `name`. We accept whichever the response
    /// carries via the two optional fields and synthesize the displayable
    /// string below.
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// Date strings; movies use `release_date`, TV uses `first_air_date`.
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    vote_average: Option<f64>,
    #[serde(default)]
    popularity: Option<f64>,
}

impl TrendingItem {
    fn into_provider(self, kind: TitleKind, rank: usize) -> ProviderItem {
        let title = self.title.or(self.name).unwrap_or_default();
        let year = self
            .release_date
            .as_deref()
            .or(self.first_air_date.as_deref())
            .and_then(parse_year);
        let poster = self.poster_path.as_deref().map(tmdb_poster_url);
        let summary = TitleSummary {
            id: format!("tmdb:{}", self.id),
            kind,
            title,
            year,
            poster,
            rating: self.vote_average,
        };
        ProviderItem {
            summary,
            rank,
            popularity: self.popularity,
            rating: self.vote_average,
        }
    }
}

/// Parse a leading `YYYY` from an ISO-8601 date string. TMDB returns dates
/// as `YYYY-MM-DD`; some catalog rows have an empty string. Out-of-range
/// values (negative years, etc.) yield `None`.
fn parse_year(date: &str) -> Option<u16> {
    let year_str = date.split('-').next().unwrap_or("");
    if year_str.len() != 4 {
        return None;
    }
    year_str.parse::<u16>().ok()
}

/// Build a TMDB poster URL at the `w500` size. The F-005 image resolver will
/// replace this with the proper provider-fallback chain; until then the
/// catalog UI gets a useful poster placeholder straight from the trending
/// fetch.
fn tmdb_poster_url(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    format!("https://image.tmdb.org/t/p/w500/{trimmed}")
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
    async fn trending_movies_parses_results_and_assigns_ranks() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/trending/movie/week"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("language", "en-US"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {
                        "id": 1,
                        "title": "First",
                        "release_date": "2025-04-01",
                        "poster_path": "/p1.jpg",
                        "vote_average": 8.4,
                        "popularity": 120.5,
                    },
                    {
                        "id": 2,
                        "title": "Second",
                        "release_date": "",
                        "poster_path": "",
                        "vote_average": 7.0,
                        "popularity": 80.0,
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.trending_movies("en-US").await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].rank, 0);
        assert_eq!(items[0].summary.id, "tmdb:1");
        assert_eq!(items[0].summary.title, "First");
        assert_eq!(items[0].summary.year, Some(2025));
        assert_eq!(
            items[0].summary.poster.as_deref(),
            Some("https://image.tmdb.org/t/p/w500/p1.jpg")
        );
        assert_eq!(items[0].rating, Some(8.4));
        assert_eq!(items[0].popularity, Some(120.5));

        assert_eq!(items[1].rank, 1);
        assert_eq!(items[1].summary.year, None);
        // Empty poster_path normalizes to a URL too — the F-005 resolver
        // will replace it, but we don't drop the entry.
        assert!(items[1].summary.poster.is_some());
    }

    #[tokio::test]
    async fn trending_shows_uses_tv_endpoint_and_name_field() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/trending/tv/week"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [{
                    "id": 42,
                    "name": "Show",
                    "first_air_date": "2024-09-15",
                    "vote_average": 9.1,
                    "popularity": 220.0,
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.trending_shows("en-US").await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].summary.title, "Show");
        assert_eq!(items[0].summary.year, Some(2024));
        assert_eq!(items[0].summary.kind, TitleKind::Series);
    }

    #[tokio::test]
    async fn trending_caps_results_at_100() {
        let mut results = Vec::new();
        for i in 0..150 {
            results.push(json!({
                "id": i,
                "title": format!("T{i}"),
                "release_date": "2020-01-01",
                "vote_average": 5.0,
                "popularity": 10.0,
            }));
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/trending/movie/week"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "results": results })))
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.trending_movies("en-US").await.unwrap();
        assert_eq!(items.len(), 100);
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
