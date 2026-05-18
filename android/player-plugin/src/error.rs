//! Plugin-internal error vocabulary (PRD §F-015 Android driver).
//!
//! The Rust driver bridges three error surfaces: Tauri's plugin invoke
//! errors, JSON (de)serialization failures, and the
//! [`kino_player::PlayerError`] enum the rest of the workspace
//! consumes. [`PluginError`] is the local sum; [`From`] impls collapse
//! each subcase into the closest [`PlayerError`] variant when the
//! driver hands an error back to the host.

use thiserror::Error;

use kino_player::PlayerError;

/// Errors the Android plugin shell can surface.
#[derive(Debug, Error)]
pub enum PluginError {
    /// `tauri-plugin-kino-player::init()` was never installed on this
    /// `tauri::Builder`. Almost always a host-side wiring bug.
    #[error("kino-player plugin not registered with the Tauri host")]
    Unregistered,

    /// The Tauri mobile plugin invoke layer returned an error. Wraps
    /// the original message because [`tauri::plugin::mobile`] error
    /// types are not stable across Tauri 2 minor releases.
    #[error("android plugin invoke failed: {0}")]
    Invoke(String),

    /// Argument or response serialization failed. Usually means a Rust
    /// / Kotlin DTO mismatch — fix the [`crate::models`] schema.
    #[error("payload encode/decode failed: {0}")]
    Codec(#[from] serde_json::Error),

    /// The Kotlin plugin reported a player-side failure (e.g.
    /// `ExoPlayer` refused to load, surface destroyed mid-playback).
    /// Surfaced verbatim so the frontend's error overlay can show the
    /// message.
    #[error("android player error: {0}")]
    Player(String),

    /// The crate is compiled into a non-Android target and a
    /// driver-only path was hit. Should never fire in production —
    /// the stub driver short-circuits before this happens.
    #[error("kino-player Android driver invoked on a non-Android target")]
    NotAndroid,
}

impl From<PluginError> for PlayerError {
    fn from(value: PluginError) -> Self {
        match value {
            PluginError::Codec(e) => PlayerError::Parse {
                message: e.to_string(),
                line: String::new(),
            },
            PluginError::Player(message) => PlayerError::Backend(message),
            PluginError::Unregistered => PlayerError::Spawn(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "kino-player plugin not registered with the Tauri host",
            )),
            PluginError::NotAndroid => PlayerError::Spawn(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "kino-player Android driver invoked on a non-Android target",
            )),
            other @ PluginError::Invoke(_) => PlayerError::Backend(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_collapses_to_spawn() {
        let err: PlayerError = PluginError::Unregistered.into();
        assert!(matches!(err, PlayerError::Spawn(_)));
    }

    #[test]
    fn player_collapses_to_backend() {
        let err: PlayerError = PluginError::Player("boom".into()).into();
        match err {
            PlayerError::Backend(m) => assert_eq!(m, "boom"),
            other => panic!("expected Backend, got {other:?}"),
        }
    }

    #[test]
    fn invoke_message_round_trips_via_display() {
        let err = PluginError::Invoke("missing arg".into());
        assert_eq!(err.to_string(), "android plugin invoke failed: missing arg");
    }

    #[test]
    fn not_android_collapses_to_spawn_unsupported() {
        let err: PlayerError = PluginError::NotAndroid.into();
        let PlayerError::Spawn(io_err) = err else {
            panic!("expected Spawn, got something else");
        };
        assert_eq!(io_err.kind(), std::io::ErrorKind::Unsupported);
    }
}
