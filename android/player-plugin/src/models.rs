//! Wire DTOs shared between the Rust driver and the Kotlin
//! `PlayerPlugin` (PRD §F-015 Android side).
//!
//! Every type here is `serde(rename_all = "camelCase")` so the JSON the
//! Tauri mobile-plugin invoke layer carries from Rust to Kotlin uses
//! the same field names the Kotlin side reads via
//! `JSObject.getString(...)` / `getDouble(...)` / etc. The Kotlin
//! `@InvokeArg` data classes mirror these shapes verbatim — change a
//! field name here, change it on the Kotlin side too.

use serde::{Deserialize, Serialize};

use kino_player::{OpenRequest, PlayerEvent, PlayerSnapshot, TrackList};

/// Request payload for the Kotlin-side `open` command.
///
/// Mirrors [`kino_player::OpenRequest`] but lives in this crate so we
/// can attach serde attributes without touching the shared trait
/// crate. A `From<OpenRequest>` impl keeps the conversion noise-free.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenArgs {
    pub token: String,
    pub url: String,
    pub resume_position_s: f64,
    pub file_name: Option<String>,
    pub duration_hint_s: Option<f64>,
}

impl From<OpenRequest> for OpenArgs {
    fn from(req: OpenRequest) -> Self {
        Self {
            token: req.token,
            url: req.url,
            resume_position_s: req.resume_position_s,
            file_name: req.file_name,
            duration_hint_s: req.duration_hint_s,
        }
    }
}

/// `set_paused(paused)` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPausedArgs {
    pub paused: bool,
}

/// `seek(position_s)` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeekArgs {
    pub position_s: f64,
}

/// `select_audio_track(track_id?)` / `select_subtitle_track(track_id?)`
/// payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectTrackArgs {
    /// `None` disables the track entirely (mpv: `aid=no` / `sid=no`;
    /// `ExoPlayer`: clear the corresponding track selection override).
    pub track_id: Option<i64>,
}

/// Empty-payload command marker used by `close` / `snapshot` /
/// `tracks` / `drain_events` / `ping`. Tauri's mobile plugin invoke
/// API expects a serializable payload even when there's nothing to
/// send.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoArgs;

/// Response payload for the Kotlin-side `snapshot` command.
///
/// Mirrors [`kino_player::PlayerSnapshot`] verbatim — defined here so
/// the JSON serde derives stay co-located with the other wire types
/// (the source-of-truth `PlayerSnapshot` already derives the same
/// camelCase rename).
pub type SnapshotResponse = PlayerSnapshot;

/// Response payload for the Kotlin-side `tracks` command.
pub type TracksResponse = TrackList;

/// Response payload for the Kotlin-side `drain_events` command.
///
/// The Android plugin can't push events into Rust synchronously
/// (Tauri's mobile-plugin layer is request/response only), so the
/// Kotlin side queues each `PlayerEvent` and the driver polls them via
/// `drain_events`. Bounded queue: PRD §8 `PLAYER_POSITION_INTERVAL_S =
/// 5s` keeps the steady-state event rate low; the Kotlin side caps the
/// queue at 256 entries and drops oldest-first if the Rust poller is
/// stalled.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainEventsResponse {
    /// Events in arrival order.
    pub events: Vec<PlayerEvent>,
    /// `true` if the Kotlin-side queue overflowed since the last drain
    /// and one or more events were dropped. The Rust driver logs a
    /// warning when this is set so debugging can correlate dropped
    /// state with an over-burdened bridge.
    #[serde(default)]
    pub overflowed: bool,
}

/// Response payload for `ping` — used by tests to verify the round
/// trip is functional without side-effecting the player.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingResponse {
    pub pong: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_args_round_trips_via_open_request() {
        let req = OpenRequest {
            token: "tok".into(),
            url: "http://127.0.0.1:42/stream/x".into(),
            resume_position_s: 12.5,
            file_name: Some("matrix.mkv".into()),
            duration_hint_s: Some(8197.0),
        };
        let args: OpenArgs = req.clone().into();
        assert_eq!(args.token, req.token);
        assert_eq!(args.url, req.url);
        assert!((args.resume_position_s - 12.5).abs() < f64::EPSILON);
        assert_eq!(args.file_name.as_deref(), Some("matrix.mkv"));
        assert_eq!(args.duration_hint_s, Some(8197.0));
    }

    #[test]
    fn open_args_serializes_in_camel_case() {
        let args = OpenArgs::from(OpenRequest {
            token: "t".into(),
            url: "u".into(),
            resume_position_s: 0.0,
            file_name: None,
            duration_hint_s: None,
        });
        let v = serde_json::to_value(&args).unwrap();
        assert_eq!(v["token"], "t");
        assert_eq!(v["url"], "u");
        assert!(v.get("resumePositionS").is_some());
        assert!(v.get("fileName").is_some());
        assert!(v.get("durationHintS").is_some());
    }

    #[test]
    fn set_paused_args_round_trip() {
        let args = SetPausedArgs { paused: true };
        let v = serde_json::to_value(&args).unwrap();
        assert_eq!(v["paused"], true);
        let back: SetPausedArgs = serde_json::from_value(v).unwrap();
        assert!(back.paused);
    }

    #[test]
    fn seek_args_round_trip() {
        let args = SeekArgs { position_s: 12.0 };
        let v = serde_json::to_value(&args).unwrap();
        assert!((v["positionS"].as_f64().unwrap() - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn select_track_args_round_trip_with_some_and_none() {
        let some = SelectTrackArgs { track_id: Some(7) };
        let v = serde_json::to_value(&some).unwrap();
        assert_eq!(v["trackId"], 7);

        let none = SelectTrackArgs { track_id: None };
        let v = serde_json::to_value(&none).unwrap();
        // serde renders `Option::None` as JSON `null` (not omitted) by
        // default; the Kotlin side treats both `null` and "absent" as
        // "disable track".
        assert!(v["trackId"].is_null());
    }

    #[test]
    fn drain_events_response_defaults_are_empty_and_not_overflowed() {
        let resp = DrainEventsResponse::default();
        assert!(resp.events.is_empty());
        assert!(!resp.overflowed);
    }

    #[test]
    fn drain_events_decodes_unknown_overflow_flag_as_false() {
        let v = serde_json::json!({ "events": [] });
        let resp: DrainEventsResponse = serde_json::from_value(v).unwrap();
        assert!(!resp.overflowed);
    }
}
