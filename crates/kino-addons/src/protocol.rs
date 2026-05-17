//! Stremio protocol response shapes (PRD §F-007).
//!
//! These types describe what addons return from the catalog / meta / stream
//! / subtitles endpoints. They are intentionally permissive: Stremio addons
//! in the wild include extra fields, and the protocol is documented loosely.
//! We accept the documented fields and pass the rest through verbatim where
//! it matters (e.g. `behaviorHints` on streams) so downstream features —
//! F-006 source availability, F-010 stream rows, F-015 player metadata —
//! can read addon-supplied data without re-fetching.
//!
//! Each top-level response is wrapped in a Stremio "envelope": the addon
//! returns `{"<resource>": [...], "cacheMaxAge": N}` and our parser unwraps
//! the inner array.

use serde::{Deserialize, Serialize};

/// Catalog response (`/catalog/{type}/{id}.json` and variants).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogResponse {
    /// Catalog items returned by the addon, in addon-defined order.
    #[serde(default, rename = "metas")]
    pub metas: Vec<MetaPreview>,
    /// Optional cache hint, in seconds. Kept verbatim for downstream cache
    /// integration (PRD §8 catalog TTLs).
    #[serde(default, rename = "cacheMaxAge")]
    pub cache_max_age: Option<u64>,
}

/// Title metadata response (`/meta/{type}/{id}.json`). Stremio wraps this in
/// a `meta` field. The shape overlaps significantly with [`MetaPreview`] —
/// addons commonly serve the same fields from both endpoints, with the meta
/// endpoint adding `videos[]` for series and richer relationships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaResponse {
    pub meta: MetaDetail,
    #[serde(default, rename = "cacheMaxAge")]
    pub cache_max_age: Option<u64>,
}

/// Stream response (`/stream/{type}/{id}.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamResponse {
    #[serde(default)]
    pub streams: Vec<Stream>,
    #[serde(default, rename = "cacheMaxAge")]
    pub cache_max_age: Option<u64>,
}

/// Subtitle response (`/subtitles/{type}/{id}.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitlesResponse {
    #[serde(default)]
    pub subtitles: Vec<Subtitle>,
    #[serde(default, rename = "cacheMaxAge")]
    pub cache_max_age: Option<u64>,
}

/// Catalog row item ("meta preview" in Stremio terminology).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaPreview {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub logo: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "releaseInfo")]
    pub release_info: Option<String>,
    #[serde(default, rename = "imdbRating")]
    pub imdb_rating: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    /// Carry-through for fields the addon includes but we don't model
    /// explicitly. Downstream UI rendering uses this for addon-specific
    /// extras (badges, region hints, etc.).
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Full metadata for a single title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaDetail {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub logo: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "releaseInfo")]
    pub release_info: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default, rename = "imdbRating")]
    pub imdb_rating: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub director: Vec<String>,
    #[serde(default)]
    pub cast: Vec<String>,
    /// For series: the season/episode listing. Each entry is a
    /// `Stremio` "video" record. F-010 series rendering iterates this.
    #[serde(default)]
    pub videos: Vec<MetaVideo>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Per-episode entry inside a series' [`MetaDetail::videos`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaVideo {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub season: Option<i64>,
    #[serde(default)]
    pub episode: Option<i64>,
    #[serde(default)]
    pub released: Option<String>,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
}

/// One stream entry returned by a stream-serving addon.
///
/// Stremio's stream shape is one-of: `url` (direct playable), `infoHash`
/// (`BitTorrent`), `ytId` (`YouTube`), or `externalUrl` (open elsewhere).
/// We surface all four; the F-006 availability check considers any
/// non-empty response "available", and F-015 player selection picks
/// per-stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stream {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, rename = "infoHash")]
    pub info_hash: Option<String>,
    #[serde(default, rename = "fileIdx")]
    pub file_idx: Option<i64>,
    #[serde(default, rename = "ytId")]
    pub yt_id: Option<String>,
    #[serde(default, rename = "externalUrl")]
    pub external_url: Option<String>,
    #[serde(default, rename = "behaviorHints")]
    pub behavior_hints: serde_json::Value,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// One subtitle track entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subtitle {
    pub id: String,
    pub url: String,
    pub lang: String,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_response_round_trips() {
        let body = r#"{
            "metas": [
                {"id": "tt1234", "type": "movie", "name": "X", "poster": "https://p"},
                {"id": "tt5678", "type": "movie", "name": "Y"}
            ],
            "cacheMaxAge": 3600
        }"#;
        let parsed: CatalogResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.metas.len(), 2);
        assert_eq!(parsed.metas[0].id, "tt1234");
        assert_eq!(parsed.metas[0].poster.as_deref(), Some("https://p"));
        assert_eq!(parsed.cache_max_age, Some(3600));
    }

    #[test]
    fn stream_response_accepts_info_hash() {
        let body = r#"{
            "streams": [
                {"name": "torrentio", "infoHash": "deadbeef", "fileIdx": 0, "sources": ["dht"]}
            ]
        }"#;
        let parsed: StreamResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.streams.len(), 1);
        assert_eq!(parsed.streams[0].info_hash.as_deref(), Some("deadbeef"));
        assert_eq!(parsed.streams[0].file_idx, Some(0));
        assert_eq!(parsed.streams[0].sources, vec!["dht"]);
    }

    #[test]
    fn stream_response_accepts_direct_url() {
        let body = r#"{"streams": [{"name": "pdm", "url": "https://archive.org/m.mp4"}]}"#;
        let parsed: StreamResponse = serde_json::from_str(body).unwrap();
        assert_eq!(
            parsed.streams[0].url.as_deref(),
            Some("https://archive.org/m.mp4")
        );
        assert!(parsed.streams[0].info_hash.is_none());
    }

    #[test]
    fn meta_response_includes_videos_for_series() {
        let body = r#"{
            "meta": {
                "id": "tt123",
                "type": "series",
                "name": "Show",
                "videos": [
                    {"id": "tt123:1:1", "title": "Pilot", "season": 1, "episode": 1}
                ]
            }
        }"#;
        let parsed: MetaResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.meta.videos.len(), 1);
        assert_eq!(parsed.meta.videos[0].season, Some(1));
    }

    #[test]
    fn subtitles_response_round_trips() {
        let body = r#"{
            "subtitles": [
                {"id": "1", "url": "https://subs/eng.vtt", "lang": "eng"}
            ]
        }"#;
        let parsed: SubtitlesResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.subtitles[0].lang, "eng");
    }
}
