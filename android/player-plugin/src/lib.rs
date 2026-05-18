//! `tauri-plugin-kino-player` — Tauri 2 mobile plugin that wraps the
//! kino Android `PlayerActivity` (PRD §F-015, ADR-010).
//!
//! ## Architecture
//!
//! The Linux player driver (`kino_player::MpvPlayer`) speaks to a
//! sidecar `mpv` subprocess over a JSON-IPC socket. The Android player
//! cannot follow the same shape: PRD §F-015 prescribes a native Kotlin
//! `PlayerActivity` running in a separate Android activity (fullscreen,
//! immersive, owning `ExoPlayer`), and the Tauri host (which lives in
//! `MainActivity`) drives it across a process-internal IPC.
//!
//! This crate is the bridge:
//!
//! - **Rust side** ([`AndroidPlayer`]): a [`kino_player::PlayerHandle`]
//!   implementation that forwards every method (`open` / `close` /
//!   `set_paused` / `seek` / `select_audio_track` /
//!   `select_subtitle_track`) to the companion Kotlin plugin via the
//!   Tauri mobile plugin invoke mechanism. Events flow the other way —
//!   the Kotlin plugin queues `PlayerEvent`s on the activity main
//!   thread and the driver polls them on a dedicated tokio task,
//!   rebroadcasting through a `tokio::sync::broadcast` channel so the
//!   `kino-app` bridge task sees the same event vocabulary as the mpv
//!   driver.
//!
//! - **Kotlin side** (`android/src/main/java/dev/kino/player/`): the
//!   `PlayerPlugin` (Tauri `Plugin` subclass with `@Command` methods)
//!   launches the `PlayerActivity` on `open`, holds a strong reference
//!   to it for command dispatch, and shuttles `PlayerEvent`s back via a
//!   `LocalBroadcastManager` to its own event queue.
//!
//! ## Desktop builds
//!
//! On non-Android targets the crate compiles as a no-op stub
//! ([`stub::StubPlayer`]) that returns a "platform unsupported" error
//! on every method call. This keeps the workspace `cargo check` green
//! on Linux / macOS / Windows; the actual Android wiring is only
//! exercised by `cargo tauri android build`.

#![cfg_attr(not(target_os = "android"), allow(dead_code))]

pub mod cache;
pub mod error;
pub mod models;

#[cfg(target_os = "android")]
mod mobile;
#[cfg(not(target_os = "android"))]
mod stub;

use std::sync::Arc;

pub use error::PluginError;
pub use kino_player::{
    OpenRequest, PlayerError, PlayerEvent, PlayerHandle, PlayerSnapshot, TrackList,
};
pub use models::*;

/// Convenience alias for the type-erased handle the host stashes in
/// app state.
pub type SharedPlayer = Arc<dyn PlayerHandle>;

/// Tauri plugin name. Pinned here so the host registration matches the
/// Kotlin-side `PluginConfig.namespace` and the `register_android_plugin`
/// identifier.
pub const PLUGIN_NAME: &str = "kino-player";

/// Kotlin-side fully-qualified plugin identifier the Tauri mobile
/// runtime uses to resolve the JNI class on Android.
#[cfg(target_os = "android")]
pub(crate) const ANDROID_PLUGIN_IDENTIFIER: &str = "dev.kino.player";

/// Build the Tauri plugin shell. Host wires this into
/// `tauri::Builder::default().plugin(tauri_plugin_kino_player::init())`.
///
/// The plugin's only host-visible side effect is installing
/// [`AndroidPlayer`] (or [`stub::StubPlayer`] on desktop) as a managed
/// [`SharedPlayer`] in app state. The Tauri command layer that drives
/// playback (`kino-app::commands::spawn_platform_player`) reads the
/// `SharedPlayer` out of state via `app.state::<SharedPlayer>()`.
#[must_use]
pub fn init<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    use tauri::plugin::Builder as PluginBuilder;
    use tauri::Manager;

    PluginBuilder::new(PLUGIN_NAME)
        .setup(|app, _api| {
            #[cfg(target_os = "android")]
            {
                let driver = mobile::AndroidPlayer::register(app, &_api)?;
                let shared: SharedPlayer = Arc::new(driver);
                app.manage(shared);
            }
            #[cfg(not(target_os = "android"))]
            {
                let _ = app;
                let driver = stub::StubPlayer::new();
                let shared: SharedPlayer = Arc::new(driver);
                app.manage(shared);
            }
            Ok(())
        })
        .build()
}

/// Pull the shared player handle out of Tauri's managed state.
///
/// # Errors
///
/// Returns an error if the host failed to register the plugin (the
/// `setup` callback above sets the state on success). In practice the
/// only way this fires is if the plugin init failed — see plugin setup
/// logs.
pub fn handle<R: tauri::Runtime, M: tauri::Manager<R>>(
    manager: &M,
) -> Result<SharedPlayer, PluginError> {
    manager
        .try_state::<SharedPlayer>()
        .map(|s| Arc::clone(&*s))
        .ok_or(PluginError::Unregistered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_name_pinned() {
        assert_eq!(PLUGIN_NAME, "kino-player");
    }

    #[cfg(target_os = "android")]
    #[test]
    fn android_plugin_identifier_pinned() {
        assert_eq!(ANDROID_PLUGIN_IDENTIFIER, "dev.kino.player");
    }
}
