//! Player state model (PRD §F-015 lifecycle).
//!
//! The state diagram is small by design — every backend (mpv, `ExoPlayer`,
//! a future libmpv-in-process driver) must agree on the same vocabulary
//! so the `SolidJS` overlay can render `state === "buffering"`,
//! `state === "playing"`, etc. without branching on the backend.

use serde::{Deserialize, Serialize};

/// Possible playback states.
///
/// Transitions (rough):
///
/// ```text
///   Idle ──open──► Loading ──ready──► Playing ◄──► Paused
///                     │                  │
///                     │                  ├──buffer empty──► Buffering
///                     │                  │                     │
///                     │                  └──end-of-file───► Ended
///                     │
///                     └──open fails──► Error
/// ```
///
/// `Error` and `Ended` are terminal for a given media session. A
/// subsequent `open` resets back to `Loading`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlayerState {
    /// Driver is up but nothing is loaded.
    Idle,
    /// `open` has been issued; waiting for the demuxer and the first
    /// frame.
    Loading,
    /// Playing back, decoder + clock advancing.
    Playing,
    /// User pressed pause (or the host suspended via
    /// [`PlayerHandle::set_paused`](crate::handle::PlayerHandle::set_paused)).
    Paused,
    /// Demuxer underrun: the player is waiting for more bytes from the
    /// torrent / HTTP source. Distinct from `Loading` because the
    /// pipeline is already up.
    Buffering,
    /// End-of-file reached, the player is keeping the last frame.
    Ended,
    /// Unrecoverable backend error; the message lives on the carrying
    /// [`PlayerEvent::Error`](crate::event::PlayerEvent::Error) event.
    Error,
}

impl PlayerState {
    /// `true` when the player owns a loaded media session — the F-012
    /// position-saver and F-014 buffer monitor should only run in these
    /// states.
    #[must_use]
    pub fn has_media(self) -> bool {
        matches!(
            self,
            Self::Loading | Self::Playing | Self::Paused | Self::Buffering | Self::Ended
        )
    }
}

/// Snapshot returned by [`PlayerHandle::snapshot`](crate::handle::PlayerHandle::snapshot).
///
/// Used by the host's `player_status` Tauri command so the frontend's
/// first paint matches the live state without waiting for the next
/// `player:state` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSnapshot {
    /// Stable token the playback was opened with (the same one
    /// `start_playback` returned, or a host-generated id for direct
    /// URLs).
    pub token: String,
    /// Current state.
    pub state: PlayerState,
    /// Most recent playhead position in seconds.
    pub position_s: f64,
    /// Detected media duration in seconds, or `0.0` while the demuxer
    /// is still probing.
    pub duration_s: f64,
    /// `true` while the user / driver has explicitly paused playback;
    /// distinct from [`PlayerState::Buffering`].
    pub paused: bool,
}

impl PlayerSnapshot {
    /// Build the initial snapshot from a token, with everything at the
    /// `Idle` defaults.
    #[must_use]
    pub fn idle(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            state: PlayerState::Idle,
            position_s: 0.0,
            duration_s: 0.0,
            paused: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_media_covers_active_states() {
        for s in [
            PlayerState::Loading,
            PlayerState::Playing,
            PlayerState::Paused,
            PlayerState::Buffering,
            PlayerState::Ended,
        ] {
            assert!(s.has_media(), "{s:?} should have_media");
        }
        for s in [PlayerState::Idle, PlayerState::Error] {
            assert!(!s.has_media(), "{s:?} should NOT have_media");
        }
    }

    #[test]
    fn snapshot_idle_defaults() {
        let s = PlayerSnapshot::idle("abc");
        assert_eq!(s.token, "abc");
        assert_eq!(s.state, PlayerState::Idle);
        assert!((s.position_s - 0.0).abs() < f64::EPSILON);
        assert!((s.duration_s - 0.0).abs() < f64::EPSILON);
        assert!(!s.paused);
    }
}
