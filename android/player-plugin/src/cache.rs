//! Cached snapshot + track-list state shared between the Android
//! driver and the event poll task.
//!
//! Lifted out of [`crate::mobile`] so the event-folding logic compiles
//! and tests on every workspace target (Linux CI runs the unit tests
//! here; the surrounding driver only compiles on `target_os =
//! "android"` because the Tauri mobile plugin API is Android-gated).

use std::sync::{Arc, Mutex};

use kino_player::{PlayerEvent, PlayerSnapshot, PlayerState, TrackList};

/// Tuple of latest snapshot + tracks the driver reports without round-
/// tripping back to Kotlin. Updated by [`fold_event`] each time the
/// poll task drains a batch of events.
#[derive(Debug, Clone)]
pub struct CachedState {
    pub snapshot: PlayerSnapshot,
    pub tracks: TrackList,
}

impl Default for CachedState {
    fn default() -> Self {
        Self {
            snapshot: PlayerSnapshot::idle(""),
            tracks: TrackList::default(),
        }
    }
}

/// Convenience alias for the shared cache pointer the driver stashes
/// in app state.
pub type SharedCache = Arc<Mutex<CachedState>>;

/// Build a fresh shared cache.
#[must_use]
pub fn shared() -> SharedCache {
    Arc::new(Mutex::new(CachedState::default()))
}

/// Fold a single [`PlayerEvent`] into the cached state. Called by the
/// poll task for every event drained from the Kotlin queue so the
/// next [`PlayerHandle::snapshot`](kino_player::PlayerHandle::snapshot)
/// / [`PlayerHandle::tracks`](kino_player::PlayerHandle::tracks) call
/// reflects the latest values without an extra plugin invoke.
///
/// # Panics
///
/// Panics if the cache mutex is poisoned. This only happens if the
/// surrounding driver previously panicked while holding the lock —
/// any panic in the player path is unrecoverable at the host level
/// (the bridge task aborts and the next `open()` rebuilds the
/// driver).
pub fn fold_event(cached: &SharedCache, event: &PlayerEvent) {
    let mut g = cached.lock().expect("kino-player cached state poisoned");
    match event {
        PlayerEvent::Position {
            position_s,
            duration_s,
            paused,
        } => {
            g.snapshot.position_s = *position_s;
            g.snapshot.duration_s = *duration_s;
            g.snapshot.paused = *paused;
        }
        PlayerEvent::State { state } => {
            g.snapshot.state = *state;
            // `Paused` state implies paused=true; same for the inverse
            // so the cached snapshot stays self-consistent for a
            // single-shot snapshot read.
            if matches!(state, PlayerState::Paused) {
                g.snapshot.paused = true;
            } else if matches!(state, PlayerState::Playing | PlayerState::Buffering) {
                g.snapshot.paused = false;
            }
        }
        PlayerEvent::Tracks { tracks } => {
            g.tracks = tracks.clone();
        }
        PlayerEvent::Exit {
            position_s,
            duration_s,
            ..
        } => {
            g.snapshot.state = PlayerState::Ended;
            g.snapshot.position_s = *position_s;
            g.snapshot.duration_s = *duration_s;
        }
        PlayerEvent::Error { .. } => {
            g.snapshot.state = PlayerState::Error;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kino_player::{AudioTrack, SubtitleTrack};

    #[test]
    fn position_event_folds_into_snapshot() {
        let cached = shared();
        fold_event(
            &cached,
            &PlayerEvent::Position {
                position_s: 12.5,
                duration_s: 100.0,
                paused: false,
            },
        );
        let g = cached.lock().unwrap();
        assert!((g.snapshot.position_s - 12.5).abs() < f64::EPSILON);
        assert!((g.snapshot.duration_s - 100.0).abs() < f64::EPSILON);
        assert!(!g.snapshot.paused);
    }

    #[test]
    fn paused_state_event_sets_paused_flag() {
        let cached = shared();
        fold_event(
            &cached,
            &PlayerEvent::State {
                state: PlayerState::Paused,
            },
        );
        let g = cached.lock().unwrap();
        assert_eq!(g.snapshot.state, PlayerState::Paused);
        assert!(g.snapshot.paused);
    }

    #[test]
    fn playing_state_event_clears_paused_flag() {
        let cached = Arc::new(Mutex::new(CachedState {
            snapshot: PlayerSnapshot {
                token: String::new(),
                state: PlayerState::Paused,
                position_s: 0.0,
                duration_s: 0.0,
                paused: true,
            },
            tracks: TrackList::default(),
        }));
        fold_event(
            &cached,
            &PlayerEvent::State {
                state: PlayerState::Playing,
            },
        );
        let g = cached.lock().unwrap();
        assert_eq!(g.snapshot.state, PlayerState::Playing);
        assert!(!g.snapshot.paused);
    }

    #[test]
    fn buffering_state_event_clears_paused_flag() {
        let cached = Arc::new(Mutex::new(CachedState {
            snapshot: PlayerSnapshot {
                token: String::new(),
                state: PlayerState::Paused,
                position_s: 0.0,
                duration_s: 0.0,
                paused: true,
            },
            tracks: TrackList::default(),
        }));
        fold_event(
            &cached,
            &PlayerEvent::State {
                state: PlayerState::Buffering,
            },
        );
        let g = cached.lock().unwrap();
        assert_eq!(g.snapshot.state, PlayerState::Buffering);
        assert!(!g.snapshot.paused);
    }

    #[test]
    fn loading_state_event_does_not_force_paused_flag() {
        // Loading → fresh open. Don't touch the paused flag (the
        // driver's `open()` already reset it).
        let cached = Arc::new(Mutex::new(CachedState {
            snapshot: PlayerSnapshot {
                token: String::new(),
                state: PlayerState::Idle,
                position_s: 0.0,
                duration_s: 0.0,
                paused: false,
            },
            tracks: TrackList::default(),
        }));
        fold_event(
            &cached,
            &PlayerEvent::State {
                state: PlayerState::Loading,
            },
        );
        let g = cached.lock().unwrap();
        assert_eq!(g.snapshot.state, PlayerState::Loading);
        assert!(!g.snapshot.paused);
    }

    #[test]
    fn tracks_event_replaces_cached_tracks() {
        let cached = shared();
        let tracks = TrackList {
            audio: vec![AudioTrack {
                id: 1,
                title: None,
                language: Some("eng".into()),
                codec: Some("truehd".into()),
                channels: Some(6),
                is_default: true,
                is_selected: true,
            }],
            subtitles: vec![SubtitleTrack {
                id: 2,
                title: None,
                language: Some("eng".into()),
                codec: Some("subrip".into()),
                is_default: false,
                is_forced: false,
                is_selected: false,
            }],
        };
        fold_event(&cached, &PlayerEvent::Tracks { tracks });
        let g = cached.lock().unwrap();
        assert_eq!(g.tracks.audio.len(), 1);
        assert_eq!(g.tracks.audio[0].id, 1);
        assert_eq!(g.tracks.subtitles.len(), 1);
        assert_eq!(g.tracks.subtitles[0].id, 2);
    }

    #[test]
    fn exit_event_moves_state_to_ended_and_records_final_position() {
        let cached = shared();
        fold_event(
            &cached,
            &PlayerEvent::Exit {
                position_s: 99.0,
                duration_s: 100.0,
                reached_eof: true,
            },
        );
        let g = cached.lock().unwrap();
        assert_eq!(g.snapshot.state, PlayerState::Ended);
        assert!((g.snapshot.position_s - 99.0).abs() < f64::EPSILON);
        assert!((g.snapshot.duration_s - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn error_event_moves_state_to_error() {
        let cached = shared();
        fold_event(
            &cached,
            &PlayerEvent::Error {
                message: "boom".into(),
            },
        );
        let g = cached.lock().unwrap();
        assert_eq!(g.snapshot.state, PlayerState::Error);
    }

    #[test]
    fn default_cached_state_is_idle() {
        let s = CachedState::default();
        assert_eq!(s.snapshot.state, PlayerState::Idle);
        assert!(s.snapshot.token.is_empty());
        assert!((s.snapshot.position_s - 0.0).abs() < f64::EPSILON);
        assert!(s.tracks.audio.is_empty());
        assert!(s.tracks.subtitles.is_empty());
    }
}
