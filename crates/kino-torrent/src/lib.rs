//! `kino-torrent` — embedded torrent engine and adaptive-buffer scheduler.
//!
//! Wraps [`librqbit::Session`] under a thin façade tailored to kino's
//! playback flow (PRD §F-013) plus the adaptive-buffer state machine
//! (PRD §F-014). The public surface is:
//!
//! - [`Engine`] — owns the `Session`, accepts magnets or `.torrent` bytes,
//!   exposes file streams.
//! - [`EngineConfig`] — locked PRD §F-013 / §8 knobs (cache root,
//!   supplementary trackers, DHT/PEX, max connections per torrent).
//! - [`AddedTorrent`] — handle returned from [`Engine::add`]; lets callers
//!   enumerate files, pick the largest video, and open a [`FileStream`] for
//!   HTTP serving. Exposes [`AddedTorrent::live_stats`] for the F-014
//!   monitor.
//! - [`FileInfo`] — name/index/size triple surfaced to the host.
//! - [`scheduler`] — pure PRD §F-014 state machine + rolling-rate estimator.
//! - [`monitor`] — async loop that drives the scheduler off a
//!   [`monitor::StatsSource`] and emits [`monitor::BufferStatus`] on a
//!   `tokio::sync::watch` channel.
//! - [`stats`] — domain-shaped [`stats::EngineStats`] view of librqbit's
//!   live snapshot and the production [`stats::LibrqbitStatsSource`] that
//!   plugs the monitor onto an [`AddedTorrent`].

pub mod engine;
pub mod monitor;
pub mod scheduler;
pub mod stats;
pub mod trackers;

pub use engine::{
    AddInput, AddedTorrent, Engine, EngineConfig, EngineError, FileInfo, FileStream,
    LARGEST_VIDEO_EXTENSIONS,
};
pub use monitor::{BufferMonitor, BufferStatus, MonitorConfig, SampleStats, StatsSource};
pub use scheduler::{
    compute_state, pieces_ahead_seconds, BufferState, RollingRate, SchedulerInputs,
};
pub use stats::{EngineStats, LibrqbitStatsSource};
pub use trackers::SUPPLEMENTARY_TRACKERS;
