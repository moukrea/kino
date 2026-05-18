//! Native player drivers for kino (PRD §F-015).
//!
//! This crate hosts the OS-level player abstraction that the Tauri host
//! drives during playback. The PRD pins two backends:
//!
//! - **Linux** (ADR-011): libmpv with controls overlaid in the `SolidJS`
//!   webview. Shipped here as a subprocess driver around the standalone
//!   `mpv` binary, communicating via mpv's JSON-IPC socket
//!   (`--input-ipc-server=<path>`). See [`MpvPlayer`] and ADR-108 for why
//!   v1 ships the subprocess form instead of the in-process
//!   `libmpv-rs` bindings: the renderer-into-Tauri-window integration is
//!   not realistically deliverable in a single session, and the
//!   subprocess form keeps the CI build matrix free of a hard `libmpv2`
//!   link dependency. The two forms expose the same operational signal
//!   surface (`PlayerEvent`) so the in-process driver can drop in as a
//!   follow-up without touching `kino-app` callers.
//! - **Android** (ADR-010): native Kotlin `PlayerActivity` driving
//!   `ExoPlayer`; wired via a Tauri plugin. This crate does not own that
//!   path — it stays in the `src-tauri` / `android/player-plugin/` tree.
//!
//! The shared surface every backend exposes:
//!
//! - [`PlayerHandle`]: control commands (open / close / pause / seek /
//!   audio + subtitle track selection).
//! - [`PlayerEvent`]: position ticks, state changes, exit notifications,
//!   and error reports. Fanned to:
//!   * the Tauri host's [`buffer_report_position`][buffer]
//!     so the F-014 monitor recomputes on the player's clock;
//!   * the F-012 [`cw_record_position`][cw] so Continue Watching always
//!     has the latest position;
//!   * Tauri events (`player:position`, `player:state`, `player:exit`,
//!     `player:error`) the frontend subscribes to for the overlay UI.
//!
//! Position ticks are emitted on the PRD §8
//! `PLAYER_POSITION_INTERVAL_S = 5 s` cadence; the backend may emit more
//! frequently on seeks and state changes — consumers MUST be idempotent.
//!
//! [buffer]: ../kino_app_lib/commands/fn.buffer_report_position.html
//! [cw]: ../kino_app_lib/commands/fn.cw_record_position.html

pub mod error;
pub mod event;
pub mod handle;
pub mod ipc;
pub mod state;
pub mod tracks;

#[cfg(target_os = "linux")]
pub mod mpv;

pub use error::PlayerError;
pub use event::{PlayerEvent, PositionTick};
pub use handle::{OpenRequest, PlayerHandle};
pub use state::{PlayerSnapshot, PlayerState};
pub use tracks::{AudioTrack, SubtitleTrack, TrackList};

#[cfg(target_os = "linux")]
pub use mpv::MpvPlayer;
