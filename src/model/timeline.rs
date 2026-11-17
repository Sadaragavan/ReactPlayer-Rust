//! The union timeline and the sync arithmetic that sits on top of it.
//!
//! Sign convention, fixed here and used everywhere else:
//!
//! ```text
//! target_pts_us = master_clock_us + layer.offset_us
//! layer start   = -offset_us
//! layer end     = duration_us - offset_us
//! ```
//!
//! A positive offset means the layer shows *later* material of its own file at
//! a given timeline time. The union timeline starts at the earliest layer start
//! (which may be negative) and the UI displays time relative to it.

use super::session::Layer;

/// Under this much drift, do nothing: the frame on screen is already the right
/// one and nudging would only add jitter.
pub const DRIFT_IGNORE_US: i64 = 5_000;
/// Above this much drift, absorb it with a rate nudge rather than a seek.
pub const DRIFT_NUDGE_US: i64 = 100_000;
/// The nudge is clamped to +-0.5 %, inaudible and invisible but enough to close
/// a 100 ms gap in 20 s.
pub const NUDGE_MIN_RATE: f64 = 0.995;
pub const NUDGE_MAX_RATE: f64 = 1.005;
/// After a seek, wait until drift falls below this before considering another.
pub const SEEK_HYSTERESIS_US: i64 = 40_000;

/// One layer's footprint on the union timeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start_us: i64,
    pub end_us: i64,
}

impl Span {
    pub fn duration_us(self) -> i64 {
        (self.end_us - self.start_us).max(0)
    }

    pub fn contains(self, t_us: i64) -> bool {
        t_us >= self.start_us && t_us < self.end_us
    }

    /// Where `t` sits inside the span, clamped to its ends.
    pub fn local_us(self, t_us: i64) -> i64 {
        t_us.clamp(self.start_us, self.end_us) - self.start_us
    }
}


/// The whole timeline: the union of every layer's span.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Union {
    pub start_us: i64,
    pub end_us: i64,
}

impl Union {
    pub fn duration_us(self) -> i64 {
        (self.end_us - self.start_us).max(0)
    }

    pub fn is_empty(self) -> bool {
        self.end_us <= self.start_us
    }

    /// Time since the start of the union, which is what the UI shows.
    pub fn display_us(self, t_us: i64) -> i64 {
        t_us - self.start_us
    }

    /// The inverse of [`Union::display_us`].
    pub fn from_display_us(self, display_us: i64) -> i64 {
        self.start_us + display_us
    }

    /// Position on the scrubber, 0..1.
    pub fn fraction(self, t_us: i64) -> f32 {
        let total = self.duration_us();
        if total <= 0 {
            return 0.0;
        }
        (((t_us - self.start_us) as f32) / total as f32).clamp(0.0, 1.0)
    }

    /// Playback ends when every layer has run past its own end.
    pub fn is_finished(self, t_us: i64) -> bool {
        !self.is_empty() && t_us >= self.end_us
    }
}

/// A layer's span on the union timeline.
pub fn span_of(layer: &Layer) -> Span {
    Span {
        start_us: layer.start_us(),
        end_us: layer.end_us(),
    }
}

/// The union of every layer's span. An empty session has an empty timeline.
pub fn union_of(layers: &[Layer]) -> Union {
    if layers.is_empty() {
        return Union::default();
    }
    let mut start = i64::MAX;
    let mut end = i64::MIN;
    for layer in layers {
        start = start.min(layer.start_us());
        end = end.max(layer.end_us());
    }
    Union {
        start_us: start,
        end_us: end.max(start),
    }
}

/// Where a layer's own footage is relative to the requested target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Inside the footage: decode and present this timestamp.
    Inside(i64),
    /// Before the first frame: hold the first frame.
    Before,
    /// Past the last frame: hold the last frame.
    After,
}

/// The presentation timestamp a layer should show at master time `master_us`.
pub fn target_pts_us(master_us: i64, offset_us: i64) -> i64 {
    master_us + offset_us
}

/// Classify a layer's target against its own duration.
pub fn target_of(layer: &Layer, master_us: i64) -> Target {
    let target = target_pts_us(master_us, layer.offset_us);
    if target < 0 {
        Target::Before
    } else if target >= layer.duration_us.max(0) {
        Target::After
    } else {
        Target::Inside(target)
    }
}

/// Clamp a desired timestamp into a layer's footage.
pub fn clamp_pts_us(pts_us: i64, duration_us: i64) -> i64 {
    pts_us.clamp(0, duration_us.max(0).saturating_sub(1))
}

/// What to do about the gap between the frame on screen and the one the clock
/// asks for. `drift = presented_pts - target_pts`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DriftClass {
    /// Under 5 ms: the right frame is already up.
    Ignore,
    /// 5..=100 ms: absorb by nudging the rate (audio) or picking a frame
    /// (video). The factor is applied to the layer's playback rate.
    Nudge(f64),
    /// Over 100 ms: seek.
    Seek,
}

/// Classify drift and, for the nudge band, say which way to lean.
pub fn classify_drift(drift_us: i64) -> DriftClass {
    let magnitude = drift_us.abs();
    if magnitude < DRIFT_IGNORE_US {
        DriftClass::Ignore
    } else if magnitude <= DRIFT_NUDGE_US {
        DriftClass::Nudge(nudge_rate(drift_us))
    } else {
        DriftClass::Seek
    }
}

/// Rate factor for a drift inside the nudge band. Ahead of the clock (positive
/// drift) means play slower; behind means play faster. Clamped to +-0.5 %.
pub fn nudge_rate(drift_us: i64) -> f64 {
    if drift_us == 0 {
        return 1.0;
    }
    let scale = (drift_us.abs().min(DRIFT_NUDGE_US) as f64) / (DRIFT_NUDGE_US as f64);
    let delta = 0.005 * scale;
    if drift_us > 0 {
        (1.0 - delta).clamp(NUDGE_MIN_RATE, NUDGE_MAX_RATE)
    } else {
        (1.0 + delta).clamp(NUDGE_MIN_RATE, NUDGE_MAX_RATE)
    }
}

/// The offset that lines `other`'s capture up with `reference`'s: with A as the
/// reference and B the other, `offset_B = pts_B - pts_A`. Applying it makes
/// `target_pts(B) == pts_B` when the master clock reads `pts_A`.
pub fn alignment_delta_us(reference_pts_us: i64, other_pts_us: i64) -> i64 {
    other_pts_us - reference_pts_us
}

/// Step `steps` frames from `pts_us`, never leaving the footage.
pub fn step_pts_us(pts_us: i64, frame_us: i64, steps: i64, duration_us: i64) -> i64 {
    let next = pts_us.saturating_add(frame_us.max(1).saturating_mul(steps));
    clamp_pts_us(next, duration_us)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::geom::NormRect;
    use crate::model::session::{MediaSource, Role};

    fn layer(duration_us: i64, offset_us: i64) -> Layer {
        let mut l = Layer::new(
            "l",
            Role::Reaction,
            MediaSource::file("/tmp/l.mp4"),
            NormRect::FULL,
            0,
        );
        l.duration_us = duration_us;
        l.offset_us = offset_us;
        l
    }

    #[test]
    fn union_covers_every_layer() {
        // 10 s at offset 0, 12 s at offset +1.5 s: starts at -1.5 s, ends at 10.5 s.
        let layers = [layer(10_000_000, 0), layer(12_000_000, 1_500_000)];
        let union = union_of(&layers);
        assert_eq!(union.start_us, -1_500_000);
        assert_eq!(union.end_us, 10_500_000);
        assert_eq!(union.duration_us(), 12_000_000);
        assert_eq!(union.display_us(-1_500_000), 0);
        assert_eq!(union.from_display_us(0), -1_500_000);
        assert!(!union.is_finished(10_499_999));
        assert!(union.is_finished(10_500_000));
    }

    #[test]
    fn union_of_nothing_is_empty() {
        let union = union_of(&[]);
        assert!(union.is_empty());
        assert_eq!(union.duration_us(), 0);
        assert_eq!(union.fraction(123), 0.0);
        assert!(!union.is_finished(0));
    }

    #[test]
    fn target_is_clamped_per_layer() {
        let l = layer(10_000_000, 1_500_000);
        // Before the layer starts (-1.5 s) the target would be negative.
        assert_eq!(target_of(&l, -2_000_000), Target::Before);
        assert_eq!(target_of(&l, -1_500_000), Target::Inside(0));
        assert_eq!(target_of(&l, 0), Target::Inside(1_500_000));
        assert_eq!(target_of(&l, 8_500_000), Target::After);
        assert_eq!(clamp_pts_us(-10, 10_000_000), 0);
        assert_eq!(clamp_pts_us(99_000_000, 10_000_000), 9_999_999);
    }

    #[test]
    fn span_holds_first_and_last_frames() {
        let l = layer(4_000_000, 1_000_000);
        let span = span_of(&l);
        assert_eq!(span.start_us, -1_000_000);
        assert_eq!(span.end_us, 3_000_000);
        assert_eq!(span.local_us(-5_000_000), 0);
        assert_eq!(span.local_us(1_000_000), 2_000_000);
        assert_eq!(span.local_us(9_000_000), 4_000_000);
        assert!(span.contains(0));
        assert!(!span.contains(3_000_000));
    }

    #[test]
    fn drift_thresholds_are_exact() {
        assert_eq!(classify_drift(0), DriftClass::Ignore);
        assert_eq!(classify_drift(4_999), DriftClass::Ignore);
        assert_eq!(classify_drift(-4_999), DriftClass::Ignore);
        assert!(matches!(classify_drift(5_000), DriftClass::Nudge(_)));
        assert!(matches!(classify_drift(-5_000), DriftClass::Nudge(_)));
        assert!(matches!(classify_drift(100_000), DriftClass::Nudge(_)));
        assert_eq!(classify_drift(100_001), DriftClass::Seek);
        assert_eq!(classify_drift(-100_001), DriftClass::Seek);
    }

    #[test]
    fn nudge_rate_points_the_right_way_and_clamps() {
        // Ahead of the clock: slow down.
        assert!(nudge_rate(100_000) < 1.0);
        assert!((nudge_rate(100_000) - NUDGE_MIN_RATE).abs() < 1e-9);
        // Behind: speed up.
        assert!(nudge_rate(-100_000) > 1.0);
        assert!((nudge_rate(-100_000) - NUDGE_MAX_RATE).abs() < 1e-9);
        // Proportional in the middle of the band.
        let mid = nudge_rate(50_000);
        assert!(mid > NUDGE_MIN_RATE && mid < 1.0);
        assert_eq!(nudge_rate(0), 1.0);
        assert!((nudge_rate(10_000_000) - NUDGE_MIN_RATE).abs() < 1e-9);
    }

    #[test]
    fn alignment_delta_uses_the_target_convention() {
        // The same flash is at 2 s in A and at 3.5 s in B.
        let pts_a = 2_000_000;
        let pts_b = 3_500_000;
        let offset_b = alignment_delta_us(pts_a, pts_b);
        assert_eq!(offset_b, 1_500_000);
        // With the master clock on A's capture, B must show its own capture.
        assert_eq!(target_pts_us(pts_a, offset_b), pts_b);
        // And A stays where it is.
        assert_eq!(target_pts_us(pts_a, 0), pts_a);
        // The other way round the sign flips.
        assert_eq!(alignment_delta_us(pts_b, pts_a), -1_500_000);
    }

    #[test]
    fn stepping_stays_inside_the_footage() {
        assert_eq!(step_pts_us(1_000_000, 33_333, 1, 10_000_000), 1_033_333);
        assert_eq!(step_pts_us(0, 33_333, -1, 10_000_000), 0);
        assert_eq!(step_pts_us(9_990_000, 33_333, 10, 10_000_000), 9_999_999);
    }

    #[test]
    fn scrubber_fraction_is_relative_to_union_start() {
        let union = Union {
            start_us: -1_500_000,
            end_us: 10_500_000,
        };
        assert_eq!(union.fraction(-1_500_000), 0.0);
        assert_eq!(union.fraction(10_500_000), 1.0);
        assert!((union.fraction(4_500_000) - 0.5).abs() < 1e-6);
    }
}
