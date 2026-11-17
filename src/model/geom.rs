//! Canvas geometry: normalized rectangles, crop, resize handles and snapping.
//!
//! Layer rectangles are stored as fractions of the canvas (`0..1`), never as
//! pixels, so a layout survives a window resize, a canvas-aspect change and a
//! trip through a preset file unchanged. Pixels only exist while a pointer is
//! being handled.

/// Smallest layer, as a fraction of the canvas.
pub const MIN_LAYER_SIZE: f32 = 0.06;

/// A rectangle in canvas fractions.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct NormRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl NormRect {
    pub const FULL: Self = Self {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn left(self) -> f32 {
        self.x
    }

    pub fn right(self) -> f32 {
        self.x + self.w
    }

    pub fn top(self) -> f32 {
        self.y
    }

    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    pub fn center_x(self) -> f32 {
        self.x + self.w * 0.5
    }

    pub fn center_y(self) -> f32 {
        self.y + self.h * 0.5
    }

    pub fn center(self) -> Point {
        Point {
            x: self.center_x(),
            y: self.center_y(),
        }
    }

    /// Page-relative rect in pixels.
    pub fn to_px(self, canvas: Size) -> PxRect {
        PxRect {
            x: self.x * canvas.w,
            y: self.y * canvas.h,
            w: self.w * canvas.w,
            h: self.h * canvas.h,
        }
    }

    /// From pixels back to fractions.
    pub fn from_px(px: PxRect, canvas: Size) -> Self {
        let cw = canvas.w.max(1.0);
        let ch = canvas.h.max(1.0);
        Self {
            x: px.x / cw,
            y: px.y / ch,
            w: px.w / cw,
            h: px.h / ch,
        }
    }

    /// Pixel aspect ratio (width / height) for a given canvas.
    pub fn aspect_px(self, canvas: Size) -> f32 {
        let px = self.to_px(canvas);
        if px.h <= 0.0 {
            return 1.0;
        }
        px.w / px.h
    }

    /// Keep the rectangle on the canvas and at least [`MIN_LAYER_SIZE`] big.
    pub fn clamped(self) -> Self {
        let w = self.w.clamp(MIN_LAYER_SIZE, 1.0);
        let h = self.h.clamp(MIN_LAYER_SIZE, 1.0);
        let x = self.x.clamp(0.0, 1.0 - w);
        let y = self.y.clamp(0.0, 1.0 - h);
        Self { x, y, w, h }
    }

    pub fn contains(self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }
}

/// A point in canvas fractions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// A size in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }
}

/// A rectangle in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PxRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl PxRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    pub fn center(self) -> Point {
        Point {
            x: self.x + self.w * 0.5,
            y: self.y + self.h * 0.5,
        }
    }

    pub fn contains(self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }
}

/// The four sides of the crop, as fractions of the source frame.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct Crop {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Crop {
    pub const ZERO: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };

    /// Fraction of the source that is left after cropping, per axis.
    pub const MAX_EDGE: f32 = 0.95;

    pub fn is_zero(self) -> bool {
        self == Self::ZERO
    }

    fn normalized(self) -> Self {
        let left = self.left.clamp(0.0, Self::MAX_EDGE);
        let top = self.top.clamp(0.0, Self::MAX_EDGE);
        let right = self.right.clamp(0.0, Self::MAX_EDGE - left);
        let bottom = self.bottom.clamp(0.0, Self::MAX_EDGE - top);
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Normalized UV rect for the shader: (left, top, right, bottom) in 0..1
    /// with the right/bottom expressed as "up to".
    pub fn uv(self) -> [f32; 4] {
        let c = self.normalized();
        [c.left, c.top, 1.0 - c.right, 1.0 - c.bottom]
    }

    /// The cropped region of a `w` x `h` frame, in pixels.
    pub fn to_px(self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let c = self.normalized();
        let x0 = (c.left * w as f32).round().clamp(0.0, w.saturating_sub(1) as f32) as u32;
        let y0 = (c.top * h as f32).round().clamp(0.0, h.saturating_sub(1) as f32) as u32;
        let x1 = ((1.0 - c.right) * w as f32)
            .round()
            .clamp((x0 + 1) as f32, w as f32) as u32;
        let y1 = ((1.0 - c.bottom) * h as f32)
            .round()
            .clamp((y0 + 1) as f32, h as f32) as u32;
        (x0, y0, x1 - x0, y1 - y0)
    }

    /// Effective visible size in pixels, used to keep the aspect math honest
    /// when a layer is cropped.
    pub fn visible(self, w: u32, h: u32) -> (u32, u32) {
        let (_, _, cw, ch) = self.to_px(w, h);
        (cw, ch)
    }
}

/// The eight resize handles, named by the corner or edge they sit on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl Handle {
    pub const ALL: [Self; 8] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Right,
        Self::BottomRight,
        Self::Bottom,
        Self::BottomLeft,
        Self::Left,
    ];

    /// Unit anchor inside the rectangle: (0,0) is the top-left corner.
    pub fn anchor(self) -> (f32, f32) {
        match self {
            Self::TopLeft => (0.0, 0.0),
            Self::Top => (0.5, 0.0),
            Self::TopRight => (1.0, 0.0),
            Self::Right => (1.0, 0.5),
            Self::BottomRight => (1.0, 1.0),
            Self::Bottom => (0.5, 1.0),
            Self::BottomLeft => (0.0, 1.0),
            Self::Left => (0.0, 0.5),
        }
    }

    /// True when the handle moves the left edge, the right edge, both, or
    /// neither.
    pub fn edges(self) -> (bool, bool, bool, bool) {
        let (ax, ay) = self.anchor();
        (ax < 0.5, ax > 0.5, ay < 0.5, ay > 0.5)
    }
}

/// The handle nearest to `p`, within `tolerance` pixels of the rectangle's
/// edge frame. Corners win over edges when both are in range.
pub fn hit_handle(rect: PxRect, p: Point, tolerance: f32) -> Option<Handle> {
    let mut best: Option<(f32, Handle)> = None;
    for handle in Handle::ALL {
        let (ax, ay) = handle.anchor();
        let hp = Point {
            x: rect.x + ax * rect.w,
            y: rect.y + ay * rect.h,
        };
        let d = ((hp.x - p.x).powi(2) + (hp.y - p.y).powi(2)).sqrt();
        if d <= tolerance && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, handle));
        }
    }
    best.map(|(_, h)| h)
}

/// Resize `rect` by dragging `handle` a `delta` in pixels.
///
/// `aspect` is a pixel aspect ratio (width / height) to hold, if the layer is
/// aspect-locked. The opposite corner stays put.
pub fn resize_with_handle(
    rect: NormRect,
    handle: Handle,
    delta: Point,
    canvas: Size,
    aspect: Option<f32>,
) -> NormRect {
    let px = rect.to_px(canvas);
    let (left, right, top, bottom) = handle.edges();
    let mut x0 = px.x;
    let mut y0 = px.y;
    let mut x1 = px.x + px.w;
    let mut y1 = px.y + px.h;
    if left {
        x0 += delta.x;
    }
    if right {
        x1 += delta.x;
    }
    if top {
        y0 += delta.y;
    }
    if bottom {
        y1 += delta.y;
    }

    let min_w = MIN_LAYER_SIZE * canvas.w;
    let min_h = MIN_LAYER_SIZE * canvas.h;
    if x1 - x0 < min_w {
        if left {
            x0 = x1 - min_w;
        } else {
            x1 = x0 + min_w;
        }
    }
    if y1 - y0 < min_h {
        if top {
            y0 = y1 - min_h;
        } else {
            y1 = y0 + min_h;
        }
    }

    // Aspect lock: drive the free axis from the dragged one.
    if let Some(a) = aspect.filter(|a| *a > 0.0) {
        let w = x1 - x0;
        let h = y1 - y0;
        let corner = left || right;
        let vertical = top || bottom;
        if corner && vertical {
            // Corners keep the width and derive the height.
            let new_h = w / a;
            if top {
                y0 = y1 - new_h;
            } else {
                y1 = y0 + new_h;
            }
        } else if corner {
            let new_h = w / a;
            let cy = (y0 + y1) * 0.5;
            y0 = cy - new_h * 0.5;
            y1 = cy + new_h * 0.5;
        } else {
            let new_w = h * a;
            let cx = (x0 + x1) * 0.5;
            x0 = cx - new_w * 0.5;
            x1 = cx + new_w * 0.5;
        }
    }

    NormRect::from_px(PxRect::new(x0, y0, x1 - x0, y1 - y0), canvas).clamped()
}

/// Result of a snapping pass: the adjusted rectangle plus the guide lines that
/// should be drawn while the snap holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Snap {
    pub rect: NormRect,
    /// Vertical guide lines, as fractions of the canvas width.
    pub guides_x: Vec<f32>,
    /// Horizontal guide lines, as fractions of the canvas height.
    pub guides_y: Vec<f32>,
}

impl Snap {
    pub fn none(rect: NormRect) -> Self {
        Self {
            rect,
            guides_x: Vec::new(),
            guides_y: Vec::new(),
        }
    }

    pub fn is_snapped(&self) -> bool {
        !self.guides_x.is_empty() || !self.guides_y.is_empty()
    }
}

/// Snap a moved rectangle to the canvas edges, the canvas centre lines and the
/// edges/centres of the other visible layers, within `tolerance_px`.
pub fn snap_move(rect: NormRect, canvas: Size, others: &[NormRect], tolerance_px: f32) -> Snap {
    let mut targets_x = vec![0.0, 0.5, 1.0];
    let mut targets_y = vec![0.0, 0.5, 1.0];
    for other in others {
        targets_x.extend([other.left(), other.center_x(), other.right()]);
        targets_y.extend([other.top(), other.center_y(), other.bottom()]);
    }

    let tol_x = (tolerance_px / canvas.w.max(1.0)).min(0.5);
    let tol_y = (tolerance_px / canvas.h.max(1.0)).min(0.5);

    let mut snapped = rect;
    let mut guides_x = Vec::new();
    let mut guides_y = Vec::new();

    if let Some((dx, target)) = nearest_snap([rect.left(), rect.center_x(), rect.right()], &targets_x, tol_x)
    {
        snapped.x += dx;
        guides_x.push(target);
    }
    if let Some((dy, target)) = nearest_snap([rect.top(), rect.center_y(), rect.bottom()], &targets_y, tol_y)
    {
        snapped.y += dy;
        guides_y.push(target);
    }

    Snap {
        rect: snapped,
        guides_x,
        guides_y,
    }
}

/// The smallest adjustment that puts one of `edges` on one of `targets`, plus
/// the target it lands on, if any is within `tolerance` (in the same units).
fn nearest_snap(edges: [f32; 3], targets: &[f32], tolerance: f32) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32)> = None;
    for edge in edges {
        for target in targets {
            let distance = (target - edge).abs();
            if distance <= tolerance && best.is_none_or(|(best_d, _)| distance < best_d) {
                best = Some((distance, *target));
            }
        }
    }
    best.map(|(distance, target)| {
        let edge = edges
            .iter()
            .copied()
            .find(|e| ((target - e).abs() - distance).abs() < 1e-6)
            .unwrap_or(edges[0]);
        (target - edge, target)
    })
}

/// Fit `rect` to a pixel aspect ratio without moving its centre, used by the
/// per-layer "lock aspect" toggle.
pub fn apply_aspect(rect: NormRect, aspect: f32, canvas: Size) -> NormRect {
    if aspect <= 0.0 || canvas.w <= 0.0 || canvas.h <= 0.0 {
        return rect;
    }
    let px = rect.to_px(canvas);
    let w = px.w;
    let h = px.h;
    let current = w / h.max(1e-3);
    let (mut new_w, mut new_h) = if current > aspect {
        (h * aspect, h)
    } else {
        (w, w / aspect)
    };
    if new_w < MIN_LAYER_SIZE * canvas.w {
        new_w = MIN_LAYER_SIZE * canvas.w;
        new_h = new_w / aspect;
    }
    if new_h < MIN_LAYER_SIZE * canvas.h {
        new_h = MIN_LAYER_SIZE * canvas.h;
        new_w = new_h * aspect;
    }
    let cx = px.center().x;
    let cy = px.center().y;
    NormRect::from_px(
        PxRect::new(cx - new_w * 0.5, cy - new_h * 0.5, new_w, new_h),
        canvas,
    )
    .clamped()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Size {
        Size::new(1600.0, 900.0)
    }

    #[test]
    fn normalized_round_trip() {
        let r = NormRect::new(0.1, 0.2, 0.3, 0.4);
        let px = r.to_px(canvas());
        assert_eq!(px, PxRect::new(160.0, 180.0, 480.0, 360.0));
        assert_eq!(NormRect::from_px(px, canvas()), r);
        assert!((r.aspect_px(canvas()) - 480.0 / 360.0).abs() < 1e-5);
    }

    #[test]
    fn clamping_keeps_layer_on_canvas() {
        let r = NormRect::new(0.9, -0.2, 0.5, 0.01).clamped();
        assert!(r.right() <= 1.0 + 1e-6);
        assert!(r.top() >= 0.0);
        assert!(r.h >= MIN_LAYER_SIZE);
    }

    #[test]
    fn hit_testing_the_eight_handles() {
        let px = PxRect::new(100.0, 100.0, 400.0, 200.0);
        let tolerance = 10.0;
        for handle in Handle::ALL {
            let (ax, ay) = handle.anchor();
            let p = Point {
                x: px.x + ax * px.w,
                y: px.y + ay * px.h,
            };
            assert_eq!(hit_handle(px, p, tolerance), Some(handle));
        }
        assert_eq!(hit_handle(px, Point { x: 300.0, y: 200.0 }, tolerance), None);
    }

    #[test]
    fn resizing_from_corners_keeps_the_opposite_corner() {
        let r = NormRect::new(0.25, 0.25, 0.5, 0.5);
        let moved = resize_with_handle(
            r,
            Handle::BottomRight,
            Point { x: 160.0, y: 90.0 },
            canvas(),
            None,
        );
        assert!((moved.x - r.x).abs() < 1e-5);
        assert!((moved.y - r.y).abs() < 1e-5);
        assert!((moved.w - r.w - 0.1).abs() < 1e-4);
        assert!((moved.h - r.h - 0.1).abs() < 1e-4);

        let shrunk = resize_with_handle(
            r,
            Handle::TopLeft,
            Point { x: 80.0, y: 45.0 },
            canvas(),
            None,
        );
        assert!((shrunk.right() - r.right()).abs() < 1e-5);
        assert!((shrunk.bottom() - r.bottom()).abs() < 1e-5);
        assert!(shrunk.w < r.w);
    }

    #[test]
    fn resize_never_goes_below_minimum() {
        let r = NormRect::new(0.4, 0.4, 0.2, 0.2);
        let tiny = resize_with_handle(
            r,
            Handle::BottomRight,
            Point { x: -500.0, y: -500.0 },
            canvas(),
            None,
        );
        assert!(tiny.w >= MIN_LAYER_SIZE - 1e-6);
        assert!(tiny.h >= MIN_LAYER_SIZE - 1e-6);
    }

    #[test]
    fn aspect_lock_holds_the_ratio_at_a_corner() {
        let r = NormRect::new(0.2, 0.2, 0.4, 0.4);
        let aspect = 16.0 / 9.0;
        let out = resize_with_handle(
            r,
            Handle::BottomRight,
            Point { x: 100.0, y: 0.0 },
            canvas(),
            Some(aspect),
        );
        let px = out.to_px(canvas());
        assert!((px.w / px.h - aspect).abs() < 1e-3, "{}", px.w / px.h);
    }

    #[test]
    fn apply_aspect_fits_inside_the_rect() {
        let r = NormRect::new(0.1, 0.1, 0.5, 0.5);
        let out = apply_aspect(r, 16.0 / 9.0, canvas());
        let px = out.to_px(canvas());
        assert!((px.w / px.h - 16.0 / 9.0).abs() < 1e-3);
        assert!((out.center_x() - r.center_x()).abs() < 1e-4);
        assert!((out.center_y() - r.center_y()).abs() < 1e-4);
    }

    #[test]
    fn snapping_to_canvas_edges_and_centre() {
        let r = NormRect::new(0.004, 0.3, 0.3, 0.3);
        let snap = snap_move(r, canvas(), &[], 10.0);
        assert_eq!(snap.rect.x, 0.0);
        assert!(snap.is_snapped());
        assert!(snap.guides_x.iter().any(|g| g.abs() < 1e-6));

        let centered = NormRect::new(0.34, 0.3, 0.3, 0.3);
        let snap = snap_move(centered, canvas(), &[], 20.0);
        assert!((snap.rect.center_x() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn snapping_to_other_layers() {
        let other = NormRect::new(0.5, 0.0, 0.5, 1.0);
        let moving = NormRect::new(0.196, 0.2, 0.3, 0.3);
        let snap = snap_move(moving, canvas(), &[other], 10.0);
        assert!((snap.rect.right() - other.left()).abs() < 1e-5);
        assert!(
            snap.guides_x
                .iter()
                .any(|g| (g - other.left()).abs() < 1e-5)
        );
    }

    #[test]
    fn no_snap_when_far_away() {
        let r = NormRect::new(0.2, 0.35, 0.3, 0.3);
        let snap = snap_move(r, canvas(), &[], 4.0);
        assert_eq!(snap.rect, r);
        assert!(!snap.is_snapped());
    }

    #[test]
    fn crop_uv_and_pixels() {
        let crop = Crop {
            left: 0.1,
            top: 0.2,
            right: 0.1,
            bottom: 0.2,
        };
        assert_eq!(crop.uv(), [0.1, 0.2, 0.9, 0.8]);
        assert_eq!(crop.to_px(1000, 500), (100, 100, 800, 300));
        assert_eq!(crop.visible(1000, 500), (800, 300));
    }

    #[test]
    fn crop_is_bounded() {
        let crop = Crop {
            left: 0.6,
            top: 0.6,
            right: 0.6,
            bottom: 0.6,
        };
        let (_, _, w, h) = crop.to_px(100, 100);
        assert!(w >= 1 && h >= 1);
        let uv = crop.uv();
        assert!(uv[2] > uv[0]);
        assert!(uv[3] > uv[1]);
    }

    #[test]
    fn hit_handle_misses_outside_tolerance() {
        let px = PxRect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(hit_handle(px, Point { x: 50.0, y: 50.0 }, 8.0), None);
        assert_eq!(
            hit_handle(px, Point { x: 101.0, y: 101.0 }, 8.0),
            Some(Handle::BottomRight)
        );
    }
}
