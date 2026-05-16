//! Locked numeric constants from PRD §8.
//!
//! Any change to a value in this module is a PRD revision and requires human
//! sign-off. Subsystems consume these via re-export rather than redefining.

/// Seconds of playback that must remain after estimated download completion
/// for playback to proceed without rebuffering (PRD §F-014, §8).
pub const SAFETY_MARGIN_S: f64 = 30.0;

/// Initial prebuffer target in seconds before playback starts (PRD §F-014, §8).
pub const PREBUFFER_TARGET_S: f64 = 15.0;

/// HIGHEST piece-priority window ahead of the playhead, in seconds (PRD §F-014).
pub const PIECE_PRIORITY_HIGH_WINDOW_S: f64 = 60.0;

/// HIGH piece-priority window ahead of the playhead, in seconds (PRD §F-014).
pub const PIECE_PRIORITY_MED_WINDOW_S: f64 = 300.0;

/// Rolling-average window for the download rate estimator (PRD §F-014).
pub const DL_RATE_WINDOW_S: f64 = 30.0;

/// Buffer state-machine recompute cadence (PRD §F-014).
pub const RECOMPUTE_INTERVAL_S: f64 = 5.0;

/// `pieces_ahead_seconds` polling cadence in milliseconds (PRD §F-014).
pub const AHEAD_CHECK_INTERVAL_MS: u64 = 250;

/// Default torrent cache size on Linux, in gibibytes (PRD §F-013, §8).
pub const CACHE_DEFAULT_LINUX_GIB: u32 = 4;

/// Default torrent cache size on Android, in gibibytes (PRD §F-013, §8).
pub const CACHE_DEFAULT_ANDROID_GIB: u32 = 2;

/// Maximum in-flight stream-availability requests (PRD §F-006).
pub const AVAILABILITY_CONCURRENCY: usize = 8;

/// Per-request timeout for stream-availability checks, in seconds (PRD §F-006).
pub const AVAILABILITY_TIMEOUT_S: u64 = 5;

/// Cache TTL for `stream_availability` entries, in seconds (30 min, PRD §F-006).
pub const STREAM_AVAILABILITY_TTL_S: u64 = 1_800;

/// Cache TTL for trending payloads, in seconds (6 h, PRD §8).
pub const TRENDING_TTL_S: u64 = 21_600;

/// Cache TTL for title metadata, in seconds (24 h, PRD §8).
pub const META_TTL_S: u64 = 86_400;

/// Cache TTL for search results, in seconds (1 h, PRD §8).
pub const SEARCH_TTL_S: u64 = 3_600;

/// Cache TTL for resolved artwork, in seconds (7 d, PRD §F-005, §8).
pub const ARTWORK_TTL_S: u64 = 604_800;

/// Default outbound HTTP request timeout (PRD §F-003, §8).
pub const HTTP_TIMEOUT_S: u64 = 10;

/// Backoff schedule for 5xx/429 retries, in seconds (PRD §F-003, §8).
pub const HTTP_RETRY_BACKOFF_S: [u64; 3] = [1, 2, 4];

/// Debounce window for live search input, in milliseconds (PRD §F-011, §8).
pub const SEARCH_DEBOUNCE_MS: u64 = 300;

/// Search infinite-scroll page size (PRD §F-011, §8).
pub const SEARCH_PAGE_SIZE: usize = 20;

/// Number of recent searches surfaced when the search input is empty
/// (PRD §F-011, §8).
pub const RECENT_SEARCHES_MAX: usize = 10;

/// Continue Watching completion fraction; ≥ this counts the title as watched
/// (PRD §F-012, §8).
pub const CW_COMPLETION_THRESHOLD: f64 = 0.95;

/// Continue Watching auto-removal age for completed titles, in seconds (24 h).
pub const CW_AUTOREMOVE_S: u64 = 86_400;

/// Player position-event cadence, in seconds (PRD §F-012, §8).
pub const PLAYER_POSITION_INTERVAL_S: u64 = 5;

/// Trending result count surfaced to the home screen (PRD §F-004, §8).
pub const TRENDING_RESULT_COUNT: usize = 50;

/// Top-trending pool cutoff, as a quantile of merged trending score
/// (PRD §F-004, §8).
pub const TOP_TRENDING_QUARTILE: f64 = 0.25;

/// Minimum rating for a title to be eligible for the hidden-gems pool
/// (PRD §F-004, §8).
pub const HIDDEN_GEMS_RATING_THRESHOLD: f64 = 7.5;

/// The trending list alternation pattern. `true` = top-trending slot,
/// `false` = hidden-gems slot. Repeats until [`TRENDING_RESULT_COUNT`] is
/// reached. PRD §F-004, §8 spec is `[T, T, T, G, G]`.
pub const FINAL_LIST_PATTERN: [bool; 5] = [true, true, true, false, false];

/// librqbit `max_connections_per_torrent` (PRD §F-013, §8).
pub const MAX_CONNECTIONS_PER_TORRENT: u32 = 200;

// Cross-constant invariants enforced at compile time. If any of these fire,
// the build will fail before tests run — and any value bump that breaks the
// PRD math will be caught here rather than at runtime.
const _: () = assert!(PIECE_PRIORITY_HIGH_WINDOW_S < PIECE_PRIORITY_MED_WINDOW_S);
const _: () = assert!(PREBUFFER_TARGET_S < SAFETY_MARGIN_S);
const _: () = assert!(CW_COMPLETION_THRESHOLD > 0.0 && CW_COMPLETION_THRESHOLD < 1.0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_list_pattern_matches_prd() {
        // Spec says [T, T, T, G, G] (PRD §8).
        assert_eq!(FINAL_LIST_PATTERN, [true, true, true, false, false]);
    }

    #[test]
    fn retry_backoff_is_locked() {
        // Spec says [1, 2, 4] (PRD §F-003, §8).
        assert_eq!(HTTP_RETRY_BACKOFF_S, [1, 2, 4]);
    }
}
