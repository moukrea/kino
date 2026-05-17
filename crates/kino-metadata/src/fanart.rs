//! Fanart.tv v3 client (PRD §F-003).
//!
//! Fanart.tv exposes artwork via `webservice.fanart.tv/v3/{movies,tv}/<id>`.
//! Authentication is a single `api_key` query parameter; absence or a wrong
//! key yields a 401/403.
//!
//! [`FanartClient::test_credentials`] hits `/v3/movies/tt0111161` (Shawshank,
//! a stable known-good `IMDb` id) to confirm the key works without coupling
//! the test to any other catalog state.

use serde::Deserialize;

use crate::artwork::{LangAsset, ProviderArtBundle};
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

    /// Fetch the artwork bundle for a movie (PRD §F-005).
    ///
    /// Fanart.tv movies are keyed by `IMDb` id (e.g. `tt0133093`). The
    /// `/v3/movies/{id}` endpoint returns all artwork categories Fanart has
    /// for the title in a single response. The bundle's `summaries` list
    /// stays empty because Fanart does not serve summary text.
    pub async fn fetch_movie_art_bundle(&self, imdb_id: &str) -> Result<ProviderArtBundle, Error> {
        let url = format!("{}/v3/movies/{imdb_id}", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str())])
            },
            &self.config,
        )
        .await?;
        let raw: MovieArt = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("fanart movie {imdb_id}: {e}")))?;
        Ok(ProviderArtBundle {
            posters: convert(raw.movieposter),
            backdrops: convert(raw.moviebackground),
            logos: convert(raw.hdmovielogo),
            clearart: convert(raw.movieclearart),
            summaries: Vec::new(),
        })
    }

    /// Fetch the artwork bundle for a show (PRD §F-005).
    ///
    /// Fanart.tv TV is keyed by **TVDB** id (not `IMDb`). The `/v3/tv/{id}`
    /// endpoint returns all artwork categories. As with movies, `summaries`
    /// is empty.
    pub async fn fetch_show_art_bundle(&self, tvdb_id: u64) -> Result<ProviderArtBundle, Error> {
        let url = format!("{}/v3/tv/{tvdb_id}", self.base_url);
        let response = fetch_with_retry(
            || {
                self.client
                    .get(&url)
                    .query(&[("api_key", self.key.as_str())])
            },
            &self.config,
        )
        .await?;
        let raw: ShowArt = response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("fanart tv {tvdb_id}: {e}")))?;
        Ok(ProviderArtBundle {
            posters: convert(raw.tvposter),
            backdrops: convert(raw.showbackground),
            logos: convert(raw.hdtvlogo),
            clearart: convert(raw.clearart),
            summaries: Vec::new(),
        })
    }
}

/// Shape of the `/v3/movies/{imdb_id}` response, narrowed to the fields
/// F-005 cares about. Fanart returns many extra categories; serde ignores
/// them.
#[derive(Debug, Default, Deserialize)]
struct MovieArt {
    #[serde(default)]
    movieposter: Vec<RawAsset>,
    #[serde(default)]
    moviebackground: Vec<RawAsset>,
    #[serde(default)]
    hdmovielogo: Vec<RawAsset>,
    #[serde(default)]
    movieclearart: Vec<RawAsset>,
}

/// Shape of the `/v3/tv/{tvdb_id}` response, narrowed similarly.
#[derive(Debug, Default, Deserialize)]
struct ShowArt {
    #[serde(default)]
    tvposter: Vec<RawAsset>,
    #[serde(default)]
    showbackground: Vec<RawAsset>,
    #[serde(default)]
    hdtvlogo: Vec<RawAsset>,
    #[serde(default)]
    clearart: Vec<RawAsset>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    url: String,
    #[serde(default)]
    lang: Option<String>,
}

fn convert(raw: Vec<RawAsset>) -> Vec<LangAsset> {
    raw.into_iter()
        .map(|a| LangAsset {
            lang: a.lang.unwrap_or_default(),
            url: a.url,
        })
        .collect()
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

    #[tokio::test]
    async fn fetch_movie_art_bundle_maps_all_categories() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/movies/tt0133093"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "The Matrix",
                "imdb_id": "tt0133093",
                "movieposter": [
                    { "url": "https://f.example/poster-en.jpg", "lang": "en" },
                    { "url": "https://f.example/poster-fr.jpg", "lang": "fr" }
                ],
                "moviebackground": [
                    { "url": "https://f.example/back.jpg", "lang": "00" }
                ],
                "hdmovielogo": [
                    { "url": "https://f.example/logo-en.png", "lang": "en" }
                ],
                "movieclearart": [
                    { "url": "https://f.example/clear-en.png", "lang": "en" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            FanartClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client.fetch_movie_art_bundle("tt0133093").await.unwrap();
        assert_eq!(bundle.posters.len(), 2);
        assert_eq!(bundle.posters[0].lang, "en");
        assert_eq!(bundle.posters[0].url, "https://f.example/poster-en.jpg");
        assert_eq!(bundle.backdrops.len(), 1);
        assert_eq!(bundle.backdrops[0].lang, "00");
        assert_eq!(bundle.logos.len(), 1);
        assert_eq!(bundle.clearart.len(), 1);
        assert!(
            bundle.summaries.is_empty(),
            "Fanart does not serve summaries"
        );
    }

    #[tokio::test]
    async fn fetch_show_art_bundle_keys_on_tvdb_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/tv/78878"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "Some Show",
                "thetvdb_id": "78878",
                "tvposter": [
                    { "url": "https://f.example/tvposter.jpg", "lang": "en" }
                ],
                "showbackground": [],
                "hdtvlogo": [
                    { "url": "https://f.example/tvlogo.png", "lang": "en" }
                ],
                "clearart": [
                    { "url": "https://f.example/tvclear.png", "lang": "fr" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            FanartClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client.fetch_show_art_bundle(78_878).await.unwrap();
        assert_eq!(bundle.posters.len(), 1);
        assert!(bundle.backdrops.is_empty());
        assert_eq!(bundle.logos.len(), 1);
        assert_eq!(bundle.clearart[0].lang, "fr");
    }

    #[tokio::test]
    async fn fetch_movie_art_bundle_handles_missing_categories() {
        let server = MockServer::start().await;
        // Response carries only one category; the others default to empty.
        Mock::given(method("GET"))
            .and(path("/v3/movies/tt0000001"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "X",
                "movieposter": [
                    { "url": "https://f.example/p.jpg", "lang": "en" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            FanartClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client.fetch_movie_art_bundle("tt0000001").await.unwrap();
        assert_eq!(bundle.posters.len(), 1);
        assert!(bundle.backdrops.is_empty());
        assert!(bundle.logos.is_empty());
        assert!(bundle.clearart.is_empty());
    }
}
