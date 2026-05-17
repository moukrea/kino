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
use kino_core::http::{fetch_with_retry, HttpConfig};
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
        let path = match kind {
            TitleKind::Movie => "movies",
            TitleKind::Series => "series",
        };
        let url = format!("{}/v4/{path}/{tvdb_id}/extended", self.base_url);
        let token = self.login().await?;
        let request_result = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .bearer_auth(&token)
                    .query(&[("meta", "translations")])
            },
            &self.config,
        )
        .await;
        let response = match request_result {
            Ok(r) => r,
            Err(kino_core::http::HttpError::Http { status, .. })
                if status == StatusCode::NOT_FOUND.as_u16() =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
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
        Ok(Some(ProviderBundle {
            posters,
            backdrops,
            logos,
            clearart,
            summaries,
        }))
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
}
