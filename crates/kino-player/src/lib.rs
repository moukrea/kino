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

/// GTK widget-tree surgery for F-015 Linux libmpv in-window GL rendering
/// (Session 036 spike for ADR-133 Route B). See module docs for the
/// reparenting strategy and spike scope.
#[cfg(target_os = "linux")]
pub mod surface;

/// In-process libmpv driver gated by the `libmpv-inprocess` Cargo
/// feature (Session 037 / ADR-133 Route B). When the feature is OFF
/// (default through Session 037) this module is excluded from the
/// build so the workspace does NOT require `libmpv2-dev` at link
/// time. Session 038 adds the apt-install line + bundle deps and
/// flips the feature on by default after §6B-1 hardware verification.
#[cfg(all(target_os = "linux", feature = "libmpv-inprocess"))]
pub mod libmpv;

pub use error::PlayerError;
pub use event::{PlayerEvent, PositionTick};
pub use handle::{OpenRequest, PlayerHandle};
pub use state::{PlayerSnapshot, PlayerState};
pub use tracks::{AudioTrack, SubtitleTrack, TrackList};

#[cfg(target_os = "linux")]
pub use mpv::MpvPlayer;

#[cfg(target_os = "linux")]
pub use surface::{inject_overlay, OverlaySurgery, SurfaceError};

#[cfg(all(target_os = "linux", feature = "libmpv-inprocess"))]
pub use libmpv::LibmpvPlayer;
