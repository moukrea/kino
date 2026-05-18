//! Continue Watching domain type and PRD §F-012 rules.
//!
//! Mirrors the `continue_watching` schema in `migrations/0001_init.sql`
//! (PRD §F-002). Episodes of a series store `season > 0` / `episode > 0`;
//! movies use `season = 0` / `episode = 0` to share the row shape.
//!
//! The free functions in this module implement the locked PRD §F-012
//! semantics that the Tauri host applies on every position write — they
//! live in `kino-core` so unit tests can drive them without bringing up
//! a database or the Tauri runtime.

use serde::{Deserialize, Serialize};

use crate::constants::CW_COMPLETION_THRESHOLD;
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

    /// PRD §F-012 "Mark completed when exit position > 0.95 × duration".
    /// A zero / non-positive `duration_s` is never treated as completed
    /// — a missing or sentinel duration would otherwise cause the
    /// auto-removal sweep to evict legitimate in-progress rows.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.duration_s > 0.0 && self.progress() >= CW_COMPLETION_THRESHOLD
    }
}

/// PRD §F-012 series next-episode resolution. Returns the
/// `(season, episode)` tuple immediately following `(current_season,
/// current_episode)` in the list, or `None` when no next episode exists
/// (i.e. the series is at its last episode).
///
/// `episodes` is the list of `(season, episode)` tuples drawn from the
/// canonical detail metadata (Cinemeta `videos[]` or its TMDB / TVDB
/// equivalent). Order of the input does not matter — the function sorts
/// internally so callers don't need to.
///
/// Season `0` episodes (Stremio "specials" / extras) are excluded from
/// the sequence per the PRD §F-010 convention that specials are not
/// part of the main story order.
#[must_use]
pub fn next_episode_after(
    current_season: i64,
    current_episode: i64,
    episodes: &[(i64, i64)],
) -> Option<(i64, i64)> {
    if current_season <= 0 {
        return None;
    }
    let mut sorted: Vec<(i64, i64)> = episodes.iter().copied().filter(|(s, _)| *s > 0).collect();
    sorted.sort_unstable();
    sorted.dedup();

    let mut iter = sorted.into_iter().peekable();
    while let Some(entry) = iter.next() {
        if entry == (current_season, current_episode) {
            return iter.next();
        }
    }
    None
}

/// PRD §F-012 outcome of recording the final position on a CW row. The
/// `cw_record_position` Tauri command applies one of these on every
/// write so the rule is in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeDecision {
    /// Keep the row at its current `(season, episode, position_s)`.
    Keep,
    /// Series episode has finished and a next episode exists; replace
    /// this row with a freshly-zeroed row at `(season, episode)`.
    AdvanceToNext { season: i64, episode: i64 },
    /// Series has been fully watched (last episode completed); remove
    /// every CW row for this title.
    RemoveSeries,
}

/// Apply the PRD §F-012 series next-episode rules to a CW row. The
/// caller persists the outcome by either upserting (`Keep`,
/// `AdvanceToNext`) or wiping (`RemoveSeries`) the title's rows.
///
/// `episodes` is the canonical series episode list (ignored for movies
/// — their decision is always [`ResumeDecision::Keep`]).
#[must_use]
pub fn resume_decision(cw: &ContinueWatching, episodes: &[(i64, i64)]) -> ResumeDecision {
    if !cw.is_completed() {
        return ResumeDecision::Keep;
    }
    match cw.kind {
        TitleKind::Movie => ResumeDecision::Keep,
        TitleKind::Series => match next_episode_after(cw.season, cw.episode, episodes) {
            Some((s, e)) => ResumeDecision::AdvanceToNext {
                season: s,
                episode: e,
            },
            None if episodes.is_empty() => {
                // Caller didn't supply an episode list (transient meta
                // fetch failure); keep the row at its completed state
                // rather than guessing.
                ResumeDecision::Keep
            }
            None => ResumeDecision::RemoveSeries,
        },
    }
}

/// PRD §F-012 "Completed items auto-removed from Continue Watching
/// after 24h". `now_unix` is the current Unix epoch in seconds; the
/// `CW_AUTOREMOVE_S` window comes from
/// [`crate::constants::CW_AUTOREMOVE_S`].
#[must_use]
pub fn should_auto_remove(cw: &ContinueWatching, now_unix: i64) -> bool {
    if !cw.is_completed() {
        return false;
    }
    let age = now_unix.saturating_sub(cw.last_played_at);
    let window = i64::try_from(crate::constants::CW_AUTOREMOVE_S).unwrap_or(i64::MAX);
    age >= window
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movie_at(position: f64, duration: f64) -> ContinueWatching {
        ContinueWatching {
            title_id: "tt0111161".into(),
            kind: TitleKind::Movie,
            season: 0,
            episode: 0,
            position_s: position,
            duration_s: duration,
            last_played_at: 0,
            meta_json: serde_json::json!({}),
        }
    }

    fn episode_at(season: i64, episode: i64, position: f64, duration: f64) -> ContinueWatching {
        ContinueWatching {
            title_id: "tt0944947".into(),
            kind: TitleKind::Series,
            season,
            episode,
            position_s: position,
            duration_s: duration,
            last_played_at: 0,
            meta_json: serde_json::json!({}),
        }
    }

    #[test]
    fn is_completed_uses_locked_threshold() {
        // Exactly at the threshold counts as completed.
        let at = movie_at(0.95 * 100.0, 100.0);
        assert!(at.is_completed());

        // Just under counts as in-progress.
        let under = movie_at(0.95 * 100.0 - 0.001, 100.0);
        assert!(!under.is_completed());

        // Zero-duration rows can never be completed (sentinel
        // protection: a stale meta with `duration_s = 0` must not
        // accidentally evict a CW row via the auto-removal sweep).
        let zero = movie_at(99.0, 0.0);
        assert!(!zero.is_completed());
    }

    #[test]
    fn next_episode_returns_in_season_when_present() {
        let eps = [(1, 1), (1, 2), (1, 3)];
        assert_eq!(next_episode_after(1, 1, &eps), Some((1, 2)));
        assert_eq!(next_episode_after(1, 2, &eps), Some((1, 3)));
        assert_eq!(next_episode_after(1, 3, &eps), None);
    }

    #[test]
    fn next_episode_crosses_season_boundary() {
        let eps = [(1, 1), (1, 2), (2, 1), (2, 2)];
        assert_eq!(next_episode_after(1, 2, &eps), Some((2, 1)));
        assert_eq!(next_episode_after(2, 2, &eps), None);
    }

    #[test]
    fn next_episode_handles_out_of_order_input() {
        // Stremio meta sometimes ships specials (season 0) interleaved
        // or in arbitrary order. The helper sorts internally so the
        // result is independent of caller order.
        let eps = [(2, 1), (1, 1), (1, 2)];
        assert_eq!(next_episode_after(1, 1, &eps), Some((1, 2)));
        assert_eq!(next_episode_after(1, 2, &eps), Some((2, 1)));
    }

    #[test]
    fn next_episode_returns_none_when_current_not_in_list() {
        let eps = [(1, 1), (1, 2)];
        assert_eq!(next_episode_after(7, 7, &eps), None);
    }

    #[test]
    fn next_episode_skips_season_zero_specials() {
        // PRD §F-010 / Cinemeta surfaces "specials" as season 0; the
        // next-episode rule must skip them so a `S01E10 → S00E1`
        // (recap) transition doesn't happen.
        let eps = [(0, 1), (1, 1), (1, 2)];
        assert_eq!(next_episode_after(1, 1, &eps), Some((1, 2)));
        assert_eq!(next_episode_after(1, 2, &eps), None);
        // Season 0 itself never participates as an origin either.
        assert_eq!(next_episode_after(0, 1, &eps), None);
    }

    #[test]
    fn resume_decision_movie_in_progress_keeps_row() {
        // Movie, < 95% watched → row stays at saved position.
        let cw = movie_at(60.0, 120.0);
        let outcome = resume_decision(&cw, &[]);
        assert_eq!(outcome, ResumeDecision::Keep);
    }

    #[test]
    fn resume_decision_movie_completed_keeps_row() {
        // Movie, ≥ 95% watched → row stays (auto-removed by the 24h
        // sweep). PRD §F-012 distinguishes movies from series here:
        // movies are kept as a "recently finished" CW entry until
        // they age out; series advance to the next episode.
        let cw = movie_at(120.0, 120.0);
        let outcome = resume_decision(&cw, &[]);
        assert_eq!(outcome, ResumeDecision::Keep);
    }

    #[test]
    fn resume_decision_series_episode_in_progress_keeps_row() {
        // Branch 1: current episode < 95% → row shows current episode
        // at saved position, label "Resume Sxx Eyy".
        let cw = episode_at(1, 1, 1200.0, 1800.0); // 67%
        let eps = [(1, 1), (1, 2)];
        let outcome = resume_decision(&cw, &eps);
        assert_eq!(outcome, ResumeDecision::Keep);
    }

    #[test]
    fn resume_decision_series_advances_to_next_episode() {
        // Branch 2: current ≥ 95% AND next exists → row should show
        // next episode at position 0, label "Up next: Sxx Eyy".
        let cw = episode_at(1, 1, 1800.0, 1800.0); // 100%
        let eps = [(1, 1), (1, 2)];
        let outcome = resume_decision(&cw, &eps);
        assert_eq!(
            outcome,
            ResumeDecision::AdvanceToNext {
                season: 1,
                episode: 2
            }
        );
    }

    #[test]
    fn resume_decision_series_removes_when_no_next_episode() {
        // Branch 3: current ≥ 95% AND no next → series removed from
        // Continue Watching.
        let cw = episode_at(2, 10, 1800.0, 1800.0);
        let eps = [(2, 9), (2, 10)];
        let outcome = resume_decision(&cw, &eps);
        assert_eq!(outcome, ResumeDecision::RemoveSeries);
    }

    #[test]
    fn resume_decision_series_with_empty_episode_list_keeps_completed_row() {
        // Edge case: the player invoked record_position without a
        // freshly-loaded episode list (e.g. transient meta-fetch
        // failure). We can't tell whether a next episode exists, so
        // we keep the row at its completed state — the 24h sweep will
        // age it out and the next title-detail open will recompute.
        let cw = episode_at(1, 1, 1800.0, 1800.0);
        let outcome = resume_decision(&cw, &[]);
        assert_eq!(outcome, ResumeDecision::Keep);
    }

    #[test]
    fn should_auto_remove_respects_window_and_completion() {
        // Completed row, well past the window → remove.
        let completed_old = movie_at(120.0, 120.0);
        let mut cw = completed_old;
        cw.last_played_at = 0;
        assert!(should_auto_remove(&cw, 100_000));

        // Completed row inside the window → keep (still useful as a
        // "recently watched" surface).
        cw.last_played_at = 99_000;
        assert!(!should_auto_remove(&cw, 100_000));

        // In-progress row, even very old → never auto-remove.
        let in_progress = movie_at(30.0, 120.0);
        let mut cw = in_progress;
        cw.last_played_at = 0;
        assert!(!should_auto_remove(&cw, 999_999_999));
    }

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
