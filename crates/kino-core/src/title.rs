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
}
