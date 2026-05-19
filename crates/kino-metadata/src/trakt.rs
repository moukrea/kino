//! Trakt v2 client (PRD §F-003, §F-004).
//!
//! Trakt application auth uses two headers: `trakt-api-key` (the application
//! key from `settings.trakt_api_key`) and `trakt-api-version: 2`. OAuth tokens
//! are not needed for the read-only catalog calls F-004 uses.

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::trending::ProviderItem;
use kino_core::http::{fetch_with_etag, fetch_with_retry, FetchOutcome, HttpConfig};
use kino_core::title::{TitleKind, TitleSummary};

/// Production Trakt v2 base URL.
pub const TRAKT_BASE_URL: &str = "https://api.trakt.tv";

/// Maximum trending items the F-004 aggregator asks for per provider.
const TRENDING_LIMIT: usize = 100;

/// Trakt user rating for a single title, wrapping the rating value so the
/// payload is self-describing on cache reads (PRD §F-010, §F-003).
///
/// `rating = None` covers both the "Trakt has no votes" case (HTTP 200 with
/// `rating: 0`) and the "Trakt doesn't know this title" case (HTTP 404).
/// Both are cached identically so the next read short-circuits without a
/// new HTTP attempt until the row's TTL elapses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraktTitleRating {
    pub imdb_id: String,
    pub kind: TitleKind,
    /// 0-10 scale; `None` when Trakt has no rating to report.
    pub rating: Option<f64>,
}

/// Result of [`TraktClient::title_rating_with_etag`] — either a fresh
/// rating (with the server's `ETag` header, when present) or the cache-hit
/// `304 Not Modified` signal that the caller's prior cached
/// [`TraktTitleRating`] is still current. PRD §F-003 round-trip.
#[derive(Debug, Clone, PartialEq)]
pub enum TraktTitleRatingFetch {
    /// Server confirmed the caller's cached rating is still current. The
    /// caller re-uses its existing [`TraktTitleRating`] and only refreshes
    /// the cache row's `expires_at`.
    NotModified,
    /// Server returned a fresh body (or a `404` we map to
    /// `rating: None`). `etag` is the parsed `ETag` header (or `None` if
    /// the endpoint didn't send one).
    Fresh {
        rating: TraktTitleRating,
        etag: Option<String>,
    },
}

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

    /// Fetch the Trakt user-rating for a single title (PRD §F-010 ratings
    /// row). Trakt's `/movies/{id}/ratings` and `/shows/{id}/ratings`
    /// endpoints accept either the Trakt slug, the Trakt numeric id, or
    /// the `IMDb` id directly — we always pass the `IMDb` id since that's
    /// the shape kino's `TitleIds` carries.
    ///
    /// Returns the `rating` field (0-10 scale) when Trakt has votes;
    /// `None` when the title is unknown to Trakt or no ratings have been
    /// recorded.
    pub async fn title_rating(&self, imdb_id: &str, kind: TitleKind) -> Result<Option<f64>, Error> {
        match self.title_rating_with_etag(imdb_id, kind, None).await? {
            TraktTitleRatingFetch::Fresh { rating, .. } => Ok(rating.rating),
            // Unreachable: `prior_etag = None` means the server cannot
            // legally produce a 304. Treat as decode error in case a
            // misbehaving mock yields one.
            TraktTitleRatingFetch::NotModified => Err(Error::Decode(
                "trakt title_rating: 304 returned without prior etag".to_string(),
            )),
        }
    }

    /// `ETag`-aware variant of [`title_rating`](Self::title_rating) for
    /// cache revalidation (PRD §F-003). Pass the cache row's stored `ETag`
    /// as `prior_etag`; on `304 Not Modified` the server confirms the
    /// cached rating is still current and the caller re-uses it
    /// (refreshing the row's `expires_at`). On a fresh `2xx`, the parsed
    /// `ETag` header is returned alongside the new payload for
    /// persistence. A `404` from Trakt (title unknown) is surfaced as
    /// `Fresh { rating: None, etag: None }` so the absence is cached for
    /// the TTL like any other negative result.
    pub async fn title_rating_with_etag(
        &self,
        imdb_id: &str,
        kind: TitleKind,
        prior_etag: Option<&str>,
    ) -> Result<TraktTitleRatingFetch, Error> {
        let segment = match kind {
            TitleKind::Movie => "movies",
            TitleKind::Series => "shows",
        };
        let url = format!("{}/{segment}/{imdb_id}/ratings", self.base_url);
        let outcome = match fetch_with_etag(
            || {
                self.client
                    .get(&url)
                    .header("trakt-api-version", "2")
                    .header("trakt-api-key", self.key.as_str())
            },
            prior_etag,
            &self.config,
        )
        .await
        {
            Ok(o) => o,
            Err(kino_core::http::HttpError::Http { status: 404, .. }) => {
                return Ok(TraktTitleRatingFetch::Fresh {
                    rating: TraktTitleRating {
                        imdb_id: imdb_id.to_string(),
                        kind,
                        rating: None,
                    },
                    etag: None,
                });
            }
            Err(e) => return Err(e.into()),
        };
        let (response, etag) = match outcome {
            FetchOutcome::NotModified => return Ok(TraktTitleRatingFetch::NotModified),
            FetchOutcome::Fresh { response, etag } => (response, etag),
        };
        let body: TraktRatings = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("trakt {segment} ratings: {e}")))?;
        let rating = body.rating.filter(|n| *n > 0.0);
        Ok(TraktTitleRatingFetch::Fresh {
            rating: TraktTitleRating {
                imdb_id: imdb_id.to_string(),
                kind,
                rating,
            },
            etag,
        })
    }

    /// Search Trakt movies AND shows for the F-011 search aggregation.
    ///
    /// Calls `/search/movie,show?query=...&limit=...&page=...`. The
    /// response is a mixed list of typed entries; each row is converted
    /// to a kino [`TitleSummary`] preferring the `IMDb` id, falling back
    /// through TMDB / TVDB / Trakt-numeric so cross-provider dedup
    /// (PRD §F-011) has the best signal available.
    pub async fn search(
        &self,
        query: &str,
        page: u32,
        limit: u32,
    ) -> Result<Vec<TitleSummary>, Error> {
        let url = format!("{}/search/movie,show", self.base_url);
        let page_str = page.max(1).to_string();
        let limit_str = limit.max(1).to_string();
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .header("trakt-api-version", "2")
                    .header("trakt-api-key", self.key.as_str())
                    .query(&[
                        ("query", query),
                        ("page", page_str.as_str()),
                        ("limit", limit_str.as_str()),
                    ])
            },
            &self.config,
        )
        .await?;
        let entries: Vec<SearchEntry> = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("trakt search: {e}")))?;
        Ok(entries
            .into_iter()
            .filter_map(SearchEntry::into_summary)
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

/// `/{movies,shows}/{id}/ratings` response shape (PRD §F-010).
#[derive(Debug, Deserialize)]
struct TraktRatings {
    #[serde(default)]
    rating: Option<f64>,
}

/// `/search/movie,show` response entry. Trakt returns a `type` discriminator
/// plus EITHER a `movie` or `show` payload depending on the row's kind.
#[derive(Debug, Deserialize)]
struct SearchEntry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    movie: Option<TitleEntry>,
    #[serde(default)]
    show: Option<TitleEntry>,
}

impl SearchEntry {
    fn into_summary(self) -> Option<TitleSummary> {
        let (kind, entry) = match self.kind.as_str() {
            "movie" => (TitleKind::Movie, self.movie?),
            "show" => (TitleKind::Series, self.show?),
            _ => return None,
        };
        if entry.title.is_empty() {
            return None;
        }
        // Mirror the trending fetcher: prefer IMDb so the F-011 dedup
        // surface can collapse cross-provider duplicates by IMDb id.
        let id = if let Some(imdb) = entry.ids.imdb.clone() {
            imdb
        } else if let Some(tmdb) = entry.ids.tmdb {
            format!("tmdb:{tmdb}")
        } else {
            // No durable id available; skip rather than emit a synthesized
            // row that can't be navigated to.
            return None;
        };
        Some(TitleSummary {
            id,
            kind,
            title: entry.title,
            year: entry.year,
            poster: None,
            rating: None,
        })
    }
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
    async fn title_rating_returns_value_for_movie() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movies/tt0133093/ratings"))
            .and(header("trakt-api-version", "2"))
            .and(header("trakt-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rating": 8.34,
                "votes": 12345,
                "distribution": {}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let rating = client
            .title_rating("tt0133093", TitleKind::Movie)
            .await
            .unwrap();
        assert_eq!(rating, Some(8.34));
    }

    #[tokio::test]
    async fn title_rating_uses_shows_segment_for_series() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/shows/tt0944947/ratings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rating": 9.1,
                "votes": 99999,
                "distribution": {}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let rating = client
            .title_rating("tt0944947", TitleKind::Series)
            .await
            .unwrap();
        assert_eq!(rating, Some(9.1));
    }

    #[tokio::test]
    async fn title_rating_returns_none_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movies/tt9999999/ratings"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::for_test(), server.uri()).unwrap();
        let rating = client
            .title_rating("tt9999999", TitleKind::Movie)
            .await
            .unwrap();
        assert!(rating.is_none());
    }

    #[tokio::test]
    async fn title_rating_returns_none_when_value_zero() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movies/tt1234567/ratings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rating": 0.0,
                "votes": 0
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let rating = client
            .title_rating("tt1234567", TitleKind::Movie)
            .await
            .unwrap();
        assert!(rating.is_none());
    }

    #[tokio::test]
    async fn title_rating_with_etag_no_prior_returns_fresh_with_server_etag() {
        // PRD §F-003 round-trip on `/ratings`: first fetch yields the
        // rating value AND the server's ETag.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movies/tt0133093/ratings"))
            .and(header("trakt-api-version", "2"))
            .and(header("trakt-api-key", "test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"matrix-rating-v1\"")
                    .set_body_json(json!({ "rating": 8.34, "votes": 123 })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let fetch = client
            .title_rating_with_etag("tt0133093", TitleKind::Movie, None)
            .await
            .unwrap();
        match fetch {
            TraktTitleRatingFetch::Fresh { rating, etag } => {
                assert_eq!(etag.as_deref(), Some("\"matrix-rating-v1\""));
                assert_eq!(rating.imdb_id, "tt0133093");
                assert_eq!(rating.kind, TitleKind::Movie);
                assert_eq!(rating.rating, Some(8.34));
            }
            TraktTitleRatingFetch::NotModified => panic!("expected Fresh on first fetch"),
        }
    }

    #[tokio::test]
    async fn title_rating_with_etag_prior_sends_if_none_match_and_304_yields_not_modified() {
        // PRD §F-003 304 cache-hit path on `/ratings`.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/shows/tt0944947/ratings"))
            .and(header("if-none-match", "\"got-rating-v2\""))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let fetch = client
            .title_rating_with_etag("tt0944947", TitleKind::Series, Some("\"got-rating-v2\""))
            .await
            .unwrap();
        assert!(matches!(fetch, TraktTitleRatingFetch::NotModified));
    }

    #[tokio::test]
    async fn title_rating_with_etag_404_yields_fresh_none_so_absence_is_cacheable() {
        // Trakt returns 404 for titles it doesn't know. The ETag-aware
        // variant maps that to a Fresh result with `rating = None` and
        // `etag = None` so the caller can cache the absence for the TTL.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movies/tt9999999/ratings"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::for_test(), server.uri()).unwrap();
        let fetch = client
            .title_rating_with_etag("tt9999999", TitleKind::Movie, None)
            .await
            .unwrap();
        match fetch {
            TraktTitleRatingFetch::Fresh { rating, etag } => {
                assert!(rating.rating.is_none());
                assert!(etag.is_none());
            }
            TraktTitleRatingFetch::NotModified => panic!("expected Fresh(None) on 404"),
        }
    }

    #[tokio::test]
    async fn title_rating_back_compat_unchanged_when_server_sends_etag() {
        // Sanity: the back-compat `title_rating` wrapper still returns
        // the bare `Option<f64>` even when the server sends an ETag.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/movies/tt0133093/ratings"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"matrix-rating-v1\"")
                    .set_body_json(json!({ "rating": 8.34, "votes": 123 })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let rating = client
            .title_rating("tt0133093", TitleKind::Movie)
            .await
            .unwrap();
        assert_eq!(rating, Some(8.34));
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

    #[tokio::test]
    async fn search_returns_movie_and_show_rows() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/movie,show"))
            .and(header("trakt-api-version", "2"))
            .and(header("trakt-api-key", "test-key"))
            .and(query_param("query", "matrix"))
            .and(query_param("page", "1"))
            .and(query_param("limit", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "type": "movie",
                    "movie": {
                        "title": "The Matrix",
                        "year": 1999,
                        "ids": { "imdb": "tt0133093", "tmdb": 603 }
                    }
                },
                {
                    "type": "show",
                    "show": {
                        "title": "The Matrix Resurrections",
                        "year": 2021,
                        "ids": { "imdb": null, "tmdb": 624_860 }
                    }
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.search("matrix", 1, 20).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "tt0133093");
        assert_eq!(items[0].kind, TitleKind::Movie);
        assert_eq!(items[0].title, "The Matrix");
        assert_eq!(items[0].year, Some(1999));
        assert_eq!(items[1].id, "tmdb:624860");
        assert_eq!(items[1].kind, TitleKind::Series);
    }

    #[tokio::test]
    async fn search_drops_rows_with_no_durable_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/movie,show"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "type": "movie",
                    "movie": {
                        "title": "Lost Movie",
                        "year": 1990,
                        "ids": {}
                    }
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.search("lost", 1, 20).await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn search_drops_rows_with_unknown_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/movie,show"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "type": "episode",
                    "episode": { "title": "Pilot", "ids": { "imdb": "tt0000000" } }
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.search("pilot", 1, 20).await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn search_coerces_zero_page_and_limit_to_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/movie,show"))
            .and(query_param("page", "1"))
            .and(query_param("limit", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TraktClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let items = client.search("q", 0, 0).await.unwrap();
        assert!(items.is_empty());
    }
}
