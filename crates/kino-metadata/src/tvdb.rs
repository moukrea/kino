//! TVDB v4 client (PRD §F-003, §F-004).
//!
//! TVDB v4 requires a token-exchange: `POST /v4/login` with the API key in
//! the request body returns a long-lived Bearer token. Subsequent catalog
//! calls attach that token as `Authorization: Bearer <t>`.
//!
//! [`TvdbClient::test_credentials`] performs the login and reports the
//! outcome. The trending fetchers cache the token internally so a single
//! `get_trending` invocation only logs in once even though it issues two
//! filter calls.

use std::collections::HashMap;
use std::sync::Arc;

use reqwest::StatusCode;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::artwork::{LocalizedAsset, ProviderBundle};
use crate::error::Error;
use crate::trending::ProviderItem;
use kino_core::http::{fetch_with_etag, fetch_with_retry, FetchOutcome, HttpConfig};
use kino_core::title::{TitleKind, TitleSummary};

/// TVDB v4 artwork type ids the F-005 resolver cares about.
///
/// TVDB documents distinct id ranges for movies and series. Confirmed against
/// `https://thetvdb.com/api/v4/artwork/types`. We map only the four PRD §F-005
/// image types; ignore the rest.
mod artwork_types {
    pub const SERIES_BANNER: i32 = 1;
    pub const SERIES_POSTER: i32 = 2;
    pub const SERIES_BACKGROUND: i32 = 3;
    pub const SERIES_CLEARART: i32 = 22;
    pub const SERIES_CLEARLOGO: i32 = 23;

    pub const MOVIE_POSTER: i32 = 14;
    pub const MOVIE_BACKGROUND: i32 = 15;
    pub const MOVIE_BANNER: i32 = 16;
    pub const MOVIE_CLEARART: i32 = 24;
    pub const MOVIE_CLEARLOGO: i32 = 25;

    pub const fn is_poster(ty: i32, is_movie: bool) -> bool {
        if is_movie {
            ty == MOVIE_POSTER
        } else {
            ty == SERIES_POSTER
        }
    }

    pub const fn is_background(ty: i32, is_movie: bool) -> bool {
        if is_movie {
            ty == MOVIE_BACKGROUND || ty == MOVIE_BANNER
        } else {
            ty == SERIES_BACKGROUND || ty == SERIES_BANNER
        }
    }

    pub const fn is_logo(ty: i32, is_movie: bool) -> bool {
        if is_movie {
            ty == MOVIE_CLEARLOGO
        } else {
            ty == SERIES_CLEARLOGO
        }
    }

    pub const fn is_clearart(ty: i32, is_movie: bool) -> bool {
        if is_movie {
            ty == MOVIE_CLEARART
        } else {
            ty == SERIES_CLEARART
        }
    }
}

/// Production TVDB v4 base URL.
pub const TVDB_BASE_URL: &str = "https://api4.thetvdb.com";

/// Maximum trending items the F-004 aggregator asks for per provider.
const TRENDING_LIMIT: usize = 100;

/// Result of [`TvdbClient::artwork_with_etag`] — either a fresh extended-
/// title payload (parsed into a [`ProviderBundle`], with the server's
/// `ETag` header when present) or the cache-hit `304 Not Modified` signal
/// that the caller's prior cached bundle is still current. PRD §F-003
/// round-trip on the `/v4/{movies|series}/{id}/extended` endpoint.
///
/// A TVDB `404` (id unknown) is surfaced as `Fresh { bundle: None, etag:
/// None }` so the caller can cache the absence for the TTL just like any
/// other negative result.
#[derive(Debug, Clone, PartialEq)]
pub enum TvdbArtworkFetch {
    /// Server confirmed the caller's cached bundle is still current. The
    /// caller re-uses its existing bundle and only refreshes the cache
    /// row's `expires_at`.
    NotModified,
    /// Server returned a fresh body (or a `404` we map to `bundle: None`).
    /// `etag` is the parsed `ETag` header (or `None` if the endpoint
    /// didn't send one).
    Fresh {
        bundle: Option<ProviderBundle>,
        etag: Option<String>,
    },
}

/// TVDB v4 API client.
///
/// The bearer token from `/v4/login` is cached in `Arc<RwLock<_>>` so clones
/// share the same token across the lifetime of a process. The aggregator
/// builds a fresh client per `get_trending` invocation, so token expiry
/// (which TVDB documents as ~1 month) is not a concern in v1.
#[derive(Debug, Clone)]
pub struct TvdbClient {
    key: String,
    base_url: String,
    config: HttpConfig,
    client: reqwest::Client,
    token: Arc<RwLock<Option<String>>>,
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
            token: Arc::new(RwLock::new(None)),
        })
    }

    /// Verify the stored API key by performing the `POST /v4/login`
    /// exchange. A 200 means the key is accepted by TVDB. The resulting
    /// token is cached for subsequent catalog calls in the same client.
    pub async fn test_credentials(&self) -> Result<(), Error> {
        self.login().await.map(|_| ())
    }

    /// Fetch trending movies for the F-004 aggregator. TVDB v4 does not
    /// expose a dedicated "trending" endpoint; per PRD §F-004 step 1 we use
    /// the `/v4/movies/filter` endpoint sorted by score (community rating)
    /// descending. The "last 90 days" qualifier in the PRD is best-effort
    /// — TVDB filter does not accept a date-range parameter (see
    /// ADR-048), so we approximate by sorting by score and taking the top
    /// 100 across all years.
    pub async fn trending_movies(&self) -> Result<Vec<ProviderItem>, Error> {
        let url = format!("{}/v4/movies/filter", self.base_url);
        let entries: Vec<FilterEntry> = self.fetch_filter(&url).await?;
        Ok(entries
            .into_iter()
            .take(TRENDING_LIMIT)
            .enumerate()
            .map(|(rank, e)| e.into_provider(TitleKind::Movie, rank))
            .collect())
    }

    /// Fetch trending series for the F-004 aggregator. See
    /// [`trending_movies`](Self::trending_movies) for the algorithm
    /// trade-off.
    pub async fn trending_shows(&self) -> Result<Vec<ProviderItem>, Error> {
        let url = format!("{}/v4/series/filter", self.base_url);
        let entries: Vec<FilterEntry> = self.fetch_filter(&url).await?;
        Ok(entries
            .into_iter()
            .take(TRENDING_LIMIT)
            .enumerate()
            .map(|(rank, e)| e.into_provider(TitleKind::Series, rank))
            .collect())
    }

    /// Fetch F-005 artwork + per-language overviews for a TVDB title.
    ///
    /// Calls `/v4/{movies|series}/{id}/extended?meta=translations` once and
    /// parses the `artworks` array (filtered by PRD-locked type ids) plus
    /// `translations.overviewTranslations` into a [`ProviderBundle`].
    /// Returns `Ok(None)` on a TVDB 404 so the resolver can move on without
    /// the entire cascade failing.
    pub async fn artwork(
        &self,
        tvdb_id: u64,
        kind: TitleKind,
    ) -> Result<Option<ProviderBundle>, Error> {
        match self.artwork_with_etag(tvdb_id, kind, None).await? {
            TvdbArtworkFetch::Fresh { bundle, .. } => Ok(bundle),
            // Unreachable: `prior_etag = None` means the server cannot
            // legally produce a 304. Treat as decode error in case a
            // misbehaving mock yields one.
            TvdbArtworkFetch::NotModified => Err(Error::Decode(
                "tvdb extended: 304 returned without prior etag".to_string(),
            )),
        }
    }

    /// `ETag`-aware variant of [`artwork`](Self::artwork) for cache
    /// revalidation (PRD §F-003). Pass the cache row's stored `ETag` as
    /// `prior_etag`; on `304 Not Modified` the server confirms the cached
    /// bundle is still current and the caller re-uses it (refreshing the
    /// row's `expires_at`). On a fresh `2xx`, the parsed `ETag` header is
    /// returned alongside the new payload for persistence.
    pub async fn artwork_with_etag(
        &self,
        tvdb_id: u64,
        kind: TitleKind,
        prior_etag: Option<&str>,
    ) -> Result<TvdbArtworkFetch, Error> {
        let path = match kind {
            TitleKind::Movie => "movies",
            TitleKind::Series => "series",
        };
        let url = format!("{}/v4/{path}/{tvdb_id}/extended", self.base_url);
        let token = self.login().await?;
        let request_result = fetch_with_etag(
            || {
                self.client
                    .get(&url)
                    .bearer_auth(&token)
                    .query(&[("meta", "translations")])
            },
            prior_etag,
            &self.config,
        )
        .await;
        let outcome = match request_result {
            Ok(o) => o,
            Err(kino_core::http::HttpError::Http { status, .. })
                if status == StatusCode::NOT_FOUND.as_u16() =>
            {
                return Ok(TvdbArtworkFetch::Fresh {
                    bundle: None,
                    etag: None,
                });
            }
            Err(e) => return Err(e.into()),
        };
        let (response, etag) = match outcome {
            FetchOutcome::NotModified => return Ok(TvdbArtworkFetch::NotModified),
            FetchOutcome::Fresh { response, etag } => (response, etag),
        };
        let envelope: ExtendedEnvelope = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tvdb {path} extended: {e}")))?;
        let is_movie = matches!(kind, TitleKind::Movie);
        let mut posters = Vec::new();
        let mut backdrops = Vec::new();
        let mut logos = Vec::new();
        let mut clearart = Vec::new();
        for entry in envelope.data.artworks {
            let asset = LocalizedAsset {
                lang: entry.language.unwrap_or_default(),
                url: entry.image,
            };
            if artwork_types::is_poster(entry.r#type, is_movie) {
                posters.push(asset);
            } else if artwork_types::is_background(entry.r#type, is_movie) {
                backdrops.push(asset);
            } else if artwork_types::is_logo(entry.r#type, is_movie) {
                logos.push(asset);
            } else if artwork_types::is_clearart(entry.r#type, is_movie) {
                clearart.push(asset);
            }
        }
        let mut summaries = HashMap::new();
        for t in envelope.data.translations.overview_translations {
            if !t.overview.is_empty() {
                summaries.insert(t.language, t.overview);
            }
        }
        Ok(TvdbArtworkFetch::Fresh {
            bundle: Some(ProviderBundle {
                posters,
                backdrops,
                logos,
                clearart,
                summaries,
            }),
            etag,
        })
    }

    /// Search TVDB for movies AND series (PRD §F-011).
    ///
    /// Calls `/v4/search?query=...&limit=...`. TVDB returns mixed
    /// `movie` / `series` entries (plus people / companies which we drop);
    /// each row is converted to a [`TitleSummary`] preferring the `IMDb` id
    /// when TVDB carries one (the `remote_ids` array), falling back to
    /// the `tvdb:N` prefix.
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<TitleSummary>, Error> {
        let url = format!("{}/v4/search", self.base_url);
        let token = self.login().await?;
        let limit_str = limit.max(1).to_string();
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .bearer_auth(&token)
                    .query(&[("query", query), ("limit", limit_str.as_str())])
            },
            &self.config,
        )
        .await?;
        let envelope: SearchEnvelope = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tvdb search: {e}")))?;
        Ok(envelope
            .data
            .into_iter()
            .filter_map(SearchEntry::into_summary)
            .collect())
    }

    /// Issue a TVDB v4 filter request, deserializing the standard
    /// `{ status, data }` envelope. The endpoint requires three mandatory
    /// query parameters (`country`, `lang`, `sort`). We default to
    /// `usa`/`eng` since TVDB's `lang` is a 3-letter ISO 639-2 code and
    /// only governs which localized name is returned — the catalog itself
    /// is global.
    async fn fetch_filter(&self, url: &str) -> Result<Vec<FilterEntry>, Error> {
        let token = self.login().await?;
        let response = fetch_with_retry(
            || {
                self.client.get(url).bearer_auth(&token).query(&[
                    ("country", "usa"),
                    ("lang", "eng"),
                    ("sort", "score"),
                ])
            },
            &self.config,
        )
        .await?;
        let envelope: FilterEnvelope = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tvdb filter: {e}")))?;
        Ok(envelope.data)
    }

    /// Return a cached bearer token or perform `POST /v4/login` and cache
    /// the result. Concurrent callers serialize on the write lock; the
    /// second one observes the cached token without re-issuing the login.
    async fn login(&self) -> Result<String, Error> {
        if let Some(t) = self.token.read().await.clone() {
            return Ok(t);
        }
        let mut guard = self.token.write().await;
        if let Some(t) = guard.clone() {
            return Ok(t);
        }
        let url = format!("{}/v4/login", self.base_url);
        let body = serde_json::json!({ "apikey": self.key });
        let response =
            fetch_with_retry(|| self.client.post(&url).json(&body), &self.config).await?;
        let envelope: LoginEnvelope = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tvdb login: {e}")))?;
        *guard = Some(envelope.data.token.clone());
        Ok(envelope.data.token)
    }
}

#[derive(Debug, Deserialize)]
struct LoginEnvelope {
    data: LoginData,
}

#[derive(Debug, Deserialize)]
struct LoginData {
    token: String,
}

#[derive(Debug, Deserialize)]
struct FilterEnvelope {
    data: Vec<FilterEntry>,
}

#[derive(Debug, Deserialize)]
struct ExtendedEnvelope {
    data: ExtendedData,
}

#[derive(Debug, Deserialize)]
struct ExtendedData {
    #[serde(default)]
    artworks: Vec<ArtworkEntry>,
    #[serde(default)]
    translations: TranslationsBlock,
}

#[derive(Debug, Default, Deserialize)]
struct TranslationsBlock {
    #[serde(default, rename = "overviewTranslations")]
    overview_translations: Vec<OverviewTranslation>,
}

#[derive(Debug, Deserialize)]
struct OverviewTranslation {
    language: String,
    #[serde(default)]
    overview: String,
}

#[derive(Debug, Deserialize)]
struct ArtworkEntry {
    image: String,
    r#type: i32,
    #[serde(default)]
    language: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FilterEntry {
    id: u64,
    name: String,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    year: Option<String>,
    /// TVDB community score (0..10); F-004's hidden-gems threshold (PRD §8
    /// `HIDDEN_GEMS_RATING_THRESHOLD = 7.5`) is sourced from TMDB so this
    /// field is informational only on the TVDB side.
    #[serde(default)]
    score: Option<f64>,
}

impl FilterEntry {
    fn into_provider(self, kind: TitleKind, rank: usize) -> ProviderItem {
        let year = self.year.as_deref().and_then(|y| y.parse::<u16>().ok());
        let summary = TitleSummary {
            id: format!("tvdb:{}", self.id),
            kind,
            title: self.name,
            year,
            poster: self.image,
            rating: None,
        };
        ProviderItem {
            summary,
            rank,
            popularity: None,
            rating: self.score,
        }
    }
}

/// `/v4/search` envelope.
#[derive(Debug, Deserialize)]
struct SearchEnvelope {
    #[serde(default)]
    data: Vec<SearchEntry>,
}

/// One `/v4/search` row. TVDB returns a heterogeneous list with `type`
/// in `{"movie", "series", "person", "company", "episode"}`. We narrow to
/// the two kinds the F-011 search surface accepts.
#[derive(Debug, Deserialize)]
struct SearchEntry {
    /// String form of the TVDB id (TVDB inconsistently returns it as a
    /// JSON string in `/search`, vs an integer in `/filter`).
    #[serde(default)]
    tvdb_id: Option<String>,
    #[serde(default)]
    name: String,
    /// `movie` / `series` / `person` / etc.
    #[serde(default, rename = "type")]
    kind: String,
    /// Sometimes called `image_url` in the docs, returned bare here.
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    year: Option<String>,
    /// `remote_ids` carries the `IMDb` / TMDB mapping when TVDB knows it.
    #[serde(default)]
    remote_ids: Vec<RemoteIdEntry>,
}

#[derive(Debug, Deserialize)]
struct RemoteIdEntry {
    #[serde(default)]
    id: String,
    /// E.g. `"IMDB"`, `"TheMovieDB.com"`, `"Trakt"`.
    #[serde(default, rename = "sourceName")]
    source_name: String,
}

impl SearchEntry {
    fn into_summary(self) -> Option<TitleSummary> {
        let kind = match self.kind.as_str() {
            "movie" => TitleKind::Movie,
            "series" => TitleKind::Series,
            _ => return None,
        };
        if self.name.is_empty() {
            return None;
        }
        // Prefer IMDb from remote_ids for cross-provider dedup.
        let imdb = self.remote_ids.iter().find_map(|r| {
            if r.source_name.eq_ignore_ascii_case("IMDB") && !r.id.is_empty() {
                Some(r.id.clone())
            } else {
                None
            }
        });
        let id = if let Some(imdb) = imdb {
            imdb
        } else if let Some(tvdb) = self.tvdb_id {
            if tvdb.is_empty() {
                return None;
            }
            format!("tvdb:{tvdb}")
        } else {
            return None;
        };
        let year = self.year.as_deref().and_then(|y| y.parse::<u16>().ok());
        Some(TitleSummary {
            id,
            kind,
            title: self.name,
            year,
            poster: self.image_url,
            rating: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
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

    #[tokio::test]
    async fn trending_movies_logs_in_once_then_fetches_filter() {
        let server = MockServer::start().await;
        // Login is hit exactly once even though we call trending twice on
        // the same client.
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": { "token": "test-token" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/movies/filter"))
            .and(header("authorization", "Bearer test-token"))
            .and(query_param("country", "usa"))
            .and(query_param("lang", "eng"))
            .and(query_param("sort", "score"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": [
                    { "id": 1, "name": "Alpha", "year": "2024", "image": "/a.jpg", "score": 8.5 },
                    { "id": 2, "name": "Beta",  "year": "2023", "image": null,    "score": 7.0 }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/series/filter"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": [{ "id": 99, "name": "Gamma", "year": "2025" }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TvdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let movies = client.trending_movies().await.unwrap();
        let shows = client.trending_shows().await.unwrap();

        assert_eq!(movies.len(), 2);
        assert_eq!(movies[0].summary.id, "tvdb:1");
        assert_eq!(movies[0].summary.title, "Alpha");
        assert_eq!(movies[0].summary.year, Some(2024));
        assert_eq!(movies[0].rating, Some(8.5));
        assert_eq!(movies[1].summary.poster, None);

        assert_eq!(shows.len(), 1);
        assert_eq!(shows[0].summary.kind, TitleKind::Series);
        assert_eq!(shows[0].summary.id, "tvdb:99");
    }

    #[tokio::test]
    async fn artwork_movie_parses_locked_type_ids() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success","data":{"token":"tok"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/movies/5/extended"))
            .and(header("authorization", "Bearer tok"))
            .and(query_param("meta", "translations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success",
                "data": {
                    "id": 5,
                    "artworks": [
                        {"id":1,"image":"https://tvdb/poster-en.jpg","type":14,"language":"eng"},
                        {"id":2,"image":"https://tvdb/bg.jpg","type":15,"language":null},
                        {"id":3,"image":"https://tvdb/logo-en.jpg","type":25,"language":"eng"},
                        {"id":4,"image":"https://tvdb/clearart-en.jpg","type":24,"language":"eng"},
                        {"id":5,"image":"https://tvdb/skipped-character.jpg","type":99,"language":"eng"}
                    ],
                    "translations": {
                        "overviewTranslations": [
                            {"language":"eng","overview":"Movie overview EN"},
                            {"language":"fra","overview":"Aperçu FR"}
                        ]
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TvdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client.artwork(5, TitleKind::Movie).await.unwrap().unwrap();
        assert_eq!(bundle.posters.len(), 1);
        assert_eq!(bundle.posters[0].lang, "eng");
        assert_eq!(bundle.posters[0].url, "https://tvdb/poster-en.jpg");
        assert_eq!(bundle.backdrops.len(), 1);
        assert_eq!(bundle.backdrops[0].lang, ""); // null lang
        assert_eq!(bundle.logos.len(), 1);
        assert_eq!(bundle.clearart.len(), 1);
        assert_eq!(
            bundle.summaries.get("eng").map(String::as_str),
            Some("Movie overview EN")
        );
        assert_eq!(
            bundle.summaries.get("fra").map(String::as_str),
            Some("Aperçu FR")
        );
    }

    #[tokio::test]
    async fn artwork_series_parses_series_type_ids() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success","data":{"token":"tok"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/series/123/extended"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success",
                "data": {
                    "id": 123,
                    "artworks": [
                        {"id":1,"image":"https://tvdb/series-poster.jpg","type":2,"language":"eng"},
                        {"id":2,"image":"https://tvdb/series-bg.jpg","type":3,"language":"eng"},
                        {"id":3,"image":"https://tvdb/series-banner.jpg","type":1,"language":null},
                        {"id":4,"image":"https://tvdb/series-clearart.jpg","type":22,"language":"eng"},
                        {"id":5,"image":"https://tvdb/series-clearlogo.jpg","type":23,"language":"eng"}
                    ],
                    "translations": {"overviewTranslations": []}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TvdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client
            .artwork(123, TitleKind::Series)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(bundle.posters.len(), 1);
        // Series uses type 1 (banner) and type 3 (background) as wide art.
        assert_eq!(bundle.backdrops.len(), 2);
        assert_eq!(bundle.logos.len(), 1);
        assert_eq!(bundle.clearart.len(), 1);
        assert!(bundle.summaries.is_empty());
    }

    #[tokio::test]
    async fn artwork_404_returns_none() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success","data":{"token":"tok"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/movies/9999/extended"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TvdbClient::with_options("test-key", HttpConfig::for_test(), server.uri()).unwrap();
        assert!(client
            .artwork(9999, TitleKind::Movie)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn artwork_with_etag_no_prior_returns_fresh_with_server_etag() {
        // PRD §F-003 round-trip on `/v4/{movies}/{id}/extended`: first
        // fetch yields a parsed bundle AND the server's ETag for
        // revalidation.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success","data":{"token":"tok"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/movies/5/extended"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"tvdb-5-v1\"")
                    .set_body_json(json!({
                        "status":"success",
                        "data": {
                            "id": 5,
                            "artworks": [
                                {"id":1,"image":"https://tvdb/p.jpg","type":14,"language":"eng"}
                            ],
                            "translations": {"overviewTranslations": []}
                        }
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TvdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let fetch = client
            .artwork_with_etag(5, TitleKind::Movie, None)
            .await
            .unwrap();
        match fetch {
            TvdbArtworkFetch::Fresh { bundle, etag } => {
                assert_eq!(etag.as_deref(), Some("\"tvdb-5-v1\""));
                let bundle = bundle.expect("present on 200");
                assert_eq!(bundle.posters.len(), 1);
            }
            TvdbArtworkFetch::NotModified => panic!("expected Fresh on first fetch"),
        }
    }

    #[tokio::test]
    async fn artwork_with_etag_prior_sends_if_none_match_and_304_yields_not_modified() {
        // PRD §F-003 304 cache-hit path on the `/extended` endpoint.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success","data":{"token":"tok"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/series/123/extended"))
            .and(header("if-none-match", "\"tvdb-123-v2\""))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TvdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let fetch = client
            .artwork_with_etag(123, TitleKind::Series, Some("\"tvdb-123-v2\""))
            .await
            .unwrap();
        assert!(matches!(fetch, TvdbArtworkFetch::NotModified));
    }

    #[tokio::test]
    async fn artwork_with_etag_404_yields_fresh_none_so_absence_is_cacheable() {
        // TVDB returns 404 for ids it doesn't know. The ETag-aware variant
        // surfaces that as `Fresh { bundle: None, etag: None }` so the
        // caller can cache the negative result for the TTL.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success","data":{"token":"tok"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/movies/9999/extended"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TvdbClient::with_options("test-key", HttpConfig::for_test(), server.uri()).unwrap();
        let fetch = client
            .artwork_with_etag(9999, TitleKind::Movie, None)
            .await
            .unwrap();
        match fetch {
            TvdbArtworkFetch::Fresh { bundle, etag } => {
                assert!(bundle.is_none());
                assert!(etag.is_none());
            }
            TvdbArtworkFetch::NotModified => panic!("expected Fresh(None) on 404"),
        }
    }

    #[tokio::test]
    async fn search_returns_movies_and_series_preferring_imdb_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success","data":{"token":"tok"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/search"))
            .and(header("Authorization", "Bearer tok"))
            .and(query_param("query", "matrix"))
            .and(query_param("limit", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": [
                    {
                        "tvdb_id": "169",
                        "name": "The Matrix",
                        "type": "movie",
                        "year": "1999",
                        "image_url": "https://artworks.thetvdb.com/p1.jpg",
                        "remote_ids": [
                            {"id": "tt0133093", "sourceName": "IMDB"},
                            {"id": "603", "sourceName": "TheMovieDB.com"}
                        ]
                    },
                    {
                        "tvdb_id": "271124",
                        "name": "Matrix",
                        "type": "series",
                        "year": "1993",
                        "image_url": null,
                        "remote_ids": []
                    },
                    {
                        "tvdb_id": "999",
                        "name": "Carrie-Anne Moss",
                        "type": "person",
                        "remote_ids": []
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TvdbClient::with_options("test-key", HttpConfig::for_test(), server.uri()).unwrap();
        let items = client.search("matrix", 20).await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "tt0133093");
        assert_eq!(items[0].kind, TitleKind::Movie);
        assert_eq!(items[0].title, "The Matrix");
        assert_eq!(items[0].year, Some(1999));
        assert_eq!(
            items[0].poster.as_deref(),
            Some("https://artworks.thetvdb.com/p1.jpg")
        );
        assert_eq!(items[1].id, "tvdb:271124");
        assert_eq!(items[1].kind, TitleKind::Series);
    }

    #[tokio::test]
    async fn search_drops_entries_with_no_id_at_all() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success","data":{"token":"tok"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    { "name": "Orphan", "type": "movie" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TvdbClient::with_options("test-key", HttpConfig::for_test(), server.uri()).unwrap();
        let items = client.search("orphan", 20).await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn search_coerces_zero_limit_to_one() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status":"success","data":{"token":"tok"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/search"))
            .and(query_param("limit", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .expect(1)
            .mount(&server)
            .await;
        let client =
            TvdbClient::with_options("test-key", HttpConfig::for_test(), server.uri()).unwrap();
        let items = client.search("q", 0).await.unwrap();
        assert!(items.is_empty());
    }
}
