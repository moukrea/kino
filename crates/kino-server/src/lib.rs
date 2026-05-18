//! `kino-server` — local axum HTTP server consumed by the platform player.
//!
//! Binds on `127.0.0.1:0` (OS-assigned port) and exposes one route,
//! `GET /stream/{token}`, that streams bytes from a registered torrent file.
//! The server is the gateway between [`kino_torrent`] and the platform
//! player (`ExoPlayer` on Android, `libmpv` on Linux); PRD §F-013 mandates
//! that the route honor HTTP Range so the player can seek without
//! re-issuing the entire request.
//!
//! ## Lifecycle
//!
//! 1. The Tauri host constructs a [`kino_torrent::Engine`] at boot.
//! 2. The host calls [`Server::spawn`] once per process; the returned
//!    [`ServerHandle`] is stored in Tauri-managed state next to the engine.
//! 3. On `start_playback`, the host adds the torrent to the engine, picks
//!    the largest video file, then calls [`ServerHandle::register`] to
//!    bind a freshly minted UUID v4 to the file. The host returns
//!    `http://127.0.0.1:{port}/stream/{token}` to the frontend.
//! 4. On `stop_playback`, the host calls [`ServerHandle::unregister`] and
//!    (optionally) [`kino_torrent::Engine::remove`].
//!
//! Tokens are scoped to a single process; the registry lives in
//! `Arc<RwLock<HashMap>>`. App shutdown drops the lot.

pub mod range;
pub mod server;

pub use server::{ServerError, ServerHandle, StreamSession};
