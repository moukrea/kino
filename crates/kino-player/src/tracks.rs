//! Audio + subtitle track descriptors (PRD §F-015 `ExoPlayer` / mpv
//! "track-list").
//!
//! Surfaced by [`PlayerHandle::tracks`](crate::handle::PlayerHandle::tracks)
//! and by the [`PlayerEvent::Tracks`](crate::event::PlayerEvent::Tracks)
//! event so the `SolidJS` overlay can render the audio / subtitle pickers
//! the PRD requires.

use serde::{Deserialize, Serialize};

/// One audio track exposed by the player. `id` is the backend-specific
/// identifier the host passes back to
/// [`PlayerHandle::select_audio_track`](crate::handle::PlayerHandle::select_audio_track).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTrack {
    pub id: i64,
    pub title: Option<String>,
    pub language: Option<String>,
    pub codec: Option<String>,
    pub channels: Option<u8>,
    pub is_default: bool,
    pub is_selected: bool,
}

/// One subtitle track exposed by the player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleTrack {
    pub id: i64,
    pub title: Option<String>,
    pub language: Option<String>,
    pub codec: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_selected: bool,
}

/// Aggregate track listing the player reports. Empty vectors are valid
/// (e.g. a video-only stream with no audio or no subtitles).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackList {
    pub audio: Vec<AudioTrack>,
    pub subtitles: Vec<SubtitleTrack>,
}

impl TrackList {
    /// Build a track list from raw mpv `track-list` entries. mpv's
    /// representation uses `type` strings (`audio` / `sub` / `video`)
    /// plus a backend `id`. This helper exists in the shared module so
    /// the mpv driver and its tests can share the same translation.
    #[must_use]
    pub fn from_mpv_tracks(raw: &serde_json::Value) -> Self {
        let mut out = Self::default();
        let Some(arr) = raw.as_array() else {
            return out;
        };
        for t in arr {
            let id = t
                .get("id")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(-1);
            let kind = t
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let title = t
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let language = t
                .get("lang")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let codec = t
                .get("codec")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let is_default = t
                .get("default")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let is_selected = t
                .get("selected")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            match kind {
                "audio" => {
                    let channels = t
                        .get("demux-channel-count")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|n| u8::try_from(n).ok());
                    out.audio.push(AudioTrack {
                        id,
                        title,
                        language,
                        codec,
                        channels,
                        is_default,
                        is_selected,
                    });
                }
                "sub" => {
                    let is_forced = t
                        .get("forced")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    out.subtitles.push(SubtitleTrack {
                        id,
                        title,
                        language,
                        codec,
                        is_default,
                        is_forced,
                        is_selected,
                    });
                }
                _ => {} // video / unknown — ignored
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_audio_and_subtitle_tracks_from_mpv_payload() {
        let raw = json!([
            {
                "id": 1,
                "type": "video",
                "codec": "hevc",
            },
            {
                "id": 2,
                "type": "audio",
                "title": "English 5.1",
                "lang": "eng",
                "codec": "truehd",
                "demux-channel-count": 6,
                "default": true,
                "selected": true,
            },
            {
                "id": 3,
                "type": "audio",
                "lang": "fra",
                "codec": "eac3",
                "demux-channel-count": 2,
                "selected": false,
            },
            {
                "id": 4,
                "type": "sub",
                "title": "English",
                "lang": "eng",
                "codec": "subrip",
                "forced": false,
                "default": true,
                "selected": true,
            },
            {
                "id": 5,
                "type": "sub",
                "lang": "eng",
                "codec": "ass",
                "forced": true,
            },
        ]);

        let parsed = TrackList::from_mpv_tracks(&raw);

        assert_eq!(parsed.audio.len(), 2);
        assert_eq!(parsed.audio[0].id, 2);
        assert_eq!(parsed.audio[0].language.as_deref(), Some("eng"));
        assert_eq!(parsed.audio[0].channels, Some(6));
        assert!(parsed.audio[0].is_default);
        assert!(parsed.audio[0].is_selected);
        assert_eq!(parsed.audio[1].id, 3);
        assert_eq!(parsed.audio[1].channels, Some(2));
        assert!(!parsed.audio[1].is_selected);

        assert_eq!(parsed.subtitles.len(), 2);
        assert_eq!(parsed.subtitles[0].id, 4);
        assert!(parsed.subtitles[0].is_default);
        assert!(parsed.subtitles[0].is_selected);
        assert!(!parsed.subtitles[0].is_forced);
        assert_eq!(parsed.subtitles[1].id, 5);
        assert!(parsed.subtitles[1].is_forced);
        assert!(!parsed.subtitles[1].is_default);
    }

    #[test]
    fn empty_track_list_for_non_array() {
        let raw = serde_json::json!({"not": "an array"});
        let parsed = TrackList::from_mpv_tracks(&raw);
        assert!(parsed.audio.is_empty());
        assert!(parsed.subtitles.is_empty());
    }

    #[test]
    fn ignores_unknown_track_types() {
        let raw = serde_json::json!([
            {"id": 1, "type": "video", "codec": "hevc"},
            {"id": 2, "type": "metadata", "codec": "id3"},
        ]);
        let parsed = TrackList::from_mpv_tracks(&raw);
        assert!(parsed.audio.is_empty());
        assert!(parsed.subtitles.is_empty());
    }
}
