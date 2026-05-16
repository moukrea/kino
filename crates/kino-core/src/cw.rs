//! Continue Watching domain type.
//!
//! Mirrors the `continue_watching` schema in `migrations/0001_init.sql`
//! (PRD §F-002). Episodes of a series store `season > 0` / `episode > 0`;
//! movies use `season = 0` / `episode = 0` to share the row shape.

use serde::{Deserialize, Serialize};

use crate::title::TitleKind;

/// A single Continue Watching row.
///
/// `meta_json` holds the title summary the home screen needs to render the
/// row without a fresh metadata fetch (poster, title, year, …). The schema
/// is intentionally free-form — the consumers (F-008 Home, F-012 CW) decide
/// what to put there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinueWatching {
    pub title_id: String,
    pub kind: TitleKind,
    pub season: i64,
    pub episode: i64,
    pub position_s: f64,
    pub duration_s: f64,
    /// Unix epoch seconds.
    pub last_played_at: i64,
    pub meta_json: serde_json::Value,
}

impl ContinueWatching {
    /// Fraction of the title that has been played, in `[0.0, 1.0]`.
    ///
    /// Returns `0.0` if `duration_s` is non-positive (avoids `NaN` /
    /// division-by-zero leaking to UI code).
    #[must_use]
    pub fn progress(&self) -> f64 {
        if self.duration_s <= 0.0 {
            0.0
        } else {
            (self.position_s / self.duration_s).clamp(0.0, 1.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_clamps_to_unit_interval() {
        let mut cw = ContinueWatching {
            title_id: "tt0111161".into(),
            kind: TitleKind::Movie,
            season: 0,
            episode: 0,
            position_s: 60.0,
            duration_s: 120.0,
            last_played_at: 0,
            meta_json: serde_json::json!({}),
        };
        assert!((cw.progress() - 0.5).abs() < f64::EPSILON);

        cw.position_s = -1.0;
        assert!((cw.progress() - 0.0).abs() < f64::EPSILON);

        cw.position_s = 999.0;
        assert!((cw.progress() - 1.0).abs() < f64::EPSILON);

        cw.duration_s = 0.0;
        assert!((cw.progress() - 0.0).abs() < f64::EPSILON);
    }
}
