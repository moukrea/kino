//! Title-level data types.
//!
//! These shapes are intentionally minimal in Session 001. They will grow as
//! the metadata clients (F-003) and home/detail views (F-008, F-010) land.

use serde::{Deserialize, Serialize};

/// Whether a catalog item is a film or a series. Matches the values used by
/// the Stremio addon protocol (PRD §F-007).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleKind {
    Movie,
    Series,
}

impl TitleKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "movie",
            Self::Series => "series",
        }
    }
}

/// Compact title shape used by catalog rows, search results, and the
/// trending aggregator (F-004).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TitleSummary {
    pub id: String,
    pub kind: TitleKind,
    pub title: String,
    pub year: Option<u16>,
    pub poster: Option<String>,
    pub rating: Option<f64>,
}

/// Image type resolved by F-005's cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageType {
    Poster,
    Backdrop,
    Logo,
    Clearart,
}

/// Per-image-type source markers (provider + language) for the resolved
/// [`Artwork`]. Surfaced to the frontend as debug metadata so reviewers can
/// see which tier won for each asset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Provenance {
    pub poster: String,
    pub backdrop: String,
    pub logo: String,
    pub clearart: String,
    pub summary: String,
}

/// Resolved artwork bundle returned by the F-005 `resolve_artwork` Tauri
/// command.
///
/// URLs are always non-empty: if every tier yielded nothing, the field holds
/// a `kino://placeholder/...` sentinel URL (see `kino_metadata::artwork`).
/// Summary may be empty when no provider served one — tier 6 yields the empty
/// string per PRD §F-005.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artwork {
    pub poster: String,
    pub backdrop: String,
    pub logo: String,
    pub clearart: String,
    pub summary: String,
    pub sources: Provenance,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_serializes_lowercase() {
        let s = serde_json::to_string(&TitleKind::Movie).unwrap();
        assert_eq!(s, "\"movie\"");
        let s = serde_json::to_string(&TitleKind::Series).unwrap();
        assert_eq!(s, "\"series\"");
    }

    #[test]
    fn kind_as_str_matches_protocol() {
        assert_eq!(TitleKind::Movie.as_str(), "movie");
        assert_eq!(TitleKind::Series.as_str(), "series");
    }

    #[test]
    fn artwork_round_trips_through_json() {
        let art = Artwork {
            poster: "https://p".to_string(),
            backdrop: "https://b".to_string(),
            logo: "https://l".to_string(),
            clearart: "https://c".to_string(),
            summary: "S".to_string(),
            sources: Provenance {
                poster: "fanart.tv:en".to_string(),
                backdrop: "tmdb:en".to_string(),
                logo: "tvdb:en".to_string(),
                clearart: "placeholder".to_string(),
                summary: "tmdb:en".to_string(),
            },
        };
        let json = serde_json::to_string(&art).unwrap();
        let round: Artwork = serde_json::from_str(&json).unwrap();
        assert_eq!(round, art);
    }
}
