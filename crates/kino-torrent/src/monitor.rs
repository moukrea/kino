//! F-014 adaptive-buffer monitor: drives the [`scheduler`] over time.
//!
//! The pure state machine in [`crate::scheduler::compute_state`] is the
//! decision core. This module wires it to a clock and to a stats source:
//!
//! - **Sampling tick** ([`MonitorConfig::sampling_interval`], PRD §F-014: 1 s):
//!   pulls a [`SampleStats`] from the source, pushes its `download_speed_bps`
//!   into a rolling 30-s [`RollingRate`].
//! - **Recompute tick** ([`MonitorConfig::recompute_interval`], PRD §F-014:
//!   5 s) **or position change**: pulls the latest snapshot, evaluates
//!   [`compute_state`], emits the new [`BufferStatus`] on the
//!   [`watch::channel`] surfaced via [`BufferMonitor::status_rx`].
//!
//! The [`StatsSource`] trait is the test seam: production wires
//! [`crate::stats::EngineStats`] off [`crate::AddedTorrent::live_stats`];
//! tests provide a synthetic source driven by a shared `Mutex`.
//!
//! The monitor task exits cleanly when the [`BufferMonitor`] is dropped
//! (the shutdown channel closes); the spawned task observes the close and
//! breaks out of the select loop.
//!
//! ## What this module deliberately does NOT do
//!
//! - **No piece-priority side effects.** librqbit 8.1.1 does not expose
//!   its piece-priority API publicly; the scheduler relies on librqbit's
//!   stream-mode-driven natural prioritisation around the active read
//!   offset. ADR-106 documents the gap. The state machine, sampler, and
//!   buffer:status events are what the PRD §F-014 §6A acceptance pins.
//! - **No event emission to Tauri.** The monitor surfaces a
//!   [`watch::Receiver<BufferStatus>`]; the Tauri host bridges that into
//!   `buffer:status` events. Keeping the bridge in `src-tauri` lets
//!   `kino-torrent` stay framework-agnostic (and testable without a
//!   Tauri app handle).

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::scheduler::{
    compute_state, default_recompute_interval, pieces_ahead_seconds, BufferState, RollingRate,
    SchedulerInputs,
};

/// One sample of a torrent's download progress, as observed by a
/// [`StatsSource`]. The rolling rate estimator only consumes
/// `download_speed_bps`; `bytes_downloaded` feeds the state-machine math
/// directly on each recompute.
#[derive(Debug, Clone, Copy, Default)]
pub struct SampleStats {
    /// Bytes downloaded for the file being streamed (NOT the whole torrent
    /// for multi-file packs).
    pub bytes_downloaded: u64,
    /// Current download rate in bytes/s. Sources can pre-smooth this; the
    /// monitor's [`RollingRate`] re-averages over [`DL_RATE_WINDOW_S`].
    pub download_speed_bps: f64,
}

/// Trait the [`BufferMonitor`] uses to pull a fresh sample.
///
/// Implemented in production by [`LibrqbitStatsSource`] (over an
/// [`crate::AddedTorrent`] plus a file index); implemented in tests by an
/// in-memory mock that lets the test drive the rate / bytes-downloaded
/// curve. The trait is sync because librqbit's `stats()` itself is sync
/// (parking_lot-protected snapshot); making it async would mean wrapping
/// every read in `spawn_blocking` for no benefit.
pub trait StatsSource: Send + Sync + 'static {
    /// Take an instantaneous snapshot of the stream's download progress.
    fn sample(&self) -> SampleStats;
}

/// Tuning knobs for the monitor. `default()` matches PRD §F-014 / §8.
#[derive(Debug, Clone)]
pub struct MonitorConfig {
    /// Size of the file being streamed, in bytes.
    pub file_size_bytes: u64,
    /// Playable duration, in seconds.
    pub duration_s: f64,
    /// How often the sampler ticks the stats source. PRD §F-014: 1 s.
    pub sampling_interval: Duration,
    /// How often the state machine recomputes in the absence of a position
    /// event. PRD §F-014: 5 s.
    pub recompute_interval: Duration,
}

impl MonitorConfig {
    /// Build a config with PRD-locked cadences and a per-stream `file_size`
    /// / `duration_s`. Both fields must be non-zero for the scheduler math
    /// to produce useful output; the monitor still runs with zero inputs
    /// (status stays at default).
    #[must_use]
    pub fn new(file_size_bytes: u64, duration_s: f64) -> Self {
        Self {
            file_size_bytes,
            duration_s,
            sampling_interval: Duration::from_secs(1),
            recompute_interval: default_recompute_interval(),
        }
    }
}

/// Snapshot the monitor publishes on every recompute. Mirrors the on-wire
/// `buffer:status` Tauri event payload.
#[derive(Debug, Clone, PartialEq)]
pub struct BufferStatus {
    pub state: BufferState,
    pub dl_rate_bytes_per_s: f64,
    pub pieces_ahead_seconds: f64,
    pub bytes_downloaded: u64,
    pub file_size_bytes: u64,
    pub position_s: f64,
    pub duration_s: f64,
    /// Estimated seconds remaining to download the full file at the
    /// current rolling rate; `None` if `dl_rate` is 0.
    pub eta_seconds: Option<f64>,
}

impl Default for BufferStatus {
    fn default() -> Self {
        Self {
            state: BufferState::Safe,
            dl_rate_bytes_per_s: 0.0,
            pieces_ahead_seconds: 0.0,
            bytes_downloaded: 0,
            file_size_bytes: 0,
            position_s: 0.0,
            duration_s: 0.0,
            eta_seconds: None,
        }
    }
}

/// Handle to a running [`BufferMonitor`]. Drop to stop the task; clone the
/// inner channels to fan out.
pub struct BufferMonitor {
    status_rx: watch::Receiver<BufferStatus>,
    position_tx: watch::Sender<f64>,
    /// `Some` while the monitor task is alive. The drop impl signals
    /// shutdown by dropping the sender side of `_shutdown_tx`.
    _shutdown_tx: mpsc::Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl BufferMonitor {
    /// Spawn the monitor task and return its handle. The task runs until
    /// the returned [`BufferMonitor`] is dropped.
    pub fn spawn<S: StatsSource>(config: MonitorConfig, source: S) -> Self {
        let (status_tx, status_rx) = watch::channel(BufferStatus {
            file_size_bytes: config.file_size_bytes,
            duration_s: config.duration_s,
            ..BufferStatus::default()
        });
        let (position_tx, position_rx) = watch::channel(0.0_f64);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);

        let source = Arc::new(source);
        let handle = tokio::spawn(run_loop(
            config,
            source,
            status_tx,
            position_rx,
            shutdown_rx,
        ));

        Self {
            status_rx,
            position_tx,
            _shutdown_tx: shutdown_tx,
            handle: Some(handle),
        }
    }

    /// Latest [`BufferStatus`]. Cloning the receiver lets the Tauri host
    /// fan a `watch::Sender::changed()` loop into `app.emit("buffer:status", …)`
    /// calls.
    #[must_use]
    pub fn status_rx(&self) -> watch::Receiver<BufferStatus> {
        self.status_rx.clone()
    }

    /// Push a fresh playhead position. PRD §F-014 says "recompute on
    /// events"; the monitor's select loop wakes on every change and
    /// re-evaluates immediately.
    pub fn update_position(&self, position_s: f64) {
        let _ = self.position_tx.send(position_s);
    }

    /// Snapshot the current status without subscribing. Useful for
    /// one-shot `buffer_status(token)` Tauri queries.
    #[must_use]
    pub fn current(&self) -> BufferStatus {
        self.status_rx.borrow().clone()
    }

    /// Wait for the next status change, returning the post-change value.
    /// Test helper; production code subscribes via [`Self::status_rx`].
    ///
    /// # Errors
    /// Returns an error if the monitor task has exited.
    pub async fn next_status(&mut self) -> Result<BufferStatus, watch::error::RecvError> {
        self.status_rx.changed().await?;
        Ok(self.status_rx.borrow_and_update().clone())
    }
}

impl Drop for BufferMonitor {
    fn drop(&mut self) {
        // Dropping `_shutdown_tx` closes the channel; the select loop
        // observes the close in its next branch evaluation and exits.
        // We do NOT block on `handle.await` — Drop is sync.
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

async fn run_loop<S: StatsSource>(
    config: MonitorConfig,
    source: Arc<S>,
    status_tx: watch::Sender<BufferStatus>,
    mut position_rx: watch::Receiver<f64>,
    mut shutdown_rx: mpsc::Receiver<()>,
) {
    let mut rate = RollingRate::new();
    let mut sample_interval = tokio::time::interval(config.sampling_interval);
    let mut recompute_interval = tokio::time::interval(config.recompute_interval);
    // tokio::time::interval fires immediately on the first tick; consume
    // the leading tick so we don't double-sample at t=0.
    sample_interval.tick().await;
    recompute_interval.tick().await;

    // Prime the status with an initial sample so subscribers don't see
    // the all-zero default forever if recompute is slow.
    let initial = source.sample();
    let position = *position_rx.borrow_and_update();
    rate.push(Instant::now(), initial.download_speed_bps);
    publish(&status_tx, &config, &rate, &initial, position);

    loop {
        tokio::select! {
            _ = sample_interval.tick() => {
                let snap = source.sample();
                rate.push(Instant::now(), snap.download_speed_bps);
            }
            _ = recompute_interval.tick() => {
                let snap = source.sample();
                let position = *position_rx.borrow();
                rate.push(Instant::now(), snap.download_speed_bps);
                publish(&status_tx, &config, &rate, &snap, position);
            }
            change = position_rx.changed() => {
                if change.is_err() {
                    // Sender dropped; monitor is going away.
                    break;
                }
                let snap = source.sample();
                let position = *position_rx.borrow_and_update();
                publish(&status_tx, &config, &rate, &snap, position);
            }
            _ = shutdown_rx.recv() => break,
        }
    }
}

#[allow(clippy::cast_precision_loss)] // file sizes < 2^52 in practice
fn publish(
    status_tx: &watch::Sender<BufferStatus>,
    config: &MonitorConfig,
    rate: &RollingRate,
    snap: &SampleStats,
    position_s: f64,
) {
    let avg = rate.average_bps();
    let pieces_ahead = pieces_ahead_seconds(
        snap.bytes_downloaded,
        position_s,
        config.file_size_bytes,
        config.duration_s,
    );
    let inputs = SchedulerInputs {
        file_size_bytes: config.file_size_bytes,
        bytes_downloaded: snap.bytes_downloaded,
        dl_rate_bytes_per_s: avg,
        duration_s: config.duration_s,
        position_s,
        pieces_ahead_seconds: pieces_ahead,
    };
    let state = compute_state(&inputs);
    let remaining = config.file_size_bytes.saturating_sub(snap.bytes_downloaded);
    let eta_seconds = if avg > 0.0 {
        Some(remaining as f64 / avg)
    } else {
        None
    };

    let next = BufferStatus {
        state,
        dl_rate_bytes_per_s: avg,
        pieces_ahead_seconds: pieces_ahead,
        bytes_downloaded: snap.bytes_downloaded,
        file_size_bytes: config.file_size_bytes,
        position_s,
        duration_s: config.duration_s,
        eta_seconds,
    };
    // send_modify so subscribers see EVERY recompute as a change, even
    // when state is identical — the UI relies on the dl_rate / progress
    // fields to drive the overlay's progress bar and rate display.
    status_tx.send_replace(next);
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // tests deliberately assert exact sentinel values
mod tests {
    use super::*;
    use parking_lot::Mutex;

    /// Mock source where the test feeds a sequence of stats samples.
    /// `sample()` consumes the next entry; if exhausted, repeats the
    /// final entry forever so the monitor doesn't crash mid-test.
    struct FakeSource {
        scripted: Mutex<Vec<SampleStats>>,
        last: Mutex<SampleStats>,
    }

    impl FakeSource {
        fn new(samples: Vec<SampleStats>) -> Self {
            Self {
                last: Mutex::new(samples.first().copied().unwrap_or_default()),
                scripted: Mutex::new(samples.into_iter().rev().collect()),
            }
        }
    }

    impl StatsSource for FakeSource {
        fn sample(&self) -> SampleStats {
            let mut g = self.scripted.lock();
            if let Some(s) = g.pop() {
                *self.last.lock() = s;
                s
            } else {
                *self.last.lock()
            }
        }
    }

    /// Source that always reports the same fast-link snapshot.
    struct FastSource {
        bytes: u64,
        rate: f64,
    }
    impl StatsSource for FastSource {
        fn sample(&self) -> SampleStats {
            SampleStats {
                bytes_downloaded: self.bytes,
                download_speed_bps: self.rate,
            }
        }
    }

    /// 1h playable, 10 GB file. `file_bitrate ≈ 2.78 MB/s` — large enough
    /// that a 100 KB/s rolling rate decisively triggers `NEEDS_PREBUFFER`
    /// without us having to fiddle with the locked `SAFETY_MARGIN_S` /
    /// `PREBUFFER_TARGET_S` thresholds.
    fn config_1h_10gb() -> MonitorConfig {
        MonitorConfig {
            file_size_bytes: 10_000_000_000,
            duration_s: 3600.0,
            sampling_interval: Duration::from_millis(50),
            recompute_interval: Duration::from_millis(100),
        }
    }

    #[tokio::test]
    async fn fast_source_transitions_to_safe() {
        // 10 GB file at 100 MB/s → ttdl ≈ 100 s; headroom ≈ 3570 s → SAFE.
        let monitor = BufferMonitor::spawn(
            config_1h_10gb(),
            FastSource {
                bytes: 100_000_000,
                rate: 100_000_000.0,
            },
        );
        // Give the monitor a few recompute ticks to settle.
        tokio::time::sleep(Duration::from_millis(350)).await;
        let s = monitor.current();
        assert_eq!(s.state, BufferState::Safe, "fast source should be SAFE");
        assert!(s.dl_rate_bytes_per_s > 0.0);
    }

    #[tokio::test]
    async fn slow_source_emits_needs_prebuffer() {
        // 100 KB/s for a 1h 100 MB stream = NEEDS_PREBUFFER from cold start.
        let monitor = BufferMonitor::spawn(
            config_1h_10gb(),
            FastSource {
                bytes: 0,
                rate: 100_000.0,
            },
        );
        tokio::time::sleep(Duration::from_millis(350)).await;
        let s = monitor.current();
        match s.state {
            BufferState::NeedsPrebuffer { .. } => {}
            other => panic!("expected NeedsPrebuffer, got {other:?}"),
        }
        assert!(s.eta_seconds.is_some());
    }

    #[tokio::test]
    async fn position_update_triggers_immediate_recompute() {
        // Slow source so we stay in NEEDS_PREBUFFER throughout; we use the
        // position update to verify the watch::Receiver fires.
        let mut monitor = BufferMonitor::spawn(
            config_1h_10gb(),
            FastSource {
                bytes: 0,
                rate: 100_000.0,
            },
        );
        // Drain the initial publish.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let _ = monitor.next_status().await;

        monitor.update_position(120.0);
        let s = monitor.next_status().await.expect("monitor alive");
        assert!((s.position_s - 120.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn monitor_picks_up_changing_samples_over_time() {
        // Start slow (NEEDS_PREBUFFER for the 10 GB / 1 h config — file
        // bitrate ≈ 2.78 MB/s, 100 KB/s rolling rate cannot keep up),
        // then become fast.
        let source = FakeSource::new(vec![
            SampleStats {
                bytes_downloaded: 0,
                download_speed_bps: 100_000.0,
            },
            SampleStats {
                bytes_downloaded: 5_000_000,
                download_speed_bps: 100_000_000.0,
            },
            SampleStats {
                bytes_downloaded: 1_000_000_000,
                download_speed_bps: 100_000_000.0,
            },
        ]);
        let mut monitor = BufferMonitor::spawn(config_1h_10gb(), source);

        // Initial publish should be NEEDS_PREBUFFER (slow first sample).
        let s0 = monitor.next_status().await.expect("first publish");
        assert!(matches!(s0.state, BufferState::NeedsPrebuffer { .. }));

        // After enough recompute ticks the rolling avg climbs and the
        // final state should be SAFE OR very close. We assert only that
        // the rolling rate climbed past 1 MB/s to keep the test robust
        // to scheduler jitter on CI.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let s = monitor.current();
        assert!(
            s.dl_rate_bytes_per_s > 1_000_000.0,
            "rolling rate should have climbed: {}",
            s.dl_rate_bytes_per_s
        );
    }

    #[tokio::test]
    async fn drop_terminates_task() {
        let monitor = BufferMonitor::spawn(
            config_1h_10gb(),
            FastSource {
                bytes: 0,
                rate: 0.0,
            },
        );
        drop(monitor);
        // Yield so the task gets a chance to observe the channel close.
        tokio::time::sleep(Duration::from_millis(20)).await;
        // No assertion — the test passes if no panic / hang occurs.
    }
}
