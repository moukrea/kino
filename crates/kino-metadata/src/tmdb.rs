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

use crate::artwork::{LangAsset, LangText, ProviderArtBundle, TitleIds};
use crate::error::Error;
use crate::http::{fetch_with_retry, HttpConfig};
use crate::trending::ProviderItem;
use kino_core::title::{TitleKind, TitleSummary};

/// Production TMDB base URL.
pub const TMDB_BASE_URL: &str = "https://api.themoviedb.org";

/// Production TMDB image CDN base URL. PRD §F-005 image URLs are built by
/// concatenating this with a `<size>/<path>` suffix.
pub const TMDB_IMAGE_BASE_URL: &str = "https://image.tmdb.org/t/p";

/// Maximum trending items the F-004 aggregator asks for per provider.
const TRENDING_LIMIT: usize = 100;

/// TMDB image size used for poster artwork. `w500` is the widely cited
/// home-screen default; the F-008 tile spec (240×360 reference) sits well
/// within `w500` so we get one cache-friendly variant for every tile.
const TMDB_POSTER_SIZE: &str = "w500";

/// TMDB image size used for backdrops. `w1280` covers the title-detail
/// hero (F-010) and any responsive tile that scales up.
const TMDB_BACKDROP_SIZE: &str = "w1280";

/// TMDB image size used for logos / clearart. `original` is the safe pick
/// because logos are PNG with transparency at variable native sizes; TMDB
/// recommends transforming at render time.
const TMDB_LOGO_SIZE: &str = "original";

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

    /// Fetch the F-005 image + summary bundle for one title.
    ///
    /// Issues a single `/3/{type}/{id}?append_to_response=images,translations`
    /// request with `include_image_language=null,*` so the response carries
    /// every language-tagged image AND the language-agnostic ones in one
    /// shot, plus a `translations` block with the `overview` text per
    /// language. The resolver consumes the resulting [`ProviderArtBundle`].
    pub async fn fetch_art_bundle(
        &self,
        kind: TitleKind,
        id: u64,
    ) -> Result<ProviderArtBundle, Error> {
        let media_type = match kind {
            TitleKind::Movie => "movie",
            TitleKind::Series => "tv",
        };
        let url = format!("{}/3/{media_type}/{id}", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client.get(&url).query(&[
                    ("api_key", self.key.as_str()),
                    ("append_to_response", "images,translations"),
                    ("include_image_language", "null,*"),
                ])
            },
            &self.config,
        )
        .await?;
        let raw: ArtBundleResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb {media_type} {id} bundle: {e}")))?;
        Ok(raw.into_bundle(kind))
    }

    /// Fetch the external-id bag for a title (PRD §F-005 ID resolution).
    ///
    /// Returns whatever subset of `IMDb` / TVDB ids TMDB knows about. Used
    /// by the host command to enrich a catalog id that arrives with only
    /// a TMDB id (so Fanart.tv / TVDB lookups can still proceed).
    pub async fn fetch_external_ids(&self, kind: TitleKind, id: u64) -> Result<TitleIds, Error> {
        let media_type = match kind {
            TitleKind::Movie => "movie",
            TitleKind::Series => "tv",
        };
        let url = format!("{}/3/{media_type}/{id}/external_ids", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str())])
            },
            &self.config,
        )
        .await?;
        let raw: ExternalIdsResponse = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tmdb {media_type} {id} external_ids: {e}")))?;
        Ok(TitleIds {
            imdb: raw.imdb_id.filter(|s| !s.is_empty()),
            tmdb: Some(id),
            tvdb: raw.tvdb_id,
        })
    }

    /// Resolve a non-TMDB id (`IMDb` or TVDB) into the TMDB equivalent via
    /// `/3/find/{external_id}?external_source=imdb_id|tvdb_id`.
    ///
    /// Returns the first matching TMDB id of the requested kind, or `None`
    /// if no match exists. PRD §F-004 step 2 explicitly carves space for
    /// this path: "Items without `IMDb` ID resolved via TMDB /find when
    /// possible, else dropped."
    pub async fn find_by_external_id(
        &self,
        kind: TitleKind,
        external_source: &str,
        external_id: &str,
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
        let raw: FindResponse = response.json().await.map_err(|e| {
            Error::Decode(format!("tmdb find {external_source}/{external_id}: {e}"))
        })?;
        let pick = match kind {
            TitleKind::Movie => raw.movie_results,
            TitleKind::Series => raw.tv_results,
        };
        Ok(pick.into_iter().next().map(|m| m.id))
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

/// Build a TMDB poster URL at the `w500` size. The F-005 image resolver
/// reuses this via [`build_image_url`] for consistency.
fn tmdb_poster_url(path: &str) -> String {
    build_image_url(TMDB_POSTER_SIZE, path)
}

/// Build a TMDB CDN URL at the requested size for the given image path.
/// Empty paths return an empty string so the resolver can skip them.
fn build_image_url(size: &str, path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    format!("{TMDB_IMAGE_BASE_URL}/{size}/{trimmed}")
}

/// `/3/{type}/{id}?append_to_response=images,translations` envelope.
/// Trimmed to the fields F-005 needs.
#[derive(Debug, Deserialize)]
struct ArtBundleResponse {
    #[serde(default)]
    overview: Option<String>,
    /// TMDB's `original_language` is a 2-letter ISO 639-1 code attached to
    /// the title in its origin language. Used as the lang tag for the
    /// `overview` field above (TMDB's per-language `overview` is the
    /// origin-language summary when no `language=...` query param narrows
    /// it).
    #[serde(default)]
    original_language: Option<String>,
    #[serde(default)]
    images: Option<ImageBlock>,
    #[serde(default)]
    translations: Option<TranslationsBlock>,
}

#[derive(Debug, Default, Deserialize)]
struct ImageBlock {
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
    #[serde(default)]
    iso_639_1: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TranslationsBlock {
    #[serde(default)]
    translations: Vec<Translation>,
}

#[derive(Debug, Deserialize)]
struct Translation {
    /// 2-letter ISO 639-1 code, occasionally absent.
    #[serde(default)]
    iso_639_1: Option<String>,
    data: TranslationData,
}

#[derive(Debug, Default, Deserialize)]
struct TranslationData {
    #[serde(default)]
    overview: Option<String>,
}

impl ArtBundleResponse {
    fn into_bundle(self, _kind: TitleKind) -> ProviderArtBundle {
        let images = self.images.unwrap_or_default();
        let posters = images
            .posters
            .into_iter()
            .map(|e| into_asset(e, TMDB_POSTER_SIZE))
            .filter(|a| !a.url.is_empty())
            .collect();
        let backdrops = images
            .backdrops
            .into_iter()
            .map(|e| into_asset(e, TMDB_BACKDROP_SIZE))
            .filter(|a| !a.url.is_empty())
            .collect();
        let logos = images
            .logos
            .into_iter()
            .map(|e| into_asset(e, TMDB_LOGO_SIZE))
            .filter(|a| !a.url.is_empty())
            .collect();
        // TMDB does not surface a dedicated "clearart" category; the F-005
        // chain falls through to TVDB / Fanart for that field.
        let clearart = Vec::new();

        let mut summaries = Vec::new();
        // The top-level `overview` is the origin-language overview when no
        // `language=` param narrowed the response. Tag it with the title's
        // `original_language` so the resolver can match the user's lang
        // preference against it.
        if let Some(text) = self.overview.filter(|t| !t.is_empty()) {
            summaries.push(LangText {
                lang: self.original_language.unwrap_or_default(),
                text,
            });
        }
        if let Some(t) = self.translations {
            for tr in t.translations {
                let text = tr.data.overview.unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let lang = tr.iso_639_1.unwrap_or_default();
                // Skip duplicates of the top-level overview entry.
                if summaries.iter().any(|s| s.lang == lang && s.text == text) {
                    continue;
                }
                summaries.push(LangText { lang, text });
            }
        }
        ProviderArtBundle {
            posters,
            backdrops,
            logos,
            clearart,
            summaries,
        }
    }
}

fn into_asset(e: ImageEntry, size: &str) -> LangAsset {
    LangAsset {
        lang: e.iso_639_1.unwrap_or_default(),
        url: build_image_url(size, &e.file_path),
    }
}

/// `/3/{type}/{id}/external_ids` envelope.
#[derive(Debug, Deserialize)]
struct ExternalIdsResponse {
    #[serde(default)]
    imdb_id: Option<String>,
    #[serde(default)]
    tvdb_id: Option<u64>,
}

/// `/3/find/{external_id}` envelope. TMDB returns parallel buckets per
/// media kind; we read the one matching the caller's [`TitleKind`].
#[derive(Debug, Deserialize)]
struct FindResponse {
    #[serde(default)]
    movie_results: Vec<FindHit>,
    #[serde(default)]
    tv_results: Vec<FindHit>,
}

#[derive(Debug, Deserialize)]
struct FindHit {
    id: u64,
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
    async fn fetch_art_bundle_parses_images_and_translations_for_movie() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603"))
            .and(query_param("api_key", "test-key"))
            .and(query_param("append_to_response", "images,translations"))
            .and(query_param("include_image_language", "null,*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 603,
                "title": "The Matrix",
                "original_language": "en",
                "overview": "A hacker discovers the truth.",
                "images": {
                    "posters": [
                        { "file_path": "/p-en.jpg", "iso_639_1": "en" },
                        { "file_path": "/p-fr.jpg", "iso_639_1": "fr" },
                        { "file_path": "/p-none.jpg", "iso_639_1": null }
                    ],
                    "backdrops": [
                        { "file_path": "/b.jpg", "iso_639_1": null }
                    ],
                    "logos": [
                        { "file_path": "/logo-en.png", "iso_639_1": "en" }
                    ]
                },
                "translations": {
                    "translations": [
                        {
                            "iso_639_1": "fr",
                            "data": { "overview": "Un hacker découvre la vérité." }
                        },
                        {
                            "iso_639_1": "de",
                            "data": { "overview": "" }
                        }
                    ]
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client
            .fetch_art_bundle(TitleKind::Movie, 603)
            .await
            .unwrap();
        assert_eq!(bundle.posters.len(), 3);
        assert_eq!(bundle.posters[0].lang, "en");
        assert_eq!(
            bundle.posters[0].url,
            "https://image.tmdb.org/t/p/w500/p-en.jpg"
        );
        assert_eq!(bundle.posters[2].lang, "");
        assert_eq!(bundle.backdrops.len(), 1);
        assert_eq!(
            bundle.backdrops[0].url,
            "https://image.tmdb.org/t/p/w1280/b.jpg"
        );
        assert_eq!(bundle.logos.len(), 1);
        assert_eq!(
            bundle.logos[0].url,
            "https://image.tmdb.org/t/p/original/logo-en.png"
        );
        assert!(bundle.clearart.is_empty(), "TMDB does not serve clearart");

        // Top-level overview tagged with original_language; French
        // translation present; empty German translation dropped.
        assert_eq!(bundle.summaries.len(), 2);
        let en = bundle.summaries.iter().find(|s| s.lang == "en").unwrap();
        assert_eq!(en.text, "A hacker discovers the truth.");
        let fr = bundle.summaries.iter().find(|s| s.lang == "fr").unwrap();
        assert_eq!(fr.text, "Un hacker découvre la vérité.");
    }

    #[tokio::test]
    async fn fetch_art_bundle_uses_tv_endpoint_for_series() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/tv/1399"))
            .and(query_param("append_to_response", "images,translations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1399,
                "name": "Game of Thrones",
                "original_language": "en",
                "overview": "Seven noble families.",
                "images": { "posters": [], "backdrops": [], "logos": [] }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client
            .fetch_art_bundle(TitleKind::Series, 1399)
            .await
            .unwrap();
        assert_eq!(bundle.summaries.len(), 1);
        assert_eq!(bundle.summaries[0].lang, "en");
    }

    #[tokio::test]
    async fn fetch_external_ids_returns_imdb_and_tvdb() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/603/external_ids"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 603,
                "imdb_id": "tt0133093",
                "tvdb_id": 12345
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let ids = client
            .fetch_external_ids(TitleKind::Movie, 603)
            .await
            .unwrap();
        assert_eq!(ids.imdb.as_deref(), Some("tt0133093"));
        assert_eq!(ids.tmdb, Some(603));
        assert_eq!(ids.tvdb, Some(12_345));
    }

    #[tokio::test]
    async fn fetch_external_ids_handles_missing_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/movie/999/external_ids"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 999,
                "imdb_id": ""
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let ids = client
            .fetch_external_ids(TitleKind::Movie, 999)
            .await
            .unwrap();
        assert!(ids.imdb.is_none(), "empty imdb_id normalized to None");
        assert!(ids.tvdb.is_none());
    }

    #[tokio::test]
    async fn find_by_external_id_resolves_imdb_to_tmdb() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/find/tt0133093"))
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
            .find_by_external_id(TitleKind::Movie, "imdb_id", "tt0133093")
            .await
            .unwrap();
        assert_eq!(id, Some(603));
    }

    #[tokio::test]
    async fn find_by_external_id_returns_none_when_kind_mismatches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/3/find/tt0944947"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "movie_results": [],
                "tv_results": [{ "id": 1399 }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TmdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let id = client
            .find_by_external_id(TitleKind::Movie, "imdb_id", "tt0944947")
            .await
            .unwrap();
        assert_eq!(id, None, "movie kind ignored the tv_results bucket");
    }
}
