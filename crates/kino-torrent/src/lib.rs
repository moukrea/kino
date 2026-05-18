//! `kino-torrent` — embedded torrent engine and adaptive-buffer scheduler.
//!
//! Wraps [`librqbit::Session`] under a thin façade tailored to kino's
//! playback flow (PRD §F-013). Future sessions add the piece-priority
//! scheduler (F-014); this session ships the engine surface, supplementary
//! tracker list, and the bridge to [`kino-server`](../kino_server/).
//!
//! The public surface is:
//!
//! - [`Engine`] — owns the `Session`, accepts magnets or `.torrent` bytes,
//!   exposes file streams.
//! - [`EngineConfig`] — locked PRD §F-013 / §8 knobs (cache root,
//!   supplementary trackers, DHT/PEX, max connections per torrent).
//! - [`AddedTorrent`] — handle returned from [`Engine::add`]; lets callers
//!   enumerate files, pick the largest video, and open a [`FileStream`] for
//!   HTTP serving.
//! - [`FileInfo`] — name/index/size triple surfaced to the host.

pub mod engine;
pub mod trackers;

pub use engine::{
    AddInput, AddedTorrent, Engine, EngineConfig, EngineError, FileInfo, FileStream,
    LARGEST_VIDEO_EXTENSIONS,
};
pub use trackers::SUPPLEMENTARY_TRACKERS;
