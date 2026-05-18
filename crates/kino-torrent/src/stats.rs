//! Stats accessor on [`crate::AddedTorrent`] + a [`crate::monitor::StatsSource`]
//! implementation that bridges the F-014 monitor onto librqbit.
//!
//! `kino-torrent` deliberately wraps every librqbit type it surfaces (PRD
//! ADR-101 pattern) so the rest of the workspace doesn't depend on
//! librqbit's evolving public API. [`EngineStats`] is the domain-shaped
//! view we expose; [`LibrqbitStatsSource`] is the F-014 plug.

use crate::engine::AddedTorrent;
use crate::monitor::{SampleStats, StatsSource};

/// Domain-shaped snapshot of a torrent's live download progress. Sourced
/// from librqbit's `TorrentStats` but free of librqbit's API churn.
#[derive(Debug, Clone, Default)]
pub struct EngineStats {
    /// Total downloaded bytes summed across every file.
    pub progress_bytes: u64,
    /// Total file-set size in bytes.
    pub total_bytes: u64,
    /// Per-file downloaded byte count, ordered by `file_index`.
    pub file_progress: Vec<u64>,
    /// Current download rate in **bytes/s** (librqbit reports MiB/s under
    /// the misleadingly-named `mbps` field; we convert here).
    pub download_speed_bps: f64,
    /// `true` when the file set is fully downloaded.
    pub finished: bool,
}

const MIB_TO_B: f64 = 1024.0 * 1024.0;

impl AddedTorrent {
    /// Pull a fresh [`EngineStats`] off the underlying librqbit handle.
    /// Cheap (parking_lot-protected snapshot inside librqbit); call once
    /// per sampler tick rather than from the hot byte-serving path.
    #[must_use]
    pub fn live_stats(&self) -> EngineStats {
        let raw = self.inner().stats();
        EngineStats {
            progress_bytes: raw.progress_bytes,
            total_bytes: raw.total_bytes,
            file_progress: raw.file_progress.clone(),
            download_speed_bps: raw
                .live
                .as_ref()
                .map_or(0.0, |l| l.download_speed.mbps * MIB_TO_B),
            finished: raw.finished,
        }
    }
}

/// `StatsSource` that drives the F-014 monitor from a live
/// [`AddedTorrent`] + the file index being streamed.
///
/// Sampling reads `live_stats().file_progress[file_index]` for
/// `bytes_downloaded` so multi-file packs only account for the active
/// video file; `download_speed_bps` comes from the torrent-wide rate (no
/// per-file rate is available in librqbit 8.1.1).
#[derive(Clone)]
pub struct LibrqbitStatsSource {
    torrent: AddedTorrent,
    file_index: usize,
}

impl LibrqbitStatsSource {
    /// Build a source bound to `torrent` + `file_index`.
    #[must_use]
    pub fn new(torrent: AddedTorrent, file_index: usize) -> Self {
        Self {
            torrent,
            file_index,
        }
    }
}

impl StatsSource for LibrqbitStatsSource {
    fn sample(&self) -> SampleStats {
        let s = self.torrent.live_stats();
        let bytes_downloaded = s
            .file_progress
            .get(self.file_index)
            .copied()
            .unwrap_or(s.progress_bytes);
        SampleStats {
            bytes_downloaded,
            download_speed_bps: s.download_speed_bps,
        }
    }
}
