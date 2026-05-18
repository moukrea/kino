//! Android `PlayerHandle` driver (PRD §F-015, ADR-010).
//!
//! Bridges the [`kino_player::PlayerHandle`] trait to the companion
//! Kotlin `PlayerPlugin` via Tauri 2's mobile-plugin invoke layer. The
//! Rust side calls forward to Kotlin (`open` / `close` / `seek` /
//! ...); a dedicated tokio task polls the Kotlin event queue and
//! rebroadcasts each [`PlayerEvent`] over a `tokio::sync::broadcast`
//! channel so the host's bridge task sees the same vocabulary as the
//! Linux mpv driver.
//!
//! ## Event flow
//!
//! ```text
//! ExoPlayer ──► PlayerActivity ──► PlayerPlugin queue
//!                                       │
//!         poll-drain @ 250ms cadence    ▼
//!                                drain_events()
//!                                       │
//!                                       ▼
//!                          broadcast::Sender<PlayerEvent>
//!                                       │
//!                                       ▼
//!                                kino-app bridge task
//!                                       │
//!                       Tauri events + CW + buffer monitor
//! ```
//!
//! The 250ms poll cadence is chosen so a PRD §8
//! `PLAYER_POSITION_INTERVAL_S = 5s` position tick reaches the host
//! within at most one extra poll interval (worst-case lag ≈ 250ms
//! over the wire). State / error / exit events are surfaced on the
//! same cadence; this is fine — F-014's reflex actions (pause player
//! on REBUFFER) live on the order of seconds, not milliseconds.

use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tauri::plugin::{PluginApi, PluginHandle};
use tauri::Runtime;
use tokio::sync::broadcast;

use kino_player::{
    OpenRequest, PlayerError, PlayerEvent, PlayerHandle, PlayerSnapshot, PlayerState, TrackList,
};

use crate::cache::{self, SharedCache};
use crate::error::PluginError;
use crate::models::{
    DrainEventsResponse, NoArgs, OpenArgs, SeekArgs, SelectTrackArgs, SetPausedArgs,
};
use crate::ANDROID_PLUGIN_IDENTIFIER;

/// Bounded event channel capacity. Matches the Kotlin-side queue cap
/// (256) so the broadcast channel can soak one full Kotlin drain even
/// when the host bridge task is briefly stalled.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// How often the Rust poller drains the Kotlin event queue. 250ms
/// keeps wire lag well under PRD §8's 5s position cadence; lighter
/// cadences would risk burying a state transition behind a position
/// tick.
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Android driver. Holds the mobile plugin handle for command
/// dispatch, a broadcast channel for rebroadcasting events from the
/// poll task, and a cached snapshot/track list updated as events
/// arrive (so [`PlayerHandle::snapshot`] / [`PlayerHandle::tracks`]
/// stay cheap).
pub struct AndroidPlayer<R: Runtime> {
    handle: PluginHandle<R>,
    events: broadcast::Sender<PlayerEvent>,
    cached: SharedCache,
    poll_task: tokio::task::JoinHandle<()>,
}

impl<R: Runtime> AndroidPlayer<R> {
    /// Register the plugin with the Tauri mobile runtime and start the
    /// event poll task. Called from the plugin `setup` callback.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Invoke`] if the Tauri runtime fails to
    /// load the Kotlin `PlayerPlugin` class (almost always means the
    /// `dev.kino.player.PlayerPlugin` class is missing from the APK,
    /// i.e. the `android/player-plugin/android/` gradle module did not
    /// get included by the Tauri CLI).
    pub fn register<C: DeserializeOwned>(
        app: &tauri::AppHandle<R>,
        api: &PluginApi<R, C>,
    ) -> Result<Self, PluginError> {
        let handle = api
            .register_android_plugin(ANDROID_PLUGIN_IDENTIFIER, "PlayerPlugin")
            .map_err(|e| PluginError::Invoke(e.to_string()))?;
        Self::from_handle(app, handle)
    }

    /// Build a driver around an existing `PluginHandle`. Split out so
    /// integration tests can inject a hand-rolled handle.
    pub(crate) fn from_handle(
        _app: &tauri::AppHandle<R>,
        handle: PluginHandle<R>,
    ) -> Result<Self, PluginError> {
        let (events_tx, _events_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let cached = cache::shared();
        let poll_task = spawn_event_poll(handle.clone(), events_tx.clone(), cached.clone());
        Ok(Self {
            handle,
            events: events_tx,
            cached,
            poll_task,
        })
    }

    fn run<T, A>(&self, command: &str, args: A) -> Result<T, PlayerError>
    where
        T: DeserializeOwned,
        A: Serialize,
    {
        self.handle
            .run_mobile_plugin::<T>(command, args)
            .map_err(|e| PluginError::Invoke(e.to_string()).into())
    }
}

impl<R: Runtime> Drop for AndroidPlayer<R> {
    fn drop(&mut self) {
        self.poll_task.abort();
    }
}

#[async_trait]
impl<R: Runtime> PlayerHandle for AndroidPlayer<R> {
    fn snapshot(&self) -> PlayerSnapshot {
        self.cached
            .lock()
            .expect("kino-player cached state poisoned")
            .snapshot
            .clone()
    }

    fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }

    async fn open(&self, req: OpenRequest) -> Result<(), PlayerError> {
        let args: OpenArgs = req.into();
        // The Kotlin side returns NoArgs on success. The native plugin
        // resolves the invoke from the activity main thread once
        // `ExoPlayer.prepare()` has been issued, so `await`-ing here
        // is bounded by ExoPlayer's setup time (typically <100ms).
        let _: NoArgs = self.run("open", args)?;
        // Seed the cached snapshot so a `snapshot()` call between
        // `open()` and the first poll-drain reflects the new session.
        let mut g = self
            .cached
            .lock()
            .expect("kino-player cached state poisoned");
        g.snapshot.token = String::new();
        g.snapshot.state = PlayerState::Loading;
        g.snapshot.position_s = 0.0;
        g.snapshot.duration_s = 0.0;
        g.snapshot.paused = false;
        g.tracks = TrackList::default();
        Ok(())
    }

    async fn close(&self) -> Result<(), PlayerError> {
        let _: NoArgs = self.run("close", NoArgs)?;
        Ok(())
    }

    async fn set_paused(&self, paused: bool) -> Result<(), PlayerError> {
        let _: NoArgs = self.run("set_paused", SetPausedArgs { paused })?;
        Ok(())
    }

    async fn seek(&self, position_s: f64) -> Result<(), PlayerError> {
        let _: NoArgs = self.run("seek", SeekArgs { position_s })?;
        Ok(())
    }

    async fn select_audio_track(&self, track_id: Option<i64>) -> Result<(), PlayerError> {
        let _: NoArgs = self.run("select_audio_track", SelectTrackArgs { track_id })?;
        Ok(())
    }

    async fn select_subtitle_track(&self, track_id: Option<i64>) -> Result<(), PlayerError> {
        let _: NoArgs = self.run("select_subtitle_track", SelectTrackArgs { track_id })?;
        Ok(())
    }

    fn tracks(&self) -> TrackList {
        self.cached
            .lock()
            .expect("kino-player cached state poisoned")
            .tracks
            .clone()
    }
}

/// Spawn the event-drain poll task. Polls every
/// [`EVENT_POLL_INTERVAL`]; on success, rebroadcasts each event AND
/// folds it into the cached snapshot / tracks so the next
/// [`PlayerHandle::snapshot`] / [`PlayerHandle::tracks`] call reflects
/// the latest state without an extra plugin invoke.
fn spawn_event_poll<R: Runtime>(
    handle: PluginHandle<R>,
    events: broadcast::Sender<PlayerEvent>,
    cached: SharedCache,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(EVENT_POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            match handle.run_mobile_plugin::<DrainEventsResponse>("drain_events", NoArgs) {
                Ok(resp) => {
                    if resp.overflowed {
                        tracing::warn!(
                            "kino-player Android event queue overflowed — events dropped"
                        );
                    }
                    for event in resp.events {
                        cache::fold_event(&cached, &event);
                        // `broadcast::send` errors only when there are
                        // zero receivers; drop silently — the next
                        // `subscribe()` call will start fresh.
                        let _ = events.send(event);
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        "kino-player Android drain_events failed; retrying"
                    );
                    // Don't fast-loop on errors; the ticker's 250ms
                    // interval is already a sane retry cadence.
                }
            }
        }
    })
}
