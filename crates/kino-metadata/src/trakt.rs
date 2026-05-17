//! Trakt v2 client (PRD §F-003, §F-004).
//!
//! Trakt application auth uses two headers: `trakt-api-key` (the application
//! key from `settings.trakt_api_key`) and `trakt-api-version: 2`. OAuth tokens
//! are not needed for the read-only catalog calls F-004 uses.

use serde::Deserialize;

use crate::error::Error;
use crate::http::{fetch_with_retry, HttpConfig};
use crate::trending::ProviderItem;
use kino_core::title::{TitleKind, TitleSummary};

/// Production Trakt v2 base URL.
pub const TRAKT_BASE_URL: &str = "https://api.trakt.tv";

/// Maximum trending items the F-004 aggregator asks for per provider.
const TRENDING_LIMIT: usize = 100;

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

    /// Fetch the trending movies for the F-004 aggregator.
    ///
    /// Calls `/movies/trending?limit=100`. The response carries items in
    /// trending order (best first); the aggregator uses the array position
    /// as the per-provider rank.
    pub async fn trending_movies(&self) -> Result<Vec<ProviderItem>, Error> {
        let url = format!("{}/movies/trending", self.base_url);
        let raw: Vec<MovieTrendingEntry> = self.fetch_trending(&url).await?;
        Ok(raw
            .into_iter()
            .take(TRENDING_LIMIT)
            .enumerate()
            .map(|(rank, e)| e.movie.into_provider(TitleKind::Movie, rank))
            .collect())
    }

    /// Fetch the trending TV shows for the F-004 aggregator.
    ///
    /// Calls `/shows/trending?limit=100`.
    pub async fn trending_shows(&self) -> Result<Vec<ProviderItem>, Error> {
        let url = format!("{}/shows/trending", self.base_url);
        let raw: Vec<ShowTrendingEntry> = self.fetch_trending(&url).await?;
        Ok(raw
            .into_iter()
            .take(TRENDING_LIMIT)
            .enumerate()
            .map(|(rank, e)| e.show.into_provider(TitleKind::Series, rank))
            .collect())
    }

    async fn fetch_trending<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T, Error> {
        let response = fetch_with_retry(
            || {
                self.client
                    .get(url)
                    .header("trakt-api-version", "2")
                    .header("trakt-api-key", self.key.as_str())
                    .query(&[("limit", TRENDING_LIMIT.to_string().as_str())])
            },
            &self.config,
        )
        .await?;
        response
            .json::<T>()
            .await
            .map_err(|e| Error::Decode(format!("trakt trending: {e}")))
    }
}

/// `/movies/trending` returns `[{ watchers, movie: { ids, title, year } }, ..]`.
#[derive(Debug, Deserialize)]
struct MovieTrendingEntry {
    movie: TitleEntry,
}

/// `/shows/trending` returns `[{ watchers, show: { ids, title, year } }, ..]`.
#[derive(Debug, Deserialize)]
struct ShowTrendingEntry {
    show: TitleEntry,
}

/// Shared shape for both movie and show entries.
#[derive(Debug, Deserialize)]
struct TitleEntry {
    title: String,
    #[serde(default)]
    year: Option<u16>,
    ids: TitleIds,
}

#[derive(Debug, Deserialize)]
struct TitleIds {
    #[serde(default)]
    imdb: Option<String>,
    #[serde(default)]
    tmdb: Option<u64>,
}

impl TitleEntry {
    fn into_provider(self, kind: TitleKind, rank: usize) -> ProviderItem {
        // Prefer IMDb for cross-provider dedup (PRD §F-004 step 2). Fall back
        // to TMDB id, then to a Trakt-local synthesized id so two Trakt-only
        // entries don't collide with each other.
        let id = if let Some(imdb) = self.ids.imdb.clone() {
            imdb
        } else if let Some(tmdb) = self.ids.tmdb {
            format!("tmdb:{tmdb}")
        } else {
            format!("trakt-rank:{rank}")
        };
        let summary = TitleSummary {
            id,
            kind,
            title: self.title,
            year: self.year,
            poster: None,
            rating: None,
        };
        ProviderItem {
            summary,
            rank,
            popularity: None,
            rating: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
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
    async fn trending_movies_uses_imdb_id_when_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movies/trending"))
            .and(header("trakt-api-version", "2"))
            .and(header("trakt-api-key", "test-key"))
            .and(query_param("limit", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "watchers": 100,
                    "movie": {
                        "title": "Movie A",
                        "year": 2024,
                        "ids": { "imdb": "tt1234567", "tmdb": 555 }
                    }
                },
                {
                    "watchers": 50,
                    "movie": {
                        "title": "Movie B",
                        "year": 2023,
                        "ids": { "imdb": null, "tmdb": 777 }
                    }
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.trending_movies().await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].summary.id, "tt1234567");
        assert_eq!(items[0].rank, 0);
        assert_eq!(items[0].summary.year, Some(2024));
        assert_eq!(items[1].summary.id, "tmdb:777");
        assert_eq!(items[1].rank, 1);
    }

    #[tokio::test]
    async fn trending_shows_uses_show_wrapper() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/shows/trending"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "watchers": 200,
                    "show": {
                        "title": "Show A",
                        "year": 2025,
                        "ids": { "imdb": "tt7777777" }
                    }
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.trending_shows().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].summary.id, "tt7777777");
        assert_eq!(items[0].summary.kind, TitleKind::Series);
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
