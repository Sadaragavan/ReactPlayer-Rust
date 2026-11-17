//! Union-timeline arithmetic.
//!
//! ## Sign convention (see README and the spec)
//!
//! `target_pts_us = master_clock_us + layer.offset_us`.
//!
//! * A **positive** offset means the layer shows *later* material of its own
//!   file at a given timeline time.
//! * A layer starts on the timeline at `-offset_us`.
//! * A layer ends on the timeline at `duration_us - offset_us`.
//!
//! Therefore the union of all layers is
//! `union_start = min(-offset_i)` and `union_end = max(duration_i - offset_i)`.
//! `union_start` may be negative; the UI displays times relative to it.

/// Union bounds of all layers, in timeline microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnionBounds {
    /// First timeline instant covered by any layer (may be negative).
    pub start_us: i64,
    /// Last timeline instant covered by any layer.
    pub end_us: i64,
}

impl UnionBounds {
    /// An empty union (both bounds zero).
    pub const EMPTY: UnionBounds = UnionBounds {
        start_us: 0,
        end_us: 0,
    };

    /// Duration of the union; never negative.
    pub fn duration_us(&self) -> i64 {
        (self.end_us - self.start_us).max(0)
    }

    /// True if the union has a positive duration.
    pub fn is_empty(&self) -> bool {
        self.end_us <= self.start_us
    }

    /// Progress in `0..1` for a timeline instant.
    pub fn progress(&self, t_us: i64) -> f32 {
        let d = self.duration_us();
        if d <= 0 {
            return 0.0;
        }
        (((t_us - self.start_us) as f32) / (d as f32)).clamp(0.0, 1.0)
    }
}

/// A layer's contribution to the union timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackSpan {
    /// `-offset_us`: the timeline instant where the layer's first frame sits.
    pub start_us: i64,
    /// `duration_us - offset_us`: where its last frame sits.
    pub end_us: i64,
}

impl TrackSpan {
    /// Build a span from a duration and a layer offset using the sign
    /// convention above.
    pub fn new(duration_us: i64, offset_us: i64) -> Self {
        let duration_us = duration_us.max(0);
        Self {
            start_us: -offset_us,
            end_us: duration_us - offset_us,
        }
    }

    /// Length of the span.
    pub fn duration_us(&self) -> i64 {
        (self.end_us - self.start_us).max(0)
    }

    /// True if `t_us` falls inside the span.
    pub fn contains(&self, t_us: i64) -> bool {
        t_us >= self.start_us && t_us <= self.end_us
    }
}

/// Compute the union bounds of a list of spans.
pub fn union_bounds(spans: &[TrackSpan]) -> UnionBounds {
    let mut out = UnionBounds::EMPTY;
    let mut first = true;
    for s in spans {
        if first {
            out.start_us = s.start_us;
            out.end_us = s.end_us;
            first = false;
        } else {
            out.start_us = out.start_us.min(s.start_us);
            out.end_us = out.end_us.max(s.end_us);
        }
    }
    out
}

/// Clamp a requested layer-local time into the layer's own `0..duration`
/// range. This is what "hold first frame before start / hold last frame after
/// end" resolves to in terms of media time.
pub fn clamp_track_time(t_us: i64, duration_us: i64) -> i64 {
    t_us.clamp(0, duration_us.max(0))
}

/// Media time that a layer should present at timeline instant `t_us`, given the
/// layer's offset and duration. The result is not clamped: callers decide
/// whether to clamp (`hold`) or treat out-of-range as inactive.
pub fn layer_source_time_us(t_us: i64, offset_us: i64) -> i64 {
    t_us + offset_us
}

/// True if the layer is "active" (inside its own span) at `t_us`.
pub fn layer_active(t_us: i64, offset_us: i64, duration_us: i64) -> bool {
    let s = layer_source_time_us(t_us, offset_us);
    s >= 0 && s <= duration_us
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_bounds_include_negative_start() {
        // Layer A: 10s long, offset 0  -> span [0, 10s]
        // Layer B: 8s long, offset +3s -> span [-3s, 5s]
        let a = TrackSpan::new(10_000_000, 0);
        let b = TrackSpan::new(8_000_000, 3_000_000);
        assert_eq!(a.start_us, 0);
        assert_eq!(a.end_us, 10_000_000);
        assert_eq!(b.start_us, -3_000_000);
        assert_eq!(b.end_us, 5_000_000);
        let u = union_bounds(&[a, b]);
        assert_eq!(u.start_us, -3_000_000);
        assert_eq!(u.end_us, 10_000_000);
        assert_eq!(u.duration_us(), 13_000_000);
    }

    #[test]
    fn union_bounds_of_empty_list_is_empty() {
        let u = union_bounds(&[]);
        assert!(u.is_empty());
        assert_eq!(u.duration_us(), 0);
    }

    #[test]
    fn negative_offset_shifts_span_forward() {
        let s = TrackSpan::new(5_000_000, -1_500_000);
        assert_eq!(s.start_us, 1_500_000);
        assert_eq!(s.end_us, 6_500_000);
        assert!(!s.contains(1_000_000));
        assert!(s.contains(2_000_000));
    }

    #[test]
    fn clamping_holds_first_and_last_frame() {
        assert_eq!(clamp_track_time(-5_000, 10_000), 0);
        assert_eq!(clamp_track_time(15_000, 10_000), 10_000);
        assert_eq!(clamp_track_time(5_000, 10_000), 5_000);
    }

    #[test]
    fn active_is_inclusive_at_both_ends() {
        assert!(layer_active(0, 0, 1_000));
        assert!(layer_active(1_000, 0, 1_000));
        assert!(!layer_active(1_001, 0, 1_000));
        assert!(!layer_active(-1, 0, 1_000));
    }

    #[test]
    fn progress_maps_relative_to_union_start() {
        let u = UnionBounds {
            start_us: -2_000_000,
            end_us: 2_000_000,
        };
        assert!((u.progress(-2_000_000) - 0.0).abs() < 1e-6);
        assert!((u.progress(0) - 0.5).abs() < 1e-6);
        assert!((u.progress(2_000_000) - 1.0).abs() < 1e-6);
        assert!((u.progress(9_000_000) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn demo_alignment_requires_positive_offset() {
        // The demo clip plants the same flash+beep 1500 ms later in the
        // reaction file, so aligning requires offset = +1_500_000 us.
        let source = TrackSpan::new(20_000_000, 0);
        let reaction = TrackSpan::new(20_000_000, 1_500_000);
        // The event at source t=5s happens at reaction media t=6.5s.
        let event_timeline = 5_000_000;
        assert_eq!(layer_source_time_us(event_timeline, source.start_us * -1), 5_000_000);
        assert_eq!(
            layer_source_time_us(event_timeline, 1_500_000),
            6_500_000,
            "reaction must show its 6.5 s material while source shows 5 s"
        );
        assert_eq!(reaction.start_us, -1_500_000);
    }
}
