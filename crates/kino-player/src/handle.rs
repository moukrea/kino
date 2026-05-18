//! Driver-agnostic player handle (PRD §F-015).
//!
//! Every backend (Linux mpv subprocess; future libmpv-in-process; Android
//! `ExoPlayer` via Tauri plugin) implements [`PlayerHandle`] so the Tauri
//! command layer doesn't branch on platform.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::PlayerError;
use crate::event::PlayerEvent;
use crate::state::PlayerSnapshot;
use crate::tracks::TrackList;

/// Open request — what the host needs to hand to the driver to start a
/// session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    /// Stable token for the session. The host's `start_playback` token
    /// is the natural choice for torrent-backed playback; direct-URL
    /// playback uses a host-minted UUID.
    pub token: String,
    /// The URL the player consumes. For torrent-backed playback this is
    /// `http://127.0.0.1:PORT/stream/<token>` (PRD §3 playback data
    /// flow step 5). For direct streams it's the addon-supplied URL.
    pub url: String,
    /// Resume position in seconds. The driver seeks to this offset
    /// after open. `0.0` for fresh playback.
    pub resume_position_s: f64,
    /// Optional friendly file name used by the player's window title /
    /// info panel.
    pub file_name: Option<String>,
    /// Optional duration hint in seconds. The player still detects the
    /// real duration from the demuxer; this is purely for first-paint
    /// UI sizing.
    pub duration_hint_s: Option<f64>,
}

/// Async control surface every backend implements.
///
/// All methods are `&self` because the underlying drivers all use
/// internal channels / `Arc`s — keeping the trait `&self` means the
/// Tauri host can stash a single `Arc<dyn PlayerHandle>` and call into
/// it from any command without acquiring an outer lock.
#[async_trait]
pub trait PlayerHandle: Send + Sync {
    /// Snapshot of the current state. Cheap; used by `player_status`.
    fn snapshot(&self) -> PlayerSnapshot;

    /// Subscribe to the event stream. Subsequent calls return a fresh
    /// receiver; the channel is multi-consumer (broadcast).
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<PlayerEvent>;

    /// Open a new session. If a session is already in flight the driver
    /// MUST replace it (effectively `close` + `open` atomically) so the
    /// caller doesn't have to choreograph the transition.
    async fn open(&self, req: OpenRequest) -> Result<(), PlayerError>;

    /// Close the active session. Emits a terminal
    /// [`PlayerEvent::Exit`] with the final position.
    async fn close(&self) -> Result<(), PlayerError>;

    /// Toggle pause state. `paused == true` pauses, `false` resumes.
    async fn set_paused(&self, paused: bool) -> Result<(), PlayerError>;

    /// Seek to an absolute position in seconds.
    async fn seek(&self, position_s: f64) -> Result<(), PlayerError>;

    /// Select an audio track by backend id. `None` disables audio.
    async fn select_audio_track(&self, track_id: Option<i64>) -> Result<(), PlayerError>;

    /// Select a subtitle track by backend id. `None` disables subtitles.
    async fn select_subtitle_track(&self, track_id: Option<i64>) -> Result<(), PlayerError>;

    /// Latest track listing the driver knows about. Empty before the
    /// first `Tracks` event fires.
    fn tracks(&self) -> TrackList;
}
