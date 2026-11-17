//! Captures used by the Alignment Studio.
//!
//! A capture keeps the decoded YUV frame itself (not an RGBA conversion), so
//! the same planes can be uploaded to the GPU for the Onion and Wipe compares
//! at full quality and without a second decode. The studio keeps the last
//! [`MAX_CAPTURES`] captures per video.

use std::sync::Arc;

use crate::engine::hwaccel::ColorRange;
use crate::engine::video_frame::VideoFrame;

use super::renderer::{CompareKind, CompareUpload, FrameUpload};

/// How many captures are kept per video.
pub const MAX_CAPTURES: usize = 8;

/// One frozen frame from a video.
#[derive(Clone, Debug)]
pub struct Capture {
    /// Presentation timestamp of the frame, in microseconds.
    pub pts_us: i64,
    /// The decoded planes, shared with the track so no copy happens.
    pub frame: Arc<VideoFrame>,
    /// The colour range the frame was decoded in.
    pub range: ColorRange,
}

impl Capture {
    /// Build a capture from a presented frame's parts.
    pub fn new(pts_us: i64, frame: Arc<VideoFrame>, range: ColorRange) -> Self {
        Self {
            pts_us,
            frame,
            range,
        }
    }

    /// The pts in milliseconds, which is what the studio displays.
    pub fn pts_ms(&self) -> i64 {
        self.pts_us / 1_000
    }

    /// As a GPU upload.
    pub fn upload(&self) -> FrameUpload {
        FrameUpload {
            frame: Arc::clone(&self.frame),
            range: self.range,
        }
    }
}

/// The rolling capture history of one video.
#[derive(Clone, Debug, Default)]
pub struct CaptureRing {
    /// Oldest first, newest last; at most [`MAX_CAPTURES`] entries.
    entries: Vec<Capture>,
}

impl CaptureRing {
    /// An empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a capture, dropping the oldest once the ring is full.
    pub fn push(&mut self, capture: Capture) {
        self.entries.push(capture);
        if self.entries.len() > MAX_CAPTURES {
            let overflow = self.entries.len() - MAX_CAPTURES;
            self.entries.drain(0..overflow);
        }
    }

    /// Number of stored captures.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing was captured yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The newest capture.
    pub fn newest(&self) -> Option<&Capture> {
        self.entries.last()
    }

    /// The oldest capture still stored.
    pub fn oldest(&self) -> Option<&Capture> {
        self.entries.first()
    }

    /// All captures, newest first (the order the studio lists them in).
    pub fn newest_first(&self) -> Vec<&Capture> {
        self.entries.iter().rev().collect()
    }

    /// Look one up by pts.
    pub fn find(&self, pts_us: i64) -> Option<&Capture> {
        self.entries.iter().find(|c| c.pts_us == pts_us)
    }

    /// Forget everything (used when a layer's media changes).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// The pair being compared in the studio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StudioMode {
    /// Blended overlay.
    #[default]
    Onion,
    /// Split screen with an accent divider.
    Wipe,
}

impl StudioMode {
    /// Both modes, in tab order.
    pub const ALL: [StudioMode; 2] = [StudioMode::Onion, StudioMode::Wipe];

    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            StudioMode::Onion => "Onion",
            StudioMode::Wipe => "Wipe",
        }
    }

    /// Convert to the renderer's enum.
    pub fn compare_kind(self) -> CompareKind {
        match self {
            StudioMode::Onion => CompareKind::Onion,
            StudioMode::Wipe => CompareKind::Wipe,
        }
    }
}

/// One side of the comparison: a captured frame plus the crop it is shown with.
#[derive(Clone, Debug)]
pub struct StudioSide {
    /// The capture.
    pub capture: Capture,
    /// The crop the layer currently has (the studio shows exactly what the
    /// compositor shows).
    pub crop: crate::model::CropRect,
    /// Layer id the capture came from, used as a GPU cache key.
    pub layer_id: u64,
}

/// A fully configured comparison ready to hand to the compositor.
#[derive(Debug, Clone)]
pub struct Comparison {
    /// Layer A's upload.
    pub a: FrameUpload,
    /// Layer A's crop.
    pub a_crop: crate::model::CropRect,
    /// Layer B's side.
    pub compare: CompareUpload,
    /// Onion / Wipe.
    pub mode: StudioMode,
    /// Mix or split position, `0..1`.
    pub position: f32,
}

impl Comparison {
    /// Build a comparison from two sides.
    pub fn new(a: StudioSide, b: StudioSide, mode: StudioMode, position: f32) -> Self {
        Self {
            a: a.capture.upload(),
            a_crop: a.crop,
            compare: CompareUpload {
                key: b.layer_id,
                frame: b.capture.upload(),
                crop: b.crop,
                t: position.clamp(0.0, 1.0),
                kind: mode.compare_kind(),
            },
            mode,
            position: position.clamp(0.0, 1.0),
        }
    }

    /// The offset delta the studio suggests applying, in microseconds.
    ///
    /// With the locked sign convention, a positive `offset_us` shows the
    /// layer *later*, so aligning B to A means
    /// `offset_B = offset_A + (pts_B - pts_A)`.
    pub fn suggested_offset_b(&self, offset_a_us: i64, pts_a_us: i64, pts_b_us: i64) -> i64 {
        crate::engine::sync_clock::alignment_offset(offset_a_us, pts_a_us, pts_b_us)
    }
}

/// Wipe positions as a percentage, clamped and rounded for display.
pub fn wipe_percent(position: f32) -> i32 {
    (position.clamp(0.0, 1.0) * 100.0).round() as i32
}

/// Move a wipe position by `delta_pct` percent, clamped to `0..=100`.
pub fn nudge_wipe(position: f32, delta_pct: i32) -> f32 {
    let pct = (wipe_percent(position) + delta_pct).clamp(0, 100);
    pct as f32 / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::video_frame::{PixelFormat, PlaneBuf};

    fn frame(pts_us: i64) -> Arc<VideoFrame> {
        Arc::new(VideoFrame {
            format: PixelFormat::Yuv420p,
            width: 4,
            height: 4,
            planes: [
                PlaneBuf::owned(16),
                PlaneBuf::owned(4),
                PlaneBuf::owned(4),
            ],
            strides: [4, 2, 2],
            pts_us,
        })
    }

    fn capture(pts_us: i64) -> Capture {
        Capture::new(pts_us, frame(pts_us), ColorRange::Limited)
    }

    #[test]
    fn ring_keeps_only_eight() {
        let mut ring = CaptureRing::new();
        for i in 0..12 {
            ring.push(capture(i * 1_000));
        }
        assert_eq!(ring.len(), MAX_CAPTURES);
        assert_eq!(ring.oldest().unwrap().pts_us, 4_000);
        assert_eq!(ring.newest().unwrap().pts_us, 11_000);
    }

    #[test]
    fn ring_orders_newest_first() {
        let mut ring = CaptureRing::new();
        ring.push(capture(100));
        ring.push(capture(200));
        let listed: Vec<i64> = ring.newest_first().iter().map(|c| c.pts_us).collect();
        assert_eq!(listed, vec![200, 100]);
    }

    #[test]
    fn ring_finds_by_pts() {
        let mut ring = CaptureRing::new();
        ring.push(capture(500));
        assert!(ring.find(500).is_some());
        assert!(ring.find(501).is_none());
        ring.clear();
        assert!(ring.is_empty());
        assert!(ring.newest().is_none());
    }

    #[test]
    fn capture_reports_milliseconds() {
        let c = capture(1_500_000);
        assert_eq!(c.pts_ms(), 1_500);
    }

    #[test]
    fn wipe_nudge_clamps() {
        assert!((nudge_wipe(0.5, 10) - 0.6).abs() < 1e-6);
        assert!((nudge_wipe(0.5, -10) - 0.4).abs() < 1e-6);
        assert_eq!(nudge_wipe(0.99, 50), 1.0);
        assert_eq!(nudge_wipe(0.01, -50), 0.0);
        assert_eq!(wipe_percent(0.455), 46);
    }

    #[test]
    fn comparison_reports_delta_and_offset() {
        let a = StudioSide {
            capture: capture(1_000_000),
            crop: crate::model::CropRect::none(),
            layer_id: 1,
        };
        let b = StudioSide {
            capture: capture(2_500_000),
            crop: crate::model::CropRect::none(),
            layer_id: 2,
        };
        let cmp = Comparison::new(a, b, StudioMode::Wipe, 0.5);
        assert_eq!(cmp.mode, StudioMode::Wipe);
        assert_eq!(cmp.position, 0.5);
        // delta = pts_B - pts_A = +1500 ms, and with offset_A = 0 the new
        // offset for B is +1500 ms.
        assert_eq!(cmp.suggested_offset_b(0, 1_000_000, 2_500_000), 1_500_000);
        // A already-offset B moves relative to its current offset.
        assert_eq!(
            cmp.suggested_offset_b(-1_000_000, 1_000_000, 2_500_000),
            500_000
        );
    }

    #[test]
    fn studio_modes_map_to_render_kinds() {
        assert_eq!(StudioMode::Onion.compare_kind(), CompareKind::Onion);
        assert_eq!(StudioMode::Wipe.compare_kind(), CompareKind::Wipe);
        assert_eq!(StudioMode::Onion.label(), "Onion");
        assert_eq!(StudioMode::Wipe.label(), "Wipe");
    }
}
