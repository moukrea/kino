//! Desktop / non-Android stub driver.
//!
//! Provides a [`PlayerHandle`] implementation that compiles on every
//! workspace target so the `cargo check` matrix stays green. On Linux
//! the host wires the real player through
//! [`kino_player::MpvPlayer`]; this stub exists purely so the same
//! `tauri-plugin-kino-player` crate can register on every Tauri host
//! without `#[cfg]` gating at the call site.
//!
//! Every method returns a clearly-attributed error so a misconfigured
//! host immediately sees "we used the Android driver on a desktop
//! target" in the logs rather than mysterious silent failures.

use async_trait::async_trait;

use kino_player::{
    AudioTrack, OpenRequest, PlayerError, PlayerEvent, PlayerHandle, PlayerSnapshot, PlayerState,
    SubtitleTrack, TrackList,
};
use tokio::sync::broadcast;

/// No-op [`PlayerHandle`] that always errors. Carries an event channel
/// so [`PlayerHandle::subscribe`] still returns a valid receiver (the
/// channel never produces anything).
pub struct StubPlayer {
    events: broadcast::Sender<PlayerEvent>,
}

impl StubPlayer {
    /// Build a fresh stub driver.
    #[must_use]
    pub fn new() -> Self {
        let (events, _rx) = broadcast::channel(16);
        Self { events }
    }
}

impl Default for StubPlayer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PlayerHandle for StubPlayer {
    fn snapshot(&self) -> PlayerSnapshot {
        PlayerSnapshot {
            token: String::new(),
            state: PlayerState::Error,
            position_s: 0.0,
            duration_s: 0.0,
            paused: false,
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }

    async fn open(&self, _req: OpenRequest) -> Result<(), PlayerError> {
        Err(unsupported())
    }

    async fn close(&self) -> Result<(), PlayerError> {
        Err(unsupported())
    }

    async fn set_paused(&self, _paused: bool) -> Result<(), PlayerError> {
        Err(unsupported())
    }

    async fn seek(&self, _position_s: f64) -> Result<(), PlayerError> {
        Err(unsupported())
    }

    async fn select_audio_track(&self, _track_id: Option<i64>) -> Result<(), PlayerError> {
        Err(unsupported())
    }

    async fn select_subtitle_track(&self, _track_id: Option<i64>) -> Result<(), PlayerError> {
        Err(unsupported())
    }

    fn tracks(&self) -> TrackList {
        TrackList {
            audio: Vec::<AudioTrack>::new(),
            subtitles: Vec::<SubtitleTrack>::new(),
        }
    }
}

fn unsupported() -> PlayerError {
    PlayerError::Spawn(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "kino-player Android driver invoked on a non-Android target — \
         use kino_player::MpvPlayer on Linux",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_errors_on_desktop() {
        let p = StubPlayer::new();
        let err = p
            .open(OpenRequest {
                token: "t".into(),
                url: "u".into(),
                resume_position_s: 0.0,
                file_name: None,
                duration_hint_s: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PlayerError::Spawn(_)));
    }

    #[tokio::test]
    async fn every_command_errors_on_desktop() {
        let p = StubPlayer::new();
        assert!(p.close().await.is_err());
        assert!(p.set_paused(true).await.is_err());
        assert!(p.seek(10.0).await.is_err());
        assert!(p.select_audio_track(None).await.is_err());
        assert!(p.select_subtitle_track(Some(2)).await.is_err());
    }

    #[test]
    fn snapshot_reports_error_state() {
        let p = StubPlayer::new();
        let snap = p.snapshot();
        assert_eq!(snap.state, PlayerState::Error);
        assert!(snap.token.is_empty());
    }

    #[test]
    fn tracks_are_empty() {
        let p = StubPlayer::new();
        let t = p.tracks();
        assert!(t.audio.is_empty());
        assert!(t.subtitles.is_empty());
    }

    #[tokio::test]
    async fn subscribe_yields_a_live_receiver() {
        let p = StubPlayer::new();
        let mut rx = p.subscribe();
        // The stub never emits anything; `try_recv` should report
        // "empty" rather than "closed".
        let res = rx.try_recv();
        assert!(matches!(
            res,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}
