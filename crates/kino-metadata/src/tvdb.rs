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

use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::RwLock;

use crate::artwork::{LangAsset, LangText, ProviderArtBundle};
use crate::error::Error;
use crate::http::{fetch_with_retry, HttpConfig};
use crate::trending::ProviderItem;
use kino_core::title::{TitleKind, TitleSummary};

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

    /// Fetch the F-005 image + summary bundle for one title.
    ///
    /// Calls `/v4/{movies|series}/{id}/extended?meta=translations` which
    /// returns the full record plus a per-language `nameTranslations` and
    /// `overviewTranslations` block. Two extra calls per non-English lang
    /// would otherwise be needed; the `meta=translations` form short-
    /// circuits that.
    ///
    /// TVDB's `artworks` array carries numeric type IDs. We map the
    /// commonly used ones (poster / background / clearlogo / clearart) onto
    /// [`ProviderArtBundle`]'s slots; everything else is ignored.
    pub async fn fetch_art_bundle(
        &self,
        kind: TitleKind,
        id: u64,
    ) -> Result<ProviderArtBundle, Error> {
        let token = self.login().await?;
        let media_path = match kind {
            TitleKind::Movie => "movies",
            TitleKind::Series => "series",
        };
        let url = format!("{}/v4/{media_path}/{id}/extended", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .bearer_auth(&token)
                    .query(&[("meta", "translations")])
            },
            &self.config,
        )
        .await?;
        let envelope: ExtendedEnvelope = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("tvdb {media_path} {id} extended: {e}")))?;
        Ok(envelope.data.into_bundle(kind))
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

/// `/v4/{movies|series}/{id}/extended` envelope.
#[derive(Debug, Deserialize)]
struct ExtendedEnvelope {
    data: ExtendedData,
}

/// Trimmed view of TVDB's extended record. We only read `artworks` (for
/// images) and the two `*Translations` blocks (for summaries).
#[derive(Debug, Default, Deserialize)]
struct ExtendedData {
    #[serde(default)]
    artworks: Vec<ArtworkEntry>,
    /// Populated only when the response includes `meta=translations`.
    #[serde(default)]
    translations: TranslationsBundle,
}

#[derive(Debug, Deserialize)]
struct ArtworkEntry {
    /// TVDB numeric art type. The poster / background / logo / clearart
    /// IDs differ between movies and series; see [`tvdb_art_kind`].
    #[serde(rename = "type")]
    type_id: u32,
    /// CDN URL (already absolute on the TVDB image host).
    #[serde(default)]
    image: Option<String>,
    /// ISO 639-2/T 3-letter language code; `null` for language-agnostic.
    #[serde(default)]
    language: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TranslationsBundle {
    /// One entry per locale that carries an `overview`.
    #[serde(default, rename = "overviewTranslations")]
    overview: Vec<OverviewTranslation>,
}

#[derive(Debug, Deserialize)]
struct OverviewTranslation {
    /// 3-letter ISO 639-2/T language code.
    #[serde(default)]
    language: Option<String>,
    /// The localized overview text. Always present in the documented
    /// schema, but defensive `default` keeps us safe.
    #[serde(default)]
    overview: Option<String>,
}

/// The TVDB `ArtKind` slot a numeric artwork type ID belongs to. Returns
/// `None` for types F-005 does not consume (banners, season posters, icons,
/// etc.). The mapping covers both the movie and series art type IDs since
/// each catalog kind uses a disjoint set.
fn tvdb_art_kind(type_id: u32) -> Option<crate::artwork::ArtKind> {
    use crate::artwork::ArtKind;
    match type_id {
        // Series art types (per TVDB v4 `/v4/artwork/types`).
        2  // series poster
        | 14  // movie poster
        => Some(ArtKind::Poster),
        3  // series fanart background
        | 15  // movie background
        => Some(ArtKind::Backdrop),
        5  // series clearLogo
        | 23  // movie clearLogo
        => Some(ArtKind::Logo),
        22  // series clearArt
        | 24  // movie clearArt
        => Some(ArtKind::Clearart),
        _ => None,
    }
}

impl ExtendedData {
    fn into_bundle(self, _kind: TitleKind) -> ProviderArtBundle {
        let mut bundle = ProviderArtBundle::default();
        for art in self.artworks {
            let Some(kind) = tvdb_art_kind(art.type_id) else {
                continue;
            };
            let url = art.image.unwrap_or_default();
            if url.is_empty() {
                continue;
            }
            let asset = LangAsset {
                lang: art.language.unwrap_or_default(),
                url,
            };
            match kind {
                crate::artwork::ArtKind::Poster => bundle.posters.push(asset),
                crate::artwork::ArtKind::Backdrop => bundle.backdrops.push(asset),
                crate::artwork::ArtKind::Logo => bundle.logos.push(asset),
                crate::artwork::ArtKind::Clearart => bundle.clearart.push(asset),
            }
        }
        for tr in self.translations.overview {
            let text = tr.overview.unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            bundle.summaries.push(LangText {
                lang: tr.language.unwrap_or_default(),
                text,
            });
        }
        bundle
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
    async fn fetch_art_bundle_for_movie_maps_artwork_and_translations() {
        let server = MockServer::start().await;
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
            .and(path("/v4/movies/603/extended"))
            .and(header("authorization", "Bearer test-token"))
            .and(query_param("meta", "translations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {
                    "id": 603,
                    "name": "The Matrix",
                    "artworks": [
                        { "type": 14, "image": "https://tvdb.example/p-eng.jpg", "language": "eng" },
                        { "type": 14, "image": "https://tvdb.example/p-fra.jpg", "language": "fra" },
                        { "type": 15, "image": "https://tvdb.example/back.jpg", "language": null },
                        { "type": 23, "image": "https://tvdb.example/logo.png", "language": "eng" },
                        { "type": 24, "image": "https://tvdb.example/clear.png", "language": "eng" },
                        // Unknown art type — must be dropped.
                        { "type": 999, "image": "https://tvdb.example/ignored.jpg", "language": "eng" }
                    ],
                    "translations": {
                        "overviewTranslations": [
                            { "language": "eng", "overview": "A hacker discovers the truth." },
                            { "language": "fra", "overview": "Un hacker découvre la vérité." },
                            { "language": "deu", "overview": "" }
                        ]
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TvdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client
            .fetch_art_bundle(TitleKind::Movie, 603)
            .await
            .unwrap();
        assert_eq!(bundle.posters.len(), 2);
        assert_eq!(bundle.posters[0].lang, "eng");
        assert_eq!(bundle.backdrops.len(), 1);
        assert_eq!(bundle.backdrops[0].lang, "");
        assert_eq!(bundle.logos.len(), 1);
        assert_eq!(bundle.clearart.len(), 1);
        assert_eq!(bundle.summaries.len(), 2, "empty German entry was dropped");
        assert_eq!(bundle.summaries[0].lang, "eng");
    }

    #[tokio::test]
    async fn fetch_art_bundle_for_series_uses_series_endpoint_and_series_type_ids() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v4/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": { "token": "tok" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v4/series/1399/extended"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success",
                "data": {
                    "id": 1399,
                    "name": "Game of Thrones",
                    "artworks": [
                        { "type": 2, "image": "https://tvdb.example/got-poster.jpg", "language": "eng" },
                        { "type": 3, "image": "https://tvdb.example/got-back.jpg", "language": null },
                        { "type": 5, "image": "https://tvdb.example/got-logo.png", "language": "eng" }
                    ],
                    "translations": {
                        "overviewTranslations": [
                            { "language": "eng", "overview": "Seven noble families." }
                        ]
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            TvdbClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client
            .fetch_art_bundle(TitleKind::Series, 1399)
            .await
            .unwrap();
        assert_eq!(bundle.posters.len(), 1);
        assert_eq!(bundle.backdrops.len(), 1);
        assert_eq!(bundle.logos.len(), 1);
        assert!(bundle.clearart.is_empty());
        assert_eq!(bundle.summaries.len(), 1);
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
}
