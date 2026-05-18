//! Player-event vocabulary forwarded to the Tauri host (PRD §F-015).
//!
//! Wire-tagged via `serde(tag = "kind", rename_all = "camelCase")` so the
//! frontend sees objects like
//! `{ "kind": "position", "positionS": 12.34, "durationS": 5800.0 }`.
//! Each variant is a struct so the on-the-wire shape stays a flat
//! camelCase object — newtype variants with `tag` would smuggle the
//! variant's inner value as a wildcard field, which is awkward to
//! consume from TypeScript.

use serde::{Deserialize, Serialize};

use crate::state::PlayerState;
use crate::tracks::TrackList;

/// Position update emitted on the PRD §8
/// `PLAYER_POSITION_INTERVAL_S = 5 s` cadence (the player may emit more
/// frequently on seeks or state transitions; consumers MUST be
/// idempotent).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionTick {
    pub position_s: f64,
    pub duration_s: f64,
    /// `true` while the user / driver has explicitly paused playback;
    /// distinct from [`PlayerState::Buffering`].
    pub paused: bool,
}

/// Everything the driver wants the host to know about. Position + state
/// + track-list updates plus terminal Error / Exit transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlayerEvent {
    /// Periodic playhead update. Drives F-012 Continue Watching writes
    /// AND F-014 buffer-monitor recomputes.
    Position {
        position_s: f64,
        duration_s: f64,
        paused: bool,
    },
    /// State transition. Tauri event `player:state`.
    State { state: PlayerState },
    /// New / refreshed track listing. Tauri event `player:tracks`.
    Tracks { tracks: TrackList },
    /// Terminal: the user closed the player or the media ended cleanly.
    /// Carries the final position so the host can persist it before
    /// dropping the session. Tauri event `player:exit`.
    Exit {
        position_s: f64,
        duration_s: f64,
        /// `true` when the player reached end-of-file naturally rather
        /// than being closed by the user. The host uses this to decide
        /// the F-012 next-episode advance.
        reached_eof: bool,
    },
    /// Terminal: backend reported an error. The driver is no longer
    /// usable; the host should tear it down. Tauri event `player:error`.
    Error { message: String },
}

impl PlayerEvent {
    /// Convenience constructor: position event from a [`PositionTick`].
    #[must_use]
    pub fn position(tick: PositionTick) -> Self {
        Self::Position {
            position_s: tick.position_s,
            duration_s: tick.duration_s,
            paused: tick.paused,
        }
    }

    /// Convenience constructor: state event.
    #[must_use]
    pub fn state(state: PlayerState) -> Self {
        Self::State { state }
    }

    /// Convenience constructor: tracks event.
    #[must_use]
    pub fn tracks(tracks: TrackList) -> Self {
        Self::Tracks { tracks }
    }
}

impl PlayerEvent {
    /// `true` if this event marks the end of the player session — the
    /// host must release any per-session resources after it sees one.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Exit { .. } | Self::Error { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn position_event_serializes_with_camel_case_kind() {
        let e = PlayerEvent::position(PositionTick {
            position_s: 12.5,
            duration_s: 100.0,
            paused: false,
        });
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "position");
        assert!((v["positionS"].as_f64().unwrap() - 12.5).abs() < f64::EPSILON);
        assert!((v["durationS"].as_f64().unwrap() - 100.0).abs() < f64::EPSILON);
        assert_eq!(v["paused"], false);
    }

    #[test]
    fn state_event_uses_camel_case_state_string() {
        let e = PlayerEvent::state(PlayerState::Buffering);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v, json!({"kind": "state", "state": "buffering"}));
    }

    #[test]
    fn exit_event_round_trips() {
        let e = PlayerEvent::Exit {
            position_s: 42.0,
            duration_s: 100.0,
            reached_eof: true,
        };
        let v = serde_json::to_value(&e).unwrap();
        let back: PlayerEvent = serde_json::from_value(v).unwrap();
        match back {
            PlayerEvent::Exit {
                position_s,
                reached_eof,
                ..
            } => {
                assert!((position_s - 42.0).abs() < f64::EPSILON);
                assert!(reached_eof);
            }
            other => panic!("expected Exit, got {other:?}"),
        }
    }

    #[test]
    fn terminal_predicate() {
        assert!(PlayerEvent::Exit {
            position_s: 0.0,
            duration_s: 0.0,
            reached_eof: false
        }
        .is_terminal());
        assert!(PlayerEvent::Error {
            message: "boom".into()
        }
        .is_terminal());
        assert!(!PlayerEvent::state(PlayerState::Playing).is_terminal());
        assert!(!PlayerEvent::position(PositionTick {
            position_s: 0.0,
            duration_s: 0.0,
            paused: false
        })
        .is_terminal());
    }
}
