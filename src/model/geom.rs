//! Geometry primitives that are stored in sessions and presets.
//!
//! Layer rectangles are stored **normalized** (`0..1` in canvas space) so that
//! a layout stays pixel-exact across window resizes and across aspect ratios.

use serde::{Deserialize, Serialize};

/// A rectangle in normalized canvas space (`0..1` on both axes).
///
/// `x`/`y` are the top-left corner, `w`/`h` the extent. The rectangle is not
/// clamped on construction -- the manipulator clamps it while editing -- but
/// [`NormalizedRect::sanitized`] enforces the invariants used by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormalizedRect {
    /// Left edge, `0..1`.
    pub x: f32,
    /// Top edge, `0..1`.
    pub y: f32,
    /// Width, `0..1`.
    pub w: f32,
    /// Height, `0..1`.
    pub h: f32,
}

impl Default for NormalizedRect {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }
}

impl NormalizedRect {
    /// Construct a rect.
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Full-canvas rect.
    pub fn full() -> Self {
        Self::default()
    }

    /// Left edge.
    pub fn left(&self) -> f32 {
        self.x
    }

    /// Right edge.
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// Top edge.
    pub fn top(&self) -> f32 {
        self.y
    }

    /// Bottom edge.
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Center on the x axis.
    pub fn center_x(&self) -> f32 {
        self.x + self.w * 0.5
    }

    /// Center on the y axis.
    pub fn center_y(&self) -> f32 {
        self.y + self.h * 0.5
    }

    /// Center point.
    pub fn center(&self) -> (f32, f32) {
        (self.center_x(), self.center_y())
    }

    /// True if the rect has a positive area.
    pub fn is_valid(&self) -> bool {
        self.w > 0.0 && self.h > 0.0 && self.x.is_finite() && self.y.is_finite()
    }

    /// True if the point (in normalized space) lies inside the rect.
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.left() && px <= self.right() && py >= self.top() && py <= self.bottom()
    }

    /// Clamp to a minimum size of `min` on both axes, keeping the top-left.
    pub fn with_min_size(mut self, min: f32) -> Self {
        self.w = self.w.max(min);
        self.h = self.h.max(min);
        self
    }

    /// Clamp the rect into `0..1` without changing its size when possible.
    pub fn clamped_to_canvas(mut self) -> Self {
        self.w = self.w.clamp(0.001, 1.0);
        self.h = self.h.clamp(0.001, 1.0);
        self.x = self.x.clamp(0.0, 1.0 - self.w);
        self.y = self.y.clamp(0.0, 1.0 - self.h);
        self
    }

    /// Replace non-finite values with a full-canvas rect.
    pub fn sanitized(self) -> Self {
        if self.x.is_finite()
            && self.y.is_finite()
            && self.w.is_finite()
            && self.h.is_finite()
            && self.w > 0.0
            && self.h > 0.0
        {
            self
        } else {
            Self::full()
        }
    }

    /// Scale a normalized rect into pixel space of a canvas of size `size`.
    pub fn to_pixels(&self, canvas: Size) -> PixelRect {
        PixelRect {
            x: self.x * canvas.w,
            y: self.y * canvas.h,
            w: self.w * canvas.w,
            h: self.h * canvas.h,
        }
    }
}

/// A rectangle in pixels (canvas-local coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PixelRect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl PixelRect {
    /// Right edge.
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// Bottom edge.
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Center.
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
}

/// A canvas size in pixels or points.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Size {
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Size {
    /// Construct.
    pub fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }

    /// Nominal 16:9 canvas at 1920x1080 -- only used for placeholders.
    pub fn hd() -> Self {
        Self::new(1920.0, 1080.0)
    }
}

/// Texture cropping, expressed in normalized source coordinates.
///
/// `left/top/right/bottom` are the amounts removed from each edge, matching the
/// sliders in the layer context menu.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CropRect {
    /// Removed from the left edge, `0..1`.
    pub left: f32,
    /// Removed from the top edge, `0..1`.
    pub top: f32,
    /// Removed from the right edge, `0..1`.
    pub right: f32,
    /// Removed from the bottom edge, `0..1`.
    pub bottom: f32,
}

impl Default for CropRect {
    fn default() -> Self {
        Self::none()
    }
}

impl CropRect {
    /// No crop.
    pub fn none() -> Self {
        Self {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        }
    }

    /// Clamp each edge into `0..1` and ensure the remaining window is at least
    /// 1% wide/tall so the shader never samples a degenerate range.
    pub fn sanitized(self) -> Self {
        let mut c = CropRect {
            left: self.left.clamp(0.0, 1.0),
            top: self.top.clamp(0.0, 1.0),
            right: self.right.clamp(0.0, 1.0),
            bottom: self.bottom.clamp(0.0, 1.0),
        };
        if c.left + c.right > 0.99 {
            let scale = 0.99 / (c.left + c.right);
            c.left *= scale;
            c.right *= scale;
        }
        if c.top + c.bottom > 0.99 {
            let scale = 0.99 / (c.top + c.bottom);
            c.top *= scale;
            c.bottom *= scale;
        }
        c
    }

    /// Normalized `(u_min, v_min)` of the retained window.
    pub fn uv_min(&self) -> (f32, f32) {
        (self.left, self.top)
    }

    /// Normalized `(u_max, v_max)` of the retained window.
    pub fn uv_max(&self) -> (f32, f32) {
        (1.0 - self.right, 1.0 - self.bottom)
    }

    /// The retained window in normalized coordinates.
    pub fn uv_size(&self) -> (f32, f32) {
        let (u0, v0) = self.uv_min();
        let (u1, v1) = self.uv_max();
        ((u1 - u0).max(f32::EPSILON), (v1 - v0).max(f32::EPSILON))
    }

    /// True if any edge is non-zero.
    pub fn is_active(&self) -> bool {
        self.left > 0.0 || self.top > 0.0 || self.right > 0.0 || self.bottom > 0.0
    }

    /// Convert a normalized crop window into integer pixel bounds for a plane
    /// of `w x h` texels. The returned rect is `(x, y, w, h)` and is guaranteed
    /// to be non-empty and inside the plane.
    pub fn to_pixel_window(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let c = self.sanitized();
        let x0 = (c.left * w as f32).floor().clamp(0.0, (w.saturating_sub(1)) as f32) as u32;
        let y0 = (c.top * h as f32).floor().clamp(0.0, (h.saturating_sub(1)) as f32) as u32;
        let x1 = ((1.0 - c.right) * w as f32)
            .ceil()
            .clamp((x0 + 1) as f32, w as f32) as u32;
        let y1 = ((1.0 - c.bottom) * h as f32)
            .ceil()
            .clamp((y0 + 1) as f32, h as f32) as u32;
        (x0, y0, x1 - x0, y1 - y0)
    }
}

/// The eight resize handles of a selected layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// Top-left corner.
    TopLeft,
    /// Top-right corner.
    TopRight,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom-right corner.
    BottomRight,
    /// Left edge (middle).
    Left,
    /// Right edge (middle).
    Right,
    /// Top edge (middle).
    Top,
    /// Bottom edge (middle).
    Bottom,
}

impl Handle {
    /// All handles in draw order.
    pub const ALL: [Handle; 8] = [
        Handle::TopLeft,
        Handle::TopRight,
        Handle::BottomLeft,
        Handle::BottomRight,
        Handle::Left,
        Handle::Right,
        Handle::Top,
        Handle::Bottom,
    ];

    /// True for the four corner handles.
    pub fn is_corner(self) -> bool {
        matches!(
            self,
            Handle::TopLeft | Handle::TopRight | Handle::BottomLeft | Handle::BottomRight
        )
    }

    /// The handle at `(px, py)` for a pixel-space rect, if the pointer is
    /// within `radius` pixels of it.
    pub fn hit_test(rect: PixelRect, px: f32, py: f32, radius: f32) -> Option<Handle> {
        let (cx, cy) = rect.center();
        let candidates = [
            (Handle::TopLeft, rect.x, rect.y),
            (Handle::TopRight, rect.right(), rect.y),
            (Handle::BottomLeft, rect.x, rect.bottom()),
            (Handle::BottomRight, rect.right(), rect.bottom()),
            (Handle::Left, rect.x, cy),
            (Handle::Right, rect.right(), cy),
            (Handle::Top, cx, rect.y),
            (Handle::Bottom, cx, rect.bottom()),
        ];
        for (handle, hx, hy) in candidates {
            if (px - hx).abs() <= radius && (py - hy).abs() <= radius {
                return Some(handle);
            }
        }
        None
    }

    /// Resize `rect` by dragging `handle` by `(dx, dy)` normalized units.
    ///
    /// When `aspect` is `Some(ratio)` (`w/h` of the *source*), corner handles
    /// keep the source aspect ratio, while edge handles are free so the user
    /// can crop by dragging a single edge. `min_size` is enforced in
    /// normalized units.
    pub fn resize(
        handle: Handle,
        rect: NormalizedRect,
        dx: f32,
        dy: f32,
        aspect: Option<f32>,
        min_size: f32,
    ) -> NormalizedRect {
        let mut r = rect;
        match handle {
            Handle::Left => {
                let nx = (r.x + dx).min(r.right() - min_size);
                r.w = r.right() - nx;
                r.x = nx;
            }
            Handle::Right => {
                r.w = (r.w + dx).max(min_size);
            }
            Handle::Top => {
                let ny = (r.y + dy).min(r.bottom() - min_size);
                r.h = r.bottom() - ny;
                r.y = ny;
            }
            Handle::Bottom => {
                r.h = (r.h + dy).max(min_size);
            }
            Handle::TopLeft
            | Handle::TopRight
            | Handle::BottomLeft
            | Handle::BottomRight => {
                let left = matches!(handle, Handle::TopLeft | Handle::BottomLeft);
                let top = matches!(handle, Handle::TopLeft | Handle::TopRight);
                // Dragging a left/top edge moves the origin as well, so the
                // size change is applied and then the origin is pulled back
                // below.
                let mut w = if left {
                    (r.w - dx).max(min_size)
                } else {
                    (r.w + dx).max(min_size)
                };
                let mut h = if top {
                    (r.h - dy).max(min_size)
                } else {
                    (r.h + dy).max(min_size)
                };

                // Aspect ratio lock for corner drags: derive the missing
                // dimension from the dominant axis. The canvas is assumed
                // square in normalized units, which is how `fit_rect` works
                // (layers are letterboxed inside their rect).
                if let Some(a) = aspect {
                    if a.is_finite() && a > 0.0 {
                        // h = w / a only when the pointer moved horizontally more.
                        if dx.abs() >= dy.abs() {
                            h = w / a;
                        } else {
                            w = h * a;
                        }
                        w = w.max(min_size);
                        h = h.max(min_size);
                    }
                }

                if left {
                    r.x = r.right() - w;
                }
                if top {
                    r.y = r.bottom() - h;
                }
                r.w = w;
                r.h = h;
            }
        }
        r
    }
}

/// Snap a moving edge/value to the nearest candidate within `sensitivity`
/// pixels, returning the snapped normalized value and the guide position.
///
/// `candidates` are normalized positions (edges/centers). `sensitivity` is in
/// pixels of a canvas with height `canvas_len`.
pub fn snap_value(
    value: f32,
    candidates: &[f32],
    sensitivity_norm: f32,
) -> (f32, Option<f32>) {
    let mut best: Option<(f32, f32)> = None;
    for &c in candidates {
        let delta = (value - c).abs();
        if delta <= sensitivity_norm && best.is_none_or(|(d, _)| delta < d) {
            best = Some((delta, c));
        }
    }
    match best {
        Some((_, c)) => (c, Some(c)),
        None => (value, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_rect_basic_accessors() {
        let r = NormalizedRect::new(0.1, 0.2, 0.5, 0.4);
        assert!((r.right() - 0.6).abs() < 1e-6);
        assert!((r.bottom() - 0.6).abs() < 1e-6);
        assert!((r.center_x() - 0.35).abs() < 1e-6);
        assert!((r.center_y() - 0.4).abs() < 1e-6);
        assert!(r.contains(0.3, 0.3));
        assert!(!r.contains(0.9, 0.3));
    }

    #[test]
    fn crop_uv_window_is_symmetric() {
        let c = CropRect {
            left: 0.1,
            top: 0.2,
            right: 0.3,
            bottom: 0.4,
        };
        let (u0, v0) = c.uv_min();
        let (u1, v1) = c.uv_max();
        assert!((u0 - 0.1).abs() < 1e-6);
        assert!((v0 - 0.2).abs() < 1e-6);
        assert!((u1 - 0.7).abs() < 1e-6);
        assert!((v1 - 0.6).abs() < 1e-6);
        let (sw, sh) = c.uv_size();
        assert!((sw - 0.6).abs() < 1e-6);
        assert!((sh - 0.4).abs() < 1e-6);
    }

    #[test]
    fn crop_pixel_window_never_degenerate() {
        let c = CropRect {
            left: 0.999,
            top: 0.0,
            right: 0.999,
            bottom: 0.0,
        };
        let (x, y, w, h) = c.to_pixel_window(1920, 1080);
        assert!(w >= 1);
        assert!(h >= 1);
        assert!(x + w <= 1920);
        assert!(y + h <= 1080);
    }

    #[test]
    fn crop_pixel_window_accounts_for_odd_sizes() {
        let c = CropRect {
            left: 0.5,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        };
        let (x, _y, w, _h) = c.to_pixel_window(1919, 1079);
        assert_eq!(x, 959);
        assert_eq!(w, 960);
    }

    #[test]
    fn handle_hit_test_finds_corners_first() {
        let rect = PixelRect {
            x: 100.0,
            y: 100.0,
            w: 200.0,
            h: 100.0,
        };
        assert_eq!(
            Handle::hit_test(rect, 100.0, 100.0, 8.0),
            Some(Handle::TopLeft)
        );
        assert_eq!(
            Handle::hit_test(rect, 300.0, 200.0, 8.0),
            Some(Handle::BottomRight)
        );
        assert_eq!(Handle::hit_test(rect, 200.0, 150.0, 8.0), None);
    }

    #[test]
    fn resize_left_edge_moves_left_and_keeps_right_edge() {
        let r = NormalizedRect::new(0.2, 0.2, 0.4, 0.4);
        let out = Handle::resize(Handle::Left, r, 0.1, 0.0, None, 0.01);
        assert!((out.x - 0.3).abs() < 1e-6);
        assert!((out.right() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn resize_respects_min_size() {
        let r = NormalizedRect::new(0.2, 0.2, 0.2, 0.2);
        let out = Handle::resize(Handle::Right, r, -1.0, 0.0, None, 0.05);
        assert!((out.w - 0.05).abs() < 1e-6);
    }

    #[test]
    fn resize_corner_with_aspect_lock_uses_dominant_axis() {
        let r = NormalizedRect::new(0.1, 0.1, 0.5, 0.5);
        // Drag the bottom-right corner 0.2 to the right (dominant x).
        let out = Handle::resize(Handle::BottomRight, r, 0.2, 0.01, Some(2.0), 0.01);
        assert!((out.w - 0.7).abs() < 1e-5);
        assert!((out.h - 0.35).abs() < 1e-5);
    }

    #[test]
    fn snap_value_snaps_within_sensitivity() {
        let (v, guide) = snap_value(0.504, &[0.0, 0.5, 1.0], 0.01);
        assert!((v - 0.5).abs() < 1e-6);
        assert_eq!(guide, Some(0.5));
    }

    #[test]
    fn snap_value_ignores_far_candidates() {
        let (v, guide) = snap_value(0.3, &[0.0, 0.5, 1.0], 0.01);
        assert!((v - 0.3).abs() < 1e-6);
        assert_eq!(guide, None);
    }
}
