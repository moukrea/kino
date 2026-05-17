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

use crate::artwork::{LocalizedAsset, ProviderBundle};
use crate::error::Error;
use crate::http::{fetch_with_retry, HttpConfig};
use crate::trending::ProviderItem;
use kino_core::title::{TitleKind, TitleSummary};

/// Production TMDB base URL.
pub const TMDB_BASE_URL: &str = "https://api.themoviedb.org";

/// Maximum trending items the F-004 aggregator asks for per provider.
const TRENDING_LIMIT: usize = 100;

/// External-id resolution result. Each field is `None` when TMDB has no
/// corresponding mapping; the F-005 resolver uses these to dispatch the
/// Fanart.tv (needs `IMDb` or TMDB id for movies, TVDB id for series) and
/// TVDB (needs TVDB id) calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleIds {
    pub tmdb_id: Option<u64>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<u64>,
}

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

    /// Resolve a TMDB id from an external id (`IMDb` or TVDB) via `/3/find`.
    /// Returns the first matching numeric TMDB id from the appropriate
    /// `*_results` array, or `None` when TMDB has no mapping.
    ///
    /// `external_source` must be one of `"imdb_id"` or `"tvdb_id"` per TMDB
    /// documentation. `kind` selects which `*_results` array to read
    /// (`movie_results` vs `tv_results`).
    pub async fn find_external(
        &self,
        external_id: &str,
        external_source: &'static str,
        kind: TitleKind,
    ) -> Result<Option<u64>, Error> {
        let url = format!("{}/3/find/{external_id}", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client.get(&url).query(&[
                    ("api_key", self.key.as_str()),
                    ("external_source", external_source),
                ])
            },
            &self.config,
        )
        .await?;
        let body: FindResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb find {external_source}: {e}")))?;
        let results = match kind {
            TitleKind::Movie => body.movie_results,
            TitleKind::Series => body.tv_results,
        };
        Ok(results.into_iter().next().map(|r| r.id))
    }

    /// Fetch external ids (`imdb_id`, `tvdb_id`) for a TMDB title. Used by
    /// the F-005 resolver to bridge into Fanart.tv (movies key by `IMDb`,
    /// shows key by TVDB) and TVDB (keys by its own id).
    pub async fn external_ids(&self, tmdb_id: u64, kind: TitleKind) -> Result<TitleIds, Error> {
        let media = match kind {
            TitleKind::Movie => "movie",
            TitleKind::Series => "tv",
        };
        let url = format!("{}/3/{media}/{tmdb_id}/external_ids", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str())])
            },
            &self.config,
        )
        .await?;
        let body: ExternalIdsResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb external_ids: {e}")))?;
        Ok(TitleIds {
            tmdb_id: Some(tmdb_id),
            imdb_id: body.imdb_id.filter(|s| !s.is_empty()),
            tvdb_id: body.tvdb_id,
        })
    }

    /// Fetch the F-005 image bundle for a title. One HTTP call covers every
    /// language we care about by joining `lang_pref` (plus `null` for
    /// textless artwork) into TMDB's `include_image_language` filter. The
    /// response carries `posters` / `backdrops` / `logos`; TMDB does not
    /// expose clearart so [`ProviderBundle::clearart`] stays empty.
    pub async fn artwork_images(
        &self,
        tmdb_id: u64,
        kind: TitleKind,
        lang_pref: &[String],
    ) -> Result<ProviderBundle, Error> {
        let media = match kind {
            TitleKind::Movie => "movie",
            TitleKind::Series => "tv",
        };
        let url = format!("{}/3/{media}/{tmdb_id}/images", self.base_url);
        let langs = include_image_language(lang_pref);
        let response = fetch_with_retry(
            || {
                self.client.get(&url).query(&[
                    ("api_key", self.key.as_str()),
                    ("include_image_language", langs.as_str()),
                ])
            },
            &self.config,
        )
        .await?;
        let body: ImagesResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb images {media}: {e}")))?;
        Ok(ProviderBundle {
            posters: body.posters.into_iter().map(into_asset).collect(),
            backdrops: body.backdrops.into_iter().map(into_asset).collect(),
            logos: body.logos.into_iter().map(into_asset).collect(),
            clearart: Vec::new(),
            summaries: std::collections::HashMap::new(),
        })
    }

    /// Fetch the localized summary for a title in `language`. Returns
    /// `Some(text)` when the overview is non-empty, `None` otherwise. The
    /// F-005 resolver may call this several times per title (one per tier
    /// language) so the host caches at the cascade granularity, not here.
    pub async fn summary(
        &self,
        tmdb_id: u64,
        kind: TitleKind,
        language: &str,
    ) -> Result<Option<String>, Error> {
        let media = match kind {
            TitleKind::Movie => "movie",
            TitleKind::Series => "tv",
        };
        let url = format!("{}/3/{media}/{tmdb_id}", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str()), ("language", language)])
            },
            &self.config,
        )
        .await?;
        let body: DetailResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb {media} details: {e}")))?;
        Ok(body.overview.filter(|s| !s.is_empty()))
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

/// Build a TMDB image URL at the `original` size for the F-005 resolver.
/// The catalog UI scales down; using `original` lets the renderer pick the
/// right tier on a per-display basis without re-fetching.
fn tmdb_artwork_url(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    format!("https://image.tmdb.org/t/p/original/{trimmed}")
}

/// Build the `include_image_language` query string from the user's language
/// chain plus `null` (textless artwork). TMDB accepts a comma-separated list
/// of 2-letter codes; `null` is a sentinel for "no language tag".
fn include_image_language(lang_pref: &[String]) -> String {
    let mut parts: Vec<String> = lang_pref.iter().map(|s| normalize_for_tmdb(s)).collect();
    parts.push("null".to_string());
    // Dedup while preserving order.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    parts.retain(|s: &String| seen.insert(s.clone()));
    parts.join(",")
}

/// Convert a BCP-47-ish lang ("en", "en-US") into TMDB's expected 2-letter
/// code. ISO 639-2 inputs ("eng", "fre") fall through to a 3-letter form
/// which TMDB tolerates by silently ignoring.
fn normalize_for_tmdb(lang: &str) -> String {
    let primary = lang.split(['-', '_']).next().unwrap_or("");
    primary.to_ascii_lowercase()
}

fn into_asset(img: ImageEntry) -> LocalizedAsset {
    LocalizedAsset {
        lang: img.iso_639_1.unwrap_or_default(),
        url: tmdb_artwork_url(&img.file_path),
    }
}

#[derive(Debug, Deserialize)]
struct FindResponse {
    #[serde(default)]
    movie_results: Vec<FindResultEntry>,
    #[serde(default)]
    tv_results: Vec<FindResultEntry>,
}

#[derive(Debug, Deserialize)]
struct FindResultEntry {
    id: u64,
}

#[derive(Debug, Deserialize)]
struct ExternalIdsResponse {
    #[serde(default)]
    imdb_id: Option<String>,
    #[serde(default)]
    tvdb_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ImagesResponse {
    #[serde(default)]
    posters: Vec<ImageEntry>,
    #[serde(default)]
    backdrops: Vec<ImageEntry>,
    #[serde(default)]
    logos: Vec<ImageEntry>,
}

#[derive(Debug, Deserialize)]
struct ImageEntry {
    file_path: String,
    /// Empty / null means textless (provider-neutral artwork).
    #[serde(default)]
    iso_639_1: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DetailResponse {
    #[serde(default)]
    overview: Option<String>,
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

    #[tokio::test]
    async fn find_external_resolves_imdb_to_tmdb_movie_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/find/tt0133093"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("external_source", "imdb_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "movie_results": [{ "id": 603 }],
                "tv_results": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let id = client
            .find_external("tt0133093", "imdb_id", TitleKind::Movie)
            .await
            .unwrap();
        assert_eq!(id, Some(603));
    }

    #[tokio::test]
    async fn find_external_returns_none_when_no_match() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/find/tt9999999"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "movie_results": [],
                "tv_results": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let id = client
            .find_external("tt9999999", "imdb_id", TitleKind::Movie)
            .await
            .unwrap();
        assert_eq!(id, None);
    }

    #[tokio::test]
    async fn external_ids_returns_full_id_set() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603/external_ids"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "imdb_id": "tt0133093",
                "tvdb_id": 7782
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let ids = client.external_ids(603, TitleKind::Movie).await.unwrap();
        assert_eq!(ids.tmdb_id, Some(603));
        assert_eq!(ids.imdb_id.as_deref(), Some("tt0133093"));
        assert_eq!(ids.tvdb_id, Some(7782));
    }

    #[tokio::test]
    async fn external_ids_handles_empty_imdb_field() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/tv/1399/external_ids"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "imdb_id": "",
                "tvdb_id": null
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let ids = client.external_ids(1399, TitleKind::Series).await.unwrap();
        assert_eq!(ids.imdb_id, None);
        assert_eq!(ids.tvdb_id, None);
    }

    #[tokio::test]
    async fn artwork_images_parses_posters_backdrops_logos() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603/images"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("include_image_language", "en,fr,null"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "posters": [
                    {"file_path": "/poster-en.jpg", "iso_639_1": "en"},
                    {"file_path": "/poster-fr.jpg", "iso_639_1": "fr"},
                    {"file_path": "/poster-textless.jpg", "iso_639_1": null}
                ],
                "backdrops": [
                    {"file_path": "/back-en.jpg", "iso_639_1": "en"}
                ],
                "logos": [
                    {"file_path": "/logo-en.png", "iso_639_1": "en"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client
            .artwork_images(603, TitleKind::Movie, &["en".to_string(), "fr".to_string()])
            .await
            .unwrap();
        assert_eq!(bundle.posters.len(), 3);
        assert_eq!(bundle.posters[0].lang, "en");
        assert_eq!(bundle.posters[2].lang, "");
        assert_eq!(
            bundle.posters[0].url,
            "https://image.tmdb.org/t/p/original/poster-en.jpg"
        );
        assert_eq!(bundle.backdrops.len(), 1);
        assert_eq!(bundle.logos.len(), 1);
        // TMDB has no clearart endpoint; bucket stays empty.
        assert!(bundle.clearart.is_empty());
        assert!(bundle.summaries.is_empty());
    }

    #[tokio::test]
    async fn summary_returns_text_for_requested_language() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("language", "fr"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "overview": "Néo découvre la matrice."
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let text = client.summary(603, TitleKind::Movie, "fr").await.unwrap();
        assert_eq!(text.as_deref(), Some("Néo découvre la matrice."));
    }

    #[tokio::test]
    async fn summary_returns_none_when_overview_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "overview": ""
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let text = client.summary(603, TitleKind::Movie, "ja").await.unwrap();
        assert!(text.is_none());
    }
}
