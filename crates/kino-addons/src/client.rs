//! Stremio addon protocol client (PRD §F-007).
//!
//! One [`AddonClient`] is bound to a single addon's normalized base URL
//! (e.g. `https://v3-cinemeta.strem.io`). All seven protocol endpoints —
//! manifest, catalog (no-skip / paginated / search variants), meta, stream,
//! subtitles — are implemented on it.
//!
//! HTTP retry / timeout / User-Agent are inherited from
//! `kino_core::http::HttpConfig`; the locked policy (PRD §F-003 / §8) is
//! honored uniformly across metadata providers and addons (ADR-055).

use kino_core::http::{fetch_with_retry, HttpConfig, HttpError};

use crate::manifest::{parse_manifest, Manifest};
use crate::protocol::{CatalogResponse, MetaResponse, StreamResponse, SubtitlesResponse};
use crate::url::{base_url_from_manifest, normalize_manifest_url};
use crate::AddonError;

/// Stremio addon protocol client. One instance per addon.
#[derive(Debug, Clone)]
pub struct AddonClient {
    base_url: String,
    config: HttpConfig,
    client: reqwest::Client,
}

impl AddonClient {
    /// Build a client from a manifest URL. The URL is normalized
    /// (`stremio://` → `https://`) and the addon base URL stored for
    /// subsequent protocol calls.
    pub fn new(manifest_url: &str) -> Result<Self, AddonError> {
        Self::with_options(manifest_url, HttpConfig::default())
    }

    /// Build a client with an explicit `HttpConfig`. Used by tests
    /// (wiremock) and by future per-addon timeout overrides.
    pub fn with_options(manifest_url: &str, config: HttpConfig) -> Result<Self, AddonError> {
        let normalized = normalize_manifest_url(manifest_url)?;
        let base = base_url_from_manifest(&normalized).to_string();
        let client = config.build_client()?;
        Ok(Self {
            base_url: base,
            config,
            client,
        })
    }

    /// Returns the addon's base URL (the manifest URL with `/manifest.json`
    /// stripped). Useful for diagnostics.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Fetch and validate the addon's manifest (`GET /manifest.json`).
    ///
    /// Validation enforces PRD §F-007 required fields (`id`, `version`,
    /// `name`, `types`, `resources`, `catalogs`); failures surface as
    /// [`AddonError::Manifest`].
    pub async fn manifest(&self) -> Result<Manifest, AddonError> {
        let url = format!("{}/manifest.json", self.base_url);
        let body = self.get_text(&url).await?;
        parse_manifest(&body).map_err(AddonError::from)
    }

    /// Fetch a non-paginated catalog (`GET /catalog/{type}/{id}.json`).
    pub async fn catalog(&self, kind: &str, id: &str) -> Result<CatalogResponse, AddonError> {
        let url = format!(
            "{}/catalog/{}/{}.json",
            self.base_url,
            urlencode_path(kind),
            urlencode_path(id)
        );
        self.get_json(&url).await
    }

    /// Fetch a paginated catalog page
    /// (`GET /catalog/{type}/{id}/skip={skip}.json`).
    pub async fn catalog_skip(
        &self,
        kind: &str,
        id: &str,
        skip: u64,
    ) -> Result<CatalogResponse, AddonError> {
        let url = format!(
            "{}/catalog/{}/{}/skip={}.json",
            self.base_url,
            urlencode_path(kind),
            urlencode_path(id),
            skip
        );
        self.get_json(&url).await
    }

    /// Search a catalog
    /// (`GET /catalog/{type}/{id}/search={q}.json`).
    pub async fn catalog_search(
        &self,
        kind: &str,
        id: &str,
        query: &str,
    ) -> Result<CatalogResponse, AddonError> {
        let url = format!(
            "{}/catalog/{}/{}/search={}.json",
            self.base_url,
            urlencode_path(kind),
            urlencode_path(id),
            urlencode_query(query)
        );
        self.get_json(&url).await
    }

    /// Fetch title metadata (`GET /meta/{type}/{id}.json`).
    pub async fn meta(&self, kind: &str, id: &str) -> Result<MetaResponse, AddonError> {
        let url = format!(
            "{}/meta/{}/{}.json",
            self.base_url,
            urlencode_path(kind),
            urlencode_path(id)
        );
        self.get_json(&url).await
    }

    /// Fetch streams for a title (`GET /stream/{type}/{id}.json`).
    pub async fn stream(&self, kind: &str, id: &str) -> Result<StreamResponse, AddonError> {
        let url = format!(
            "{}/stream/{}/{}.json",
            self.base_url,
            urlencode_path(kind),
            urlencode_path(id)
        );
        self.get_json(&url).await
    }

    /// Fetch subtitles for a title (`GET /subtitles/{type}/{id}.json`).
    pub async fn subtitles(&self, kind: &str, id: &str) -> Result<SubtitlesResponse, AddonError> {
        let url = format!(
            "{}/subtitles/{}/{}.json",
            self.base_url,
            urlencode_path(kind),
            urlencode_path(id)
        );
        self.get_json(&url).await
    }

    async fn get_text(&self, url: &str) -> Result<String, HttpError> {
        let response = fetch_with_retry(|| self.client.get(url), &self.config).await?;
        response.text().await.map_err(HttpError::from)
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, AddonError> {
        let response = fetch_with_retry(|| self.client.get(url), &self.config).await?;
        response
            .json()
            .await
            .map_err(|e| AddonError::Decode(e.to_string()))
    }
}

/// Percent-encode a path segment (kind / id). We escape the small set of
/// characters the Stremio protocol allows in identifiers but that would
/// otherwise break URL parsing on the addon side: `/`, `?`, `#`, space.
fn urlencode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '/' => out.push_str("%2F"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            ' ' => out.push_str("%20"),
            _ => out.push(c),
        }
    }
    out
}

/// Percent-encode the query portion of the `search=...` segment. Stremio
/// addons differ on which terminators they tolerate; we escape `&`, `?`,
/// `#`, and space, leaving alphanumerics + unicode untouched (so a
/// non-ASCII search query like `"Amélie"` round-trips intact).
fn urlencode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("%26"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            ' ' => out.push_str("%20"),
            '/' => out.push_str("%2F"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client_for(server: &MockServer) -> AddonClient {
        let manifest_url = format!("{}/manifest.json", server.uri());
        AddonClient::with_options(&manifest_url, HttpConfig::for_test()).unwrap()
    }

    fn cinemeta_manifest_body() -> String {
        r#"{
            "id": "com.linvo.cinemeta",
            "version": "3.0.13",
            "name": "Cinemeta",
            "description": "The official addon for movies and series catalogs.",
            "types": ["movie", "series"],
            "resources": ["catalog", "meta"],
            "catalogs": [
                {"type": "movie", "id": "top", "name": "Popular"}
            ]
        }"#
        .to_string()
    }

    #[tokio::test]
    async fn manifest_parses_valid_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(cinemeta_manifest_body()))
            .mount(&server)
            .await;
        let client = client_for(&server);
        let m = client.manifest().await.unwrap();
        assert_eq!(m.id, "com.linvo.cinemeta");
        assert_eq!(m.name, "Cinemeta");
    }

    #[tokio::test]
    async fn manifest_rejects_invalid_body_with_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"not": "a manifest"}"#))
            .mount(&server)
            .await;
        let client = client_for(&server);
        let err = client.manifest().await.unwrap_err();
        assert!(matches!(err, AddonError::Manifest(_)), "got: {err}");
    }

    #[tokio::test]
    async fn manifest_propagates_http_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/manifest.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = client_for(&server);
        let err = client.manifest().await.unwrap_err();
        assert!(matches!(err, AddonError::Http(_)), "got: {err}");
    }

    #[tokio::test]
    async fn catalog_fetches_basic_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalog/movie/top.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"metas": [{"id": "tt1", "type": "movie", "name": "A"}]}"#),
            )
            .mount(&server)
            .await;
        let client = client_for(&server);
        let resp = client.catalog("movie", "top").await.unwrap();
        assert_eq!(resp.metas.len(), 1);
        assert_eq!(resp.metas[0].id, "tt1");
    }

    #[tokio::test]
    async fn catalog_skip_fetches_paginated_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalog/series/top/skip=20.json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    r#"{"metas": [{"id": "tt9", "type": "series", "name": "S"}]}"#,
                ),
            )
            .mount(&server)
            .await;
        let client = client_for(&server);
        let resp = client.catalog_skip("series", "top", 20).await.unwrap();
        assert_eq!(resp.metas[0].id, "tt9");
    }

    #[tokio::test]
    async fn catalog_search_encodes_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalog/movie/top/search=The%20Matrix.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"metas": [{"id": "tt0133093", "type": "movie", "name": "The Matrix"}]}"#,
            ))
            .mount(&server)
            .await;
        let client = client_for(&server);
        let resp = client
            .catalog_search("movie", "top", "The Matrix")
            .await
            .unwrap();
        assert_eq!(resp.metas[0].id, "tt0133093");
    }

    #[tokio::test]
    async fn meta_fetches_title_details() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meta/movie/tt0133093.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"meta": {"id": "tt0133093", "type": "movie", "name": "The Matrix", "runtime": "136 min"}}"#,
            ))
            .mount(&server)
            .await;
        let client = client_for(&server);
        let resp = client.meta("movie", "tt0133093").await.unwrap();
        assert_eq!(resp.meta.name, "The Matrix");
        assert_eq!(resp.meta.runtime.as_deref(), Some("136 min"));
    }

    #[tokio::test]
    async fn stream_fetches_streams() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stream/movie/tt0133093.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"streams": [{"name": "src", "infoHash": "abc", "fileIdx": 0}]}"#,
            ))
            .mount(&server)
            .await;
        let client = client_for(&server);
        let resp = client.stream("movie", "tt0133093").await.unwrap();
        assert_eq!(resp.streams.len(), 1);
        assert_eq!(resp.streams[0].info_hash.as_deref(), Some("abc"));
    }

    #[tokio::test]
    async fn subtitles_fetches_subtitle_tracks() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/subtitles/movie/tt0133093.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"subtitles": [{"id": "1", "url": "https://s/eng.vtt", "lang": "eng"}]}"#,
            ))
            .mount(&server)
            .await;
        let client = client_for(&server);
        let resp = client.subtitles("movie", "tt0133093").await.unwrap();
        assert_eq!(resp.subtitles[0].lang, "eng");
    }

    #[tokio::test]
    async fn stremio_scheme_normalizes_to_https() {
        // We don't actually fire a request here — we just confirm the
        // constructor doesn't reject the stremio:// form and the base URL
        // is normalized so subsequent calls would target https://.
        let result = AddonClient::with_options(
            "stremio://example.com/manifest.json",
            HttpConfig::for_test(),
        );
        let client = result.unwrap();
        assert_eq!(client.base_url(), "https://example.com");
    }

    #[tokio::test]
    async fn invalid_url_rejected_at_construction() {
        let result =
            AddonClient::with_options("ftp://example.com/manifest.json", HttpConfig::for_test());
        assert!(matches!(result, Err(AddonError::InvalidUrl(_))));
    }

    #[test]
    fn urlencode_path_escapes_protocol_special_chars() {
        assert_eq!(urlencode_path("a/b"), "a%2Fb");
        assert_eq!(urlencode_path("hello world"), "hello%20world");
        assert_eq!(urlencode_path("tt0133093"), "tt0133093");
    }

    #[test]
    fn urlencode_query_handles_unicode() {
        assert_eq!(urlencode_query("Amélie"), "Amélie");
        assert_eq!(urlencode_query("a & b"), "a%20%26%20b");
    }
}
