//! Fanart.tv v3 client (PRD §F-003).
//!
//! Fanart.tv exposes artwork via `webservice.fanart.tv/v3/{movies,tv}/<id>`.
//! Authentication is a single `api_key` query parameter; absence or a wrong
//! key yields a 401/403.
//!
//! [`FanartClient::test_credentials`] hits `/v3/movies/tt0111161` (Shawshank,
//! a stable known-good `IMDb` id) to confirm the key works without coupling
//! the test to any other catalog state.

use reqwest::StatusCode;
use serde::Deserialize;

use crate::artwork::{LocalizedAsset, ProviderBundle};
use crate::error::Error;
use kino_core::http::{fetch_with_retry, HttpConfig};

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

    /// Fetch artwork for a movie. Fanart.tv movies are keyed by either TMDB
    /// id (numeric) or `IMDb` id (`ttNNNN...`). Returns `Ok(None)` when the
    /// catalog has no entry (HTTP 404); other errors propagate.
    pub async fn movie_artwork(&self, id: &str) -> Result<Option<ProviderBundle>, Error> {
        let url = format!("{}/v3/movies/{id}", self.base_url);
        match self.fetch_movie(&url).await {
            Ok(body) => Ok(Some(ProviderBundle {
                posters: collect_assets(body.movieposter),
                backdrops: collect_assets(body.moviebackground),
                logos: collect_assets(body.hdmovielogo),
                clearart: collect_assets(body.hdmovieclearart),
                summaries: std::collections::HashMap::new(),
            })),
            Err(Error::Http { status, .. }) if status == StatusCode::NOT_FOUND.as_u16() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Fetch artwork for a TV show. Fanart.tv keys TV shows by TVDB id only;
    /// `tvdb_id` must be the numeric `series_id` from TVDB.
    pub async fn show_artwork(&self, tvdb_id: u64) -> Result<Option<ProviderBundle>, Error> {
        let url = format!("{}/v3/tv/{tvdb_id}", self.base_url);
        match self.fetch_show(&url).await {
            Ok(body) => Ok(Some(ProviderBundle {
                posters: collect_assets(body.tvposter),
                backdrops: collect_assets(body.showbackground),
                logos: collect_assets(body.hdtvlogo),
                clearart: collect_assets(body.hdclearart),
                summaries: std::collections::HashMap::new(),
            })),
            Err(Error::Http { status, .. }) if status == StatusCode::NOT_FOUND.as_u16() => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn fetch_movie(&self, url: &str) -> Result<MovieArtwork, Error> {
        let response = fetch_with_retry(
            || {
                self.client
                    .get(url)
                    .query(&[("api_key", self.key.as_str())])
            },
            &self.config,
        )
        .await?;
        response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("fanart movie: {e}")))
    }

    async fn fetch_show(&self, url: &str) -> Result<ShowArtwork, Error> {
        let response = fetch_with_retry(
            || {
                self.client
                    .get(url)
                    .query(&[("api_key", self.key.as_str())])
            },
            &self.config,
        )
        .await?;
        response
            .json()
            .await
            .map_err(|e| Error::Decode(format!("fanart show: {e}")))
    }
}

#[derive(Debug, Deserialize)]
struct MovieArtwork {
    #[serde(default)]
    movieposter: Vec<FanartAsset>,
    #[serde(default)]
    moviebackground: Vec<FanartAsset>,
    #[serde(default)]
    hdmovielogo: Vec<FanartAsset>,
    #[serde(default)]
    hdmovieclearart: Vec<FanartAsset>,
}

#[derive(Debug, Deserialize)]
struct ShowArtwork {
    #[serde(default)]
    tvposter: Vec<FanartAsset>,
    #[serde(default)]
    showbackground: Vec<FanartAsset>,
    #[serde(default)]
    hdtvlogo: Vec<FanartAsset>,
    #[serde(default)]
    hdclearart: Vec<FanartAsset>,
}

#[derive(Debug, Deserialize)]
struct FanartAsset {
    url: String,
    /// Fanart.tv tags assets with 2-letter ISO 639-1 codes (`en`, `fr`).
    /// `00` is their sentinel for textless artwork; we map it to the empty
    /// string so the cascade's `lang_matches` rule applies.
    #[serde(default)]
    lang: Option<String>,
}

fn collect_assets(assets: Vec<FanartAsset>) -> Vec<LocalizedAsset> {
    assets
        .into_iter()
        .map(|a| LocalizedAsset {
            lang: match a.lang.as_deref() {
                None | Some("" | "00") => String::new(),
                Some(other) => other.to_string(),
            },
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
    async fn movie_artwork_decodes_all_image_buckets() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/movies/tt0133093"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "The Matrix",
                "imdb_id": "tt0133093",
                "movieposter": [
                    {"id":"1","url":"https://fanart/m/poster-en.png","lang":"en"},
                    {"id":"2","url":"https://fanart/m/poster-fr.png","lang":"fr"}
                ],
                "moviebackground": [
                    {"id":"3","url":"https://fanart/m/bg.png","lang":"00"}
                ],
                "hdmovielogo": [
                    {"id":"4","url":"https://fanart/m/logo-en.png","lang":"en"}
                ],
                "hdmovieclearart": [
                    {"id":"5","url":"https://fanart/m/clearart-en.png","lang":"en"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            FanartClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client.movie_artwork("tt0133093").await.unwrap().unwrap();
        assert_eq!(bundle.posters.len(), 2);
        assert_eq!(bundle.posters[0].lang, "en");
        assert_eq!(bundle.posters[1].lang, "fr");
        // "00" is Fanart's textless sentinel → empty lang after normalization.
        assert_eq!(bundle.backdrops.len(), 1);
        assert_eq!(bundle.backdrops[0].lang, "");
        assert_eq!(bundle.logos.len(), 1);
        assert_eq!(bundle.clearart.len(), 1);
    }

    #[tokio::test]
    async fn movie_artwork_404_returns_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/movies/tt0000000"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            FanartClient::with_options("test-key", HttpConfig::for_test(), server.uri()).unwrap();
        assert!(client.movie_artwork("tt0000000").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn show_artwork_decodes_tv_buckets() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v3/tv/78878"))
            .and(query_param("api_key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "Lost",
                "thetvdb_id": "78878",
                "tvposter": [
                    {"id":"1","url":"https://fanart/s/poster-en.png","lang":"en"}
                ],
                "showbackground": [
                    {"id":"2","url":"https://fanart/s/bg.png","lang":"00"}
                ],
                "hdtvlogo": [
                    {"id":"3","url":"https://fanart/s/logo-en.png","lang":"en"}
                ],
                "hdclearart": [
                    {"id":"4","url":"https://fanart/s/clearart-fr.png","lang":"fr"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            FanartClient::with_options("test-key", HttpConfig::default(), server.uri()).unwrap();
        let bundle = client.show_artwork(78_878).await.unwrap().unwrap();
        assert_eq!(bundle.posters[0].url, "https://fanart/s/poster-en.png");
        assert_eq!(bundle.backdrops[0].lang, "");
        assert_eq!(bundle.clearart[0].lang, "fr");
    }
}
