//! Stream-availability persistence row shape (PRD §F-006).
//!
//! Mirrors the `stream_availability` table in `migrations/0001_init.sql`.
//! The F-006 dispatch (`src-tauri::commands::check_availability`) writes one
//! row per `(title_id, kind, source_id)` triple: whether the addon returned
//! any streams, plus the Unix-seconds timestamp the check was made at.
//! Reads honor the PRD §8 30-minute TTL via [`Db::availability_get_fresh`].

use crate::title::TitleKind;
use serde::{Deserialize, Serialize};

/// One persisted availability check result. The primary key is the triple
/// `(title_id, kind, source_id)`; checks for the same triple replace any
/// prior row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailabilityRow {
    /// The catalog item identifier (e.g. `tmdb:603`, `imdb:tt0133093`).
    pub title_id: String,
    /// Title kind the check was scoped to.
    pub kind: TitleKind,
    /// The persisted addon id (the manifest's `id` field) the check ran
    /// against.
    pub source_id: String,
    /// True if the addon returned a non-empty stream list.
    pub has_streams: bool,
    /// Unix epoch seconds the check completed at.
    pub checked_at: i64,
}
