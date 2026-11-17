//! Master clock, drift classification and the alignment delta convention.
//!
//! ## Master clock
//!
//! The timeline is advanced by the number of frames the cpal output callback
//! consumes (the *mixed* output), never by a single layer's PTS. The callback
//! publishes an anchor `(position_us, monotonic_nanos)` where `position_us` is
//! the timeline position of the first sample of the buffer it just filled and
//! `monotonic_nanos` is the predicted instant at which that sample reaches the
//! DAC (the cpal device timestamp minus callback timestamp latency).
//!
//! The UI thread interpolates from that anchor with [`std::time::Instant`],
//! which gives smooth sub-millisecond presentation even between audio callbacks.
//!
//! ## Drift
//!
//! `drift = layer_actual_pts - target_pts` where
//! `target_pts = master_clock_us + layer.offset_us`. Classification:
//!
//! | range             | action                                      |
//! |-------------------|---------------------------------------------|
//! | `< 5 ms`          | ignore                                      |
//! | `5..=100 ms`      | rate nudge clamped to `0.995..=1.005`       |
//! | `> 100 ms`        | exact seek                                  |
//!
//! A [`DriftController`] adds hysteresis so a layer hovering at a threshold
//! does not oscillate between nudging and seeking.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Drift below this is ignored.
pub const DRIFT_IGNORE_US: i64 = 5_000;
/// Drift at or above this triggers an exact seek.
pub const DRIFT_SEEK_US: i64 = 100_000;
/// Minimum playback-rate ratio used to absorb drift.
pub const MIN_RATE: f32 = 0.995;
/// Maximum playback-rate ratio used to absorb drift.
pub const MAX_RATE: f32 = 1.005;
/// Time horizon over which a nudge is meant to remove the drift.
pub const NUDGE_HORIZON_US: i64 = 2_000_000;
/// Leave "seek mode" only once drift has fallen below this.
pub const SEEK_EXIT_US: i64 = 60_000;
/// Leave "nudge mode" only once drift has fallen below this.
pub const NUDGE_EXIT_US: i64 = 3_000;

/// Process-wide monotonic epoch.
fn epoch() -> &'static Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now)
}

/// Nanoseconds since the process-wide monotonic epoch.
pub fn now_nanos() -> u64 {
    epoch().elapsed().as_nanos().min(u64::MAX as u128) as u64
}

/// Convert a [`Duration`] into microseconds saturating at `i64::MAX`.
pub fn duration_us(d: Duration) -> i64 {
    d.as_micros().min(i64::MAX as u128) as i64
}

/// Timeline target for a layer, in the layer's own media time.
///
/// This is *the* sign convention used by every other component:
/// `target = master + offset`.
pub fn target_pts_us(master_clock_us: i64, offset_us: i64) -> i64 {
    master_clock_us.saturating_add(offset_us)
}

/// Drift of a layer relative to its target.
pub fn drift_us(layer_actual_pts_us: i64, master_clock_us: i64, offset_us: i64) -> i64 {
    layer_actual_pts_us.saturating_sub(target_pts_us(master_clock_us, offset_us))
}

/// Playback-rate ratio that absorbs `drift` over [`NUDGE_HORIZON_US`].
///
/// A layer that is *ahead* (positive drift) must play slightly slower, hence
/// the minus sign. The result is clamped to `0.995..=1.005`.
pub fn nudge_rate(drift_us: i64) -> f32 {
    let ratio = 1.0 - (drift_us as f32) / (NUDGE_HORIZON_US as f32);
    ratio.clamp(MIN_RATE, MAX_RATE)
}

/// Classification of a drift measurement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DriftAction {
    /// Close enough, do nothing.
    Ignore,
    /// Absorb with a fractional rate in `0.995..=1.005`.
    Nudge(f32),
    /// Perform an exact seek to `target`.
    Seek,
}

/// Classify a single drift measurement without hysteresis.
pub fn classify_drift(drift_us: i64) -> DriftAction {
    let ad = drift_us.abs();
    if ad < DRIFT_IGNORE_US {
        DriftAction::Ignore
    } else if ad <= DRIFT_SEEK_US {
        DriftAction::Nudge(nudge_rate(drift_us))
    } else {
        DriftAction::Seek
    }
}

/// Stateful drift controller implementing the hysteresis described in the
/// spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DriftMode {
    /// Nominal playback.
    #[default]
    Nominal,
    /// Absorbing small drift with a rate nudge.
    Nudging,
    /// A seek has been issued and the controller waits for it to land.
    Seeking,
}

/// Drift controller with hysteresis.
#[derive(Debug, Clone, Copy, Default)]
pub struct DriftController {
    mode: DriftMode,
}

impl DriftController {
    /// Current mode.
    pub fn mode(&self) -> DriftMode {
        self.mode
    }

    /// Feed a drift measurement and get the action to take.
    pub fn update(&mut self, drift_us: i64) -> DriftAction {
        let ad = drift_us.abs();
        match self.mode {
            DriftMode::Nominal => {
                if ad > DRIFT_SEEK_US {
                    self.mode = DriftMode::Seeking;
                    DriftAction::Seek
                } else if ad >= DRIFT_IGNORE_US {
                    self.mode = DriftMode::Nudging;
                    DriftAction::Nudge(nudge_rate(drift_us))
                } else {
                    DriftAction::Ignore
                }
            }
            DriftMode::Nudging => {
                if ad > DRIFT_SEEK_US {
                    self.mode = DriftMode::Seeking;
                    DriftAction::Seek
                } else if ad <= NUDGE_EXIT_US {
                    self.mode = DriftMode::Nominal;
                    DriftAction::Ignore
                } else {
                    DriftAction::Nudge(nudge_rate(drift_us))
                }
            }
            DriftMode::Seeking => {
                if ad <= SEEK_EXIT_US {
                    self.mode = DriftMode::Nominal;
                    DriftAction::Ignore
                } else {
                    // Stay in seek mode: the caller may retry, but a still
                    // large drift must not decay into a nudge.
                    DriftAction::Seek
                }
            }
        }
    }

    /// Reset to nominal (e.g. after a user seek).
    pub fn reset(&mut self) {
        self.mode = DriftMode::Nominal;
    }
}

/// Alignment Studio delta: the offset to assign to layer B so that its capture
/// shows the same event as layer A's capture.
///
/// `offset_B = offset_A + (pts_B - pts_A)`.
pub fn alignment_offset(offset_a_us: i64, pts_a_us: i64, pts_b_us: i64) -> i64 {
    offset_a_us
        .saturating_add(pts_b_us)
        .saturating_sub(pts_a_us)
}

/// The delta shown in the UI, in microseconds (B minus A).
pub fn alignment_delta(pts_a_us: i64, pts_b_us: i64) -> i64 {
    pts_b_us.saturating_sub(pts_a_us)
}

/// Shared master clock. Safe to read from the UI thread and write from the
/// audio callback.
#[derive(Debug)]
pub struct MasterClock {
    anchor_pos_us: AtomicI64,
    anchor_nanos: AtomicU64,
    playing: AtomicBool,
    audio_driven: AtomicBool,
    epoch: AtomicU64,
    /// Fallback (Instant-driven) anchor, used when no audio device exists.
    fallback_anchor: std::sync::Mutex<Option<(i64, Instant)>>,
}

impl Default for MasterClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MasterClock {
    /// Create a clock at position zero.
    pub fn new() -> Self {
        Self {
            anchor_pos_us: AtomicI64::new(0),
            anchor_nanos: AtomicU64::new(now_nanos()),
            playing: AtomicBool::new(false),
            audio_driven: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            fallback_anchor: std::sync::Mutex::new(None),
        }
    }

    /// Current interpolated position in microseconds.
    pub fn position_us(&self) -> i64 {
        if !self.playing.load(Ordering::Relaxed) {
            return self.anchor_pos_us.load(Ordering::Relaxed);
        }
        if self.audio_driven.load(Ordering::Relaxed) {
            let pos = self.anchor_pos_us.load(Ordering::Relaxed);
            let at = self.anchor_nanos.load(Ordering::Relaxed);
            let now = now_nanos();
            if now >= at {
                pos.saturating_add(((now - at) / 1_000).min(i64::MAX as u64) as i64)
            } else {
                pos
            }
        } else {
            let guard = match self.fallback_anchor.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            match *guard {
                Some((pos, start)) => {
                    pos.saturating_add(duration_us(start.elapsed()))
                }
                None => self.anchor_pos_us.load(Ordering::Relaxed),
            }
        }
    }

    /// True when the clock is driven by the audio callback.
    pub fn is_audio_driven(&self) -> bool {
        self.audio_driven.load(Ordering::Relaxed)
    }

    /// Mark the clock as audio driven (or fall back to Instant).
    pub fn set_audio_driven(&self, value: bool) {
        self.audio_driven.store(value, Ordering::Relaxed);
    }

    /// Jump to `pos_us` (also used by scrubbing and seeking).
    pub fn set_position_us(&self, pos_us: i64) {
        self.anchor_pos_us.store(pos_us, Ordering::Relaxed);
        self.anchor_nanos.store(now_nanos(), Ordering::Relaxed);
        if !self.audio_driven.load(Ordering::Relaxed)
            && let Ok(mut guard) = self.fallback_anchor.lock()
        {
            *guard = Some((pos_us, Instant::now()));
        }
    }

    /// Publish an audio anchor: the timeline position of the first sample of
    /// the buffer just rendered, and the monotonic instant at which it will be
    /// heard (device timestamp minus callback timestamp already applied).
    ///
    /// Returns the position at the end of the buffer.
    pub fn publish_audio_anchor(&self, start_pos_us: i64, playback_latency_us: i64, frames: u32, sample_rate: u32) {
        let at = now_nanos().saturating_add(playback_latency_us.max(0) as u64 * 1_000);
        self.anchor_pos_us.store(start_pos_us, Ordering::Relaxed);
        self.anchor_nanos.store(at, Ordering::Relaxed);
        let _ = frames;
        let _ = sample_rate;
    }

    /// Position at the end of a buffer of `frames` at `sample_rate`.
    pub fn advance_frames(pos_us: i64, frames: u32, sample_rate: u32) -> i64 {
        if sample_rate == 0 {
            return pos_us;
        }
        pos_us.saturating_add((frames as i64 * 1_000_000) / sample_rate as i64)
    }

    /// Whether the transport is playing.
    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Start or stop the transport. Starting re-anchors to `Instant::now()`.
    pub fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Relaxed);
        if playing {
            self.anchor_nanos.store(now_nanos(), Ordering::Relaxed);
            if let Ok(mut guard) = self.fallback_anchor.lock() {
                *guard = Some((self.anchor_pos_us.load(Ordering::Relaxed), Instant::now()));
            }
        }
    }

    /// Increment and return the seek epoch. Cookies tagged with an older epoch
    /// must be discarded by consumers (queued frames and stale audio).
    pub fn bump_epoch(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Current seek epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_pts_applies_positive_offset() {
        assert_eq!(target_pts_us(1_000_000, 1_500_000), 2_500_000);
        assert_eq!(target_pts_us(0, -500_000), -500_000);
    }

    #[test]
    fn drift_is_actual_minus_target() {
        // Master at 2s, offset +1.5s -> target 3.5s.
        assert_eq!(drift_us(3_600_000, 2_000_000, 1_500_000), 100_000);
        assert_eq!(drift_us(3_400_000, 2_000_000, 1_500_000), -100_000);
    }

    #[test]
    fn classification_thresholds_match_spec() {
        assert_eq!(classify_drift(0), DriftAction::Ignore);
        assert_eq!(classify_drift(4_999), DriftAction::Ignore);
        assert_eq!(classify_drift(-4_999), DriftAction::Ignore);

        match classify_drift(5_000) {
            DriftAction::Nudge(r) => assert!(r < 1.0 && r >= MIN_RATE),
            other => panic!("expected nudge at 5 ms, got {other:?}"),
        }
        match classify_drift(-5_000) {
            DriftAction::Nudge(r) => assert!(r > 1.0 && r <= MAX_RATE),
            other => panic!("expected nudge at -5 ms, got {other:?}"),
        }
        match classify_drift(100_000) {
            DriftAction::Nudge(r) => assert!((r - MIN_RATE).abs() < 1e-6),
            other => panic!("expected nudge at 100 ms, got {other:?}"),
        }
        assert_eq!(classify_drift(100_001), DriftAction::Seek);
        assert_eq!(classify_drift(-100_001), DriftAction::Seek);
    }

    #[test]
    fn nudge_rate_is_clamped_and_signed() {
        assert_eq!(nudge_rate(0), 1.0);
        assert!((nudge_rate(50_000) - 0.975).abs() < 1e-6);
        assert_eq!(nudge_rate(10_000_000), MIN_RATE);
        assert_eq!(nudge_rate(-10_000_000), MAX_RATE);
    }

    #[test]
    fn controller_hysteresis_avoids_flapping() {
        let mut c = DriftController::default();
        assert_eq!(c.update(4_000), DriftAction::Ignore);
        assert!(matches!(c.update(6_000), DriftAction::Nudge(_)));
        assert_eq!(c.mode(), DriftMode::Nudging);
        // 10ms would be "nudge" from Nominal, but the controller stays in
        // nudge mode until below 3ms -- still a nudge, not a mode change.
        assert!(matches!(c.update(10_000), DriftAction::Nudge(_)));
        // Falling below the exit threshold returns to Nominal.
        assert_eq!(c.update(2_000), DriftAction::Ignore);
        assert_eq!(c.mode(), DriftMode::Nominal);
    }

    #[test]
    fn controller_seek_hysteresis() {
        let mut c = DriftController::default();
        assert_eq!(c.update(150_000), DriftAction::Seek);
        assert_eq!(c.mode(), DriftMode::Seeking);
        // 80 ms would be a nudge from Nominal, but must not leave seek mode.
        assert_eq!(c.update(80_000), DriftAction::Seek);
        assert_eq!(c.update(59_000), DriftAction::Ignore);
        assert_eq!(c.mode(), DriftMode::Nominal);
    }

    #[test]
    fn alignment_delta_sign_matches_target_formula() {
        // A captured the event at pts 5.0s, B at 6.5s.
        let pts_a = 5_000_000;
        let pts_b = 6_500_000;
        let delta = alignment_delta(pts_a, pts_b);
        assert_eq!(delta, 1_500_000);

        // Applying it must make both layers show the same event at the same
        // timeline instant, whatever A's current offset is.
        let offset_a = 250_000;
        let offset_b_new = alignment_offset(offset_a, pts_a, pts_b);
        assert_eq!(offset_b_new, 1_750_000);

        // Choose the timeline instant where A shows pts_a:
        let master = pts_a - offset_a;
        assert_eq!(target_pts_us(master, offset_a), pts_a);
        assert_eq!(target_pts_us(master, offset_b_new), pts_b);
    }

    #[test]
    fn demo_offset_comes_out_positive_1500ms() {
        // Reference at 0: event at 4s. Reaction: event at 5.5s.
        let offset = alignment_offset(0, 4_000_000, 5_500_000);
        assert_eq!(offset, 1_500_000);
    }

    #[test]
    fn clock_interpolates_when_playing() {
        let clock = MasterClock::new();
        clock.set_position_us(1_000_000);
        clock.set_playing(true);
        let a = clock.position_us();
        std::thread::sleep(Duration::from_millis(6));
        let b = clock.position_us();
        assert!(b > a, "clock must advance while playing ({a} -> {b})");
        assert!(b - a >= 3_000, "expected at least 3 ms of movement");
        clock.set_playing(false);
        let c = clock.position_us();
        std::thread::sleep(Duration::from_millis(3));
        assert_eq!(clock.position_us(), c, "clock must freeze when paused");
    }

    #[test]
    fn clock_advance_frames_is_exact() {
        assert_eq!(MasterClock::advance_frames(0, 480, 48_000), 10_000);
        assert_eq!(MasterClock::advance_frames(1_000, 0, 48_000), 1_000);
        assert_eq!(MasterClock::advance_frames(1_000, 100, 0), 1_000);
    }

    #[test]
    fn epoch_increments_monotonically() {
        let clock = MasterClock::new();
        let e0 = clock.epoch();
        assert_eq!(clock.bump_epoch(), e0 + 1);
        assert_eq!(clock.bump_epoch(), e0 + 2);
    }
}
