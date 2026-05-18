//! F-014 adaptive-buffer state machine.
//!
//! This module is the pure-Rust core of the adaptive buffer. It is
//! deliberately library-agnostic: every input is supplied by the caller
//! ([`SchedulerInputs`]), every output is a [`BufferState`] enum, and there
//! is no I/O. The async loop that drives this in production lives in
//! [`crate::monitor`].
//!
//! ## The math (PRD §F-014, locked)
//!
//! ```text
//! remaining_bytes        = file_size - bytes_downloaded
//! time_to_dl_remaining   = remaining_bytes / dl_rate_rolling   (∞ if dl_rate ≈ 0)
//! time_to_play_remaining = duration_s - position_s
//!
//! if time_to_dl_remaining <= time_to_play_remaining - safety_margin_s:
//!     state = SAFE
//! else:
//!     deficit_s = time_to_dl_remaining - (time_to_play_remaining - safety_margin_s)
//!     required_prebuffer_s = max(prebuffer_target_s, deficit_s)
//!     state = NEEDS_PREBUFFER(required_prebuffer_s)
//!
//! if pieces_ahead_seconds < safety_margin_s * 0.5:
//!     state = REBUFFER
//! ```
//!
//! The constants come from [`kino_core::constants`] — they are PRD-locked.
//! The pure function lets us run the entire decision tree under `#[test]`
//! with no librqbit, no clocks, and no event loops.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use kino_core::constants::{
    DL_RATE_WINDOW_S, PREBUFFER_TARGET_S, RECOMPUTE_INTERVAL_S, SAFETY_MARGIN_S,
};
use serde::{Deserialize, Serialize};

/// The three states the scheduler resolves to on every recompute (PRD §F-014).
///
/// - [`Safe`](Self::Safe): download will outrun the playhead with at least
///   `SAFETY_MARGIN_S` to spare; the player runs free, no overlay shown.
/// - [`NeedsPrebuffer`](Self::NeedsPrebuffer): we don't have enough lead;
///   the player must stay paused until `required_prebuffer_s` of playback
///   is downloaded ahead. Used on initial play and after a seek.
/// - [`Rebuffer`](Self::Rebuffer): the in-flight stream has eaten through
///   its lead; the player must pause until [`Safe`](Self::Safe) is restored.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BufferState {
    Safe,
    NeedsPrebuffer {
        #[serde(rename = "requiredPrebufferS")]
        required_prebuffer_s: f64,
    },
    Rebuffer,
}

impl BufferState {
    /// True if the player should be paused while we accumulate buffer.
    #[must_use]
    pub fn pauses_playback(&self) -> bool {
        !matches!(self, Self::Safe)
    }
}

/// Pure inputs to [`compute_state`]. Every field comes from outside the
/// scheduler — the monitor harvests them from librqbit + the player.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerInputs {
    /// Size of the file being streamed, in bytes.
    pub file_size_bytes: u64,
    /// Bytes downloaded for the file being streamed (NOT the whole torrent
    /// — multi-file torrents only count the active video file).
    pub bytes_downloaded: u64,
    /// Rolling download rate in bytes/s (PRD §F-014 30-s window).
    pub dl_rate_bytes_per_s: f64,
    /// Total playable duration of the file, in seconds.
    pub duration_s: f64,
    /// Current playhead in seconds (from player events).
    pub position_s: f64,
    /// Seconds of contiguous playback downloaded ahead of the playhead.
    pub pieces_ahead_seconds: f64,
}

/// PRD §F-014 state-machine evaluation. Pure.
///
/// `safety_margin_s` and `prebuffer_target_s` default to
/// [`SAFETY_MARGIN_S`] / [`PREBUFFER_TARGET_S`]; only override these in
/// tests that want to exercise the math at non-default thresholds.
#[must_use]
pub fn compute_state(inputs: &SchedulerInputs) -> BufferState {
    compute_state_with_thresholds(inputs, SAFETY_MARGIN_S, PREBUFFER_TARGET_S)
}

/// Variant of [`compute_state`] that lets the caller override the PRD-locked
/// thresholds. Useful in tests that pin specific boundary behaviour without
/// depending on the value of [`SAFETY_MARGIN_S`].
#[must_use]
#[allow(clippy::cast_precision_loss)] // file sizes < 2^52 in practice
pub fn compute_state_with_thresholds(
    inputs: &SchedulerInputs,
    safety_margin_s: f64,
    prebuffer_target_s: f64,
) -> BufferState {
    // REBUFFER guard runs first per PRD §F-014: even if downstream math
    // would say SAFE, an empty ahead-window means the decoder is about to
    // starve right now and the player must be paused immediately.
    if inputs.pieces_ahead_seconds < safety_margin_s * 0.5 {
        // Initial-play guard: at position 0 with no bytes yet, calling this
        // "REBUFFER" would briefly flash that state before the first
        // prebuffer transition. The caller never has both pieces_ahead == 0
        // AND a real position > 0 unless the stream truly under-ran, so we
        // gate REBUFFER on "the player has actually played some content"
        // (position > 0). For position == 0, fall through to the prebuffer
        // branch which decides between SAFE and NEEDS_PREBUFFER.
        if inputs.position_s > 0.0 {
            return BufferState::Rebuffer;
        }
    }

    let remaining_bytes = inputs
        .file_size_bytes
        .saturating_sub(inputs.bytes_downloaded);
    // PRD says `∞ if dl_rate ≈ 0`. With nothing left to download, the
    // wait is 0 regardless of rate (fully-downloaded torrents from a
    // warm cache hit this path). Otherwise f64 division yields +inf for
    // dl_rate ≈ 0 which flows correctly through the inequality below;
    // we also cap `required_prebuffer_s` to keep the UI progress finite.
    let time_to_dl_remaining = if remaining_bytes == 0 {
        0.0
    } else if inputs.dl_rate_bytes_per_s > 0.0 {
        remaining_bytes as f64 / inputs.dl_rate_bytes_per_s
    } else {
        f64::INFINITY
    };
    let time_to_play_remaining = (inputs.duration_s - inputs.position_s).max(0.0);

    let headroom = time_to_play_remaining - safety_margin_s;

    if time_to_dl_remaining <= headroom {
        BufferState::Safe
    } else {
        let deficit_s = time_to_dl_remaining - headroom;
        // If dl_rate is 0, deficit is +inf; collapse to baseline target so
        // the UI shows a finite progress bar while we wait for the first
        // real rate sample.
        let required_prebuffer_s = if deficit_s.is_finite() {
            deficit_s.max(prebuffer_target_s)
        } else {
            prebuffer_target_s
        };
        BufferState::NeedsPrebuffer {
            required_prebuffer_s,
        }
    }
}

/// Rolling rate estimator over [`DL_RATE_WINDOW_S`] (30s) with samples
/// pushed at a configurable cadence (PRD §F-014: 1s).
///
/// Insertion-order is timestamp-monotonic; samples older than the window
/// are dropped on every `push`. Memory is `O(window / sampling_interval)`.
#[derive(Debug, Clone)]
pub struct RollingRate {
    samples: VecDeque<(Instant, f64)>,
    window: Duration,
}

impl RollingRate {
    /// Build a fresh estimator with the PRD-locked 30s window.
    #[must_use]
    pub fn new() -> Self {
        Self::with_window(Duration::from_secs_f64(DL_RATE_WINDOW_S))
    }

    /// Build an estimator with a non-default window (for tests).
    #[must_use]
    pub fn with_window(window: Duration) -> Self {
        Self {
            samples: VecDeque::new(),
            window,
        }
    }

    /// Push a fresh `bytes/s` sample at `t`. Older samples are dropped.
    pub fn push(&mut self, t: Instant, bytes_per_s: f64) {
        self.samples.push_back((t, bytes_per_s.max(0.0)));
        self.trim_older_than(t);
    }

    /// Drop samples taken before `cutoff = t - window`. Tolerates
    /// `t < window` by clamping the comparison to `Instant::now`-shaped
    /// epoch — i.e. nothing is dropped if every sample lies inside the
    /// window relative to `t`.
    fn trim_older_than(&mut self, t: Instant) {
        while let Some(&(front_t, _)) = self.samples.front() {
            if t.saturating_duration_since(front_t) > self.window {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Mean of the in-window samples. Returns `0.0` when empty so the
    /// scheduler treats a fresh stream as `dl_rate ≈ 0`.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // sample count ≪ 2^52
    pub fn average_bps(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let total: f64 = self.samples.iter().map(|(_, r)| *r).sum();
        total / self.samples.len() as f64
    }

    /// Sample count currently held in the window.
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// True if no samples are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

impl Default for RollingRate {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute `pieces_ahead_seconds` from bytes-downloaded and the playhead.
///
/// `file_bitrate_bytes_per_s = file_size_bytes / duration_s`; the lead in
/// seconds is `bytes_ahead / file_bitrate`. Returns 0 when the playhead is
/// past the downloaded extent (or when either bound is zero).
#[must_use]
#[allow(
    clippy::cast_precision_loss,    // file sizes < 2^52 in practice
    clippy::cast_possible_truncation, // position_bytes ≤ file_size which fits u64
    clippy::cast_sign_loss           // position_s clamped to ≥ 0 before multiply
)]
pub fn pieces_ahead_seconds(
    bytes_downloaded: u64,
    position_s: f64,
    file_size_bytes: u64,
    duration_s: f64,
) -> f64 {
    if file_size_bytes == 0 || duration_s <= 0.0 {
        return 0.0;
    }
    let bytes_per_s = file_size_bytes as f64 / duration_s;
    if bytes_per_s <= 0.0 {
        return 0.0;
    }
    let position_bytes = (position_s.max(0.0) * bytes_per_s).round() as u64;
    let bytes_ahead = bytes_downloaded.saturating_sub(position_bytes);
    bytes_ahead as f64 / bytes_per_s
}

/// Default recompute cadence per PRD §F-014.
#[must_use]
pub fn default_recompute_interval() -> Duration {
    Duration::from_secs_f64(RECOMPUTE_INTERVAL_S)
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,           // tests deliberately assert exact sentinel values
    clippy::cast_precision_loss  // test inputs are small literals
)]
mod tests {
    use super::*;

    #[test]
    fn safe_when_download_outruns_playback_with_margin() {
        // File: 100 MB, duration 1h (3600s), playhead at 30s → 3570s left.
        // dl_rate 10 MB/s, 100 MB - 30 MB downloaded = 70 MB remaining →
        // ttdl = 7s, headroom = 3540s. 7 <= 3540 → SAFE.
        let inputs = SchedulerInputs {
            file_size_bytes: 100_000_000,
            bytes_downloaded: 30_000_000,
            dl_rate_bytes_per_s: 10_000_000.0,
            duration_s: 3600.0,
            position_s: 30.0,
            pieces_ahead_seconds: SAFETY_MARGIN_S * 2.0,
        };
        assert_eq!(compute_state(&inputs), BufferState::Safe);
    }

    #[test]
    fn needs_prebuffer_when_rate_too_slow() {
        // File: 1 GB, duration 1h (3600s), dl_rate 100 KB/s → ttdl = 10000s,
        // headroom = 3570s. NEEDS_PREBUFFER with deficit ≈ 6430s.
        let inputs = SchedulerInputs {
            file_size_bytes: 1_000_000_000,
            bytes_downloaded: 0,
            dl_rate_bytes_per_s: 100_000.0,
            duration_s: 3600.0,
            position_s: 0.0,
            pieces_ahead_seconds: SAFETY_MARGIN_S,
        };
        let state = compute_state(&inputs);
        let BufferState::NeedsPrebuffer {
            required_prebuffer_s,
        } = state
        else {
            panic!("expected NeedsPrebuffer, got {state:?}");
        };
        // Deficit ≈ 6430s, clearly above PREBUFFER_TARGET_S floor.
        assert!(required_prebuffer_s > PREBUFFER_TARGET_S);
        assert!((required_prebuffer_s - (10_000.0 - 3570.0)).abs() < 1.0);
    }

    #[test]
    fn needs_prebuffer_floor_is_prebuffer_target() {
        // File: 100 MB, duration 60s, dl_rate just barely too slow.
        // remaining = 100 MB, ttdl = 100/dl_rate, headroom = 30s.
        // We want deficit < PREBUFFER_TARGET_S → required = PREBUFFER_TARGET_S.
        // Pick dl_rate so ttdl = 35s → deficit = 5s, < 15.
        let dl_rate = 100_000_000.0 / 35.0;
        let inputs = SchedulerInputs {
            file_size_bytes: 100_000_000,
            bytes_downloaded: 0,
            dl_rate_bytes_per_s: dl_rate,
            duration_s: 60.0,
            position_s: 0.0,
            pieces_ahead_seconds: SAFETY_MARGIN_S,
        };
        let state = compute_state(&inputs);
        let BufferState::NeedsPrebuffer {
            required_prebuffer_s,
        } = state
        else {
            panic!("expected NeedsPrebuffer, got {state:?}");
        };
        assert!((required_prebuffer_s - PREBUFFER_TARGET_S).abs() < f64::EPSILON);
    }

    #[test]
    fn fully_downloaded_is_safe_even_when_dl_rate_is_zero() {
        // Warm cache: bytes_downloaded == file_size, dl_rate idles to 0.
        // PRD math: remaining = 0 → ttdl = 0 → SAFE.
        let inputs = SchedulerInputs {
            file_size_bytes: 100_000_000,
            bytes_downloaded: 100_000_000,
            dl_rate_bytes_per_s: 0.0,
            duration_s: 3600.0,
            position_s: 0.0,
            pieces_ahead_seconds: SAFETY_MARGIN_S * 4.0,
        };
        assert_eq!(compute_state(&inputs), BufferState::Safe);
    }

    #[test]
    fn dl_rate_zero_collapses_to_prebuffer_target() {
        // dl_rate = 0 → ttdl = +inf, deficit = +inf. We cap at
        // PREBUFFER_TARGET_S so the UI gets a finite progress.
        let inputs = SchedulerInputs {
            file_size_bytes: 1_000_000_000,
            bytes_downloaded: 0,
            dl_rate_bytes_per_s: 0.0,
            duration_s: 3600.0,
            position_s: 0.0,
            pieces_ahead_seconds: SAFETY_MARGIN_S,
        };
        let state = compute_state(&inputs);
        let BufferState::NeedsPrebuffer {
            required_prebuffer_s,
        } = state
        else {
            panic!("expected NeedsPrebuffer (cold start), got {state:?}");
        };
        assert!((required_prebuffer_s - PREBUFFER_TARGET_S).abs() < f64::EPSILON);
    }

    #[test]
    fn rebuffer_when_pieces_ahead_under_half_safety_margin() {
        // SAFE-shaped inputs but pieces_ahead is too low → REBUFFER.
        // Position > 0 so initial-play guard doesn't suppress.
        let inputs = SchedulerInputs {
            file_size_bytes: 100_000_000,
            bytes_downloaded: 50_000_000,
            dl_rate_bytes_per_s: 10_000_000.0,
            duration_s: 3600.0,
            position_s: 60.0,
            pieces_ahead_seconds: SAFETY_MARGIN_S * 0.4,
        };
        assert_eq!(compute_state(&inputs), BufferState::Rebuffer);
    }

    #[test]
    fn rebuffer_suppressed_at_position_zero() {
        // pieces_ahead == 0 at position 0 is the cold-start situation; we
        // emit NEEDS_PREBUFFER not REBUFFER so the UI's "Buffering for
        // smooth playback" overlay correctly reads as "initial buffer"
        // not "stream stalled mid-playback".
        let inputs = SchedulerInputs {
            file_size_bytes: 1_000_000_000,
            bytes_downloaded: 0,
            dl_rate_bytes_per_s: 0.0,
            duration_s: 3600.0,
            position_s: 0.0,
            pieces_ahead_seconds: 0.0,
        };
        let state = compute_state(&inputs);
        assert!(matches!(state, BufferState::NeedsPrebuffer { .. }));
    }

    #[test]
    fn rebuffer_kicks_in_mid_playback_even_when_math_would_say_safe() {
        // Half the file already downloaded, dl_rate fine, but the player
        // outran the contiguous lead (e.g. seek into a not-yet-downloaded
        // region). REBUFFER takes precedence per PRD §F-014.
        let inputs = SchedulerInputs {
            file_size_bytes: 100_000_000,
            bytes_downloaded: 50_000_000,
            dl_rate_bytes_per_s: 100_000_000.0, // saturated link
            duration_s: 3600.0,
            position_s: 1800.0,
            pieces_ahead_seconds: 0.0,
        };
        assert_eq!(compute_state(&inputs), BufferState::Rebuffer);
    }

    #[test]
    fn safe_at_boundary_when_headroom_exactly_matches_ttdl() {
        // time_to_play_remaining = 3600 - 30 = 3570s.
        // headroom = 3570 - SAFETY_MARGIN_S (30s) = 3540s.
        // Set dl_rate so ttdl = 3540s exactly: dl_rate = remaining / 3540.
        let file_size = 100_000_000u64;
        let remaining = file_size; // bytes_downloaded == 0
        let dl_rate = remaining as f64 / 3540.0;
        let inputs = SchedulerInputs {
            file_size_bytes: file_size,
            bytes_downloaded: 0,
            dl_rate_bytes_per_s: dl_rate,
            duration_s: 3600.0,
            position_s: 30.0,
            pieces_ahead_seconds: SAFETY_MARGIN_S,
        };
        // The PRD says `<=` for SAFE, so equality is SAFE.
        assert_eq!(compute_state(&inputs), BufferState::Safe);
    }

    #[test]
    fn buffer_state_pauses_playback_only_outside_safe() {
        assert!(!BufferState::Safe.pauses_playback());
        assert!(BufferState::NeedsPrebuffer {
            required_prebuffer_s: 5.0,
        }
        .pauses_playback());
        assert!(BufferState::Rebuffer.pauses_playback());
    }

    #[test]
    fn pieces_ahead_seconds_basic() {
        // 100 MB file at 60s duration → bitrate ≈ 1.67 MB/s.
        // Downloaded 50 MB, playhead at 10s → 50MB - 16.66MB ≈ 33.33 MB
        // ahead → ≈ 20s.
        let lead = pieces_ahead_seconds(50_000_000, 10.0, 100_000_000, 60.0);
        assert!((lead - 20.0).abs() < 0.5);
    }

    #[test]
    fn pieces_ahead_seconds_clamps_to_zero_when_playhead_outruns() {
        let lead = pieces_ahead_seconds(10_000_000, 90.0, 100_000_000, 60.0);
        assert_eq!(lead, 0.0);
    }

    #[test]
    fn pieces_ahead_seconds_handles_degenerate_inputs() {
        assert_eq!(pieces_ahead_seconds(100, 0.0, 0, 60.0), 0.0);
        assert_eq!(pieces_ahead_seconds(100, 0.0, 100, 0.0), 0.0);
    }

    #[test]
    fn rolling_rate_average_is_zero_when_empty() {
        let r = RollingRate::new();
        assert_eq!(r.average_bps(), 0.0);
        assert!(r.is_empty());
    }

    #[test]
    fn rolling_rate_drops_samples_outside_window() {
        let mut r = RollingRate::with_window(Duration::from_secs(5));
        let t0 = Instant::now();
        r.push(t0, 100.0);
        r.push(t0 + Duration::from_secs(1), 200.0);
        r.push(t0 + Duration::from_secs(2), 300.0);
        assert_eq!(r.len(), 3);
        assert!((r.average_bps() - 200.0).abs() < f64::EPSILON);

        // Push a sample 10s after t0 → first three should be evicted.
        r.push(t0 + Duration::from_secs(10), 1000.0);
        assert_eq!(r.len(), 1);
        assert!((r.average_bps() - 1000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rolling_rate_clamps_negative_samples() {
        // Negative bytes/s is meaningless but should not break the average.
        let mut r = RollingRate::new();
        let t0 = Instant::now();
        r.push(t0, -100.0);
        r.push(t0 + Duration::from_millis(100), 200.0);
        let avg = r.average_bps();
        // Clamped negative becomes 0, so avg = (0 + 200) / 2 = 100.
        assert!((avg - 100.0).abs() < f64::EPSILON);
    }
}
