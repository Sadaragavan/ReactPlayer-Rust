//! Canvas aspect ratios and the layout helper that maps a layer rectangle
//! (normalized canvas space) onto a display rectangle while preserving the
//! source aspect ratio.

use serde::{Deserialize, Serialize};

use super::geom::{NormalizedRect, PixelRect, Size};

/// How a decoded frame is fitted inside its layer rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FitMode {
    /// Letterbox: show the whole frame, may leave bars (default).
    #[default]
    Fit,
    /// Crop: fill the rect, may cut content.
    Fill,
}

/// Canvas shapes supported by the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CanvasAspect {
    /// 16:9
    Widescreen,
    /// 9:16
    Vertical,
    /// 4:3
    Classic,
    /// 1:1
    Square,
    /// 21:9
    Cinema,
    /// Fill the whole window, whatever its shape.
    FillWindow,
}

impl Default for CanvasAspect {
    /// The default canvas is 16:9.
    fn default() -> Self {
        Self::Widescreen
    }
}

impl CanvasAspect {
    /// All variants, in menu order.
    pub const ALL: [CanvasAspect; 6] = [
        CanvasAspect::Widescreen,
        CanvasAspect::Vertical,
        CanvasAspect::Classic,
        CanvasAspect::Square,
        CanvasAspect::Cinema,
        CanvasAspect::FillWindow,
    ];

    /// The width/height ratio, or `None` for [`CanvasAspect::FillWindow`].
    pub fn ratio(self) -> Option<f32> {
        match self {
            CanvasAspect::Widescreen => Some(16.0 / 9.0),
            CanvasAspect::Vertical => Some(9.0 / 16.0),
            CanvasAspect::Classic => Some(4.0 / 3.0),
            CanvasAspect::Square => Some(1.0),
            CanvasAspect::Cinema => Some(21.0 / 9.0),
            CanvasAspect::FillWindow => None,
        }
    }

    /// Human readable label for menus.
    pub fn label(self) -> &'static str {
        match self {
            CanvasAspect::Widescreen => "16:9",
            CanvasAspect::Vertical => "9:16",
            CanvasAspect::Classic => "4:3",
            CanvasAspect::Square => "1:1",
            CanvasAspect::Cinema => "21:9",
            CanvasAspect::FillWindow => "Fill Window",
        }
    }

    /// Fit the canvas inside `available`, preserving the aspect ratio.
    pub fn canvas_rect(self, available: PixelRect) -> PixelRect {
        match self.ratio() {
            None => available,
            Some(r) => {
                let mut w = available.w;
                let mut h = w / r;
                if h > available.h {
                    h = available.h;
                    w = h * r;
                }
                PixelRect {
                    x: available.x + (available.w - w) * 0.5,
                    y: available.y + (available.h - h) * 0.5,
                    w,
                    h,
                }
            }
        }
    }
}

/// Convert a [`PixelRect`] canvas into a [`Size`].
pub fn pixel_size(r: PixelRect) -> Size {
    Size::new(r.w, r.h)
}

/// Map a normalized layer rect into pixels of `canvas`.
pub fn layer_pixel_rect(layer: NormalizedRect, canvas: PixelRect) -> PixelRect {
    let n = layer.to_pixels(pixel_size(canvas));
    PixelRect {
        x: n.x + canvas.x,
        y: n.y + canvas.y,
        w: n.w,
        h: n.h,
    }
}

/// Fit a `src_w x src_h` frame inside `dst` according to `mode`, optionally
/// locking the aspect ratio. The result is the *painted* rectangle: with
/// [`FitMode::Fit`] it is inside `dst` (letterboxed), with [`FitMode::Fill`]
/// it is at least as large as `dst` and is expected to be clipped by the
/// scissor of the paint callback.
pub fn fit_rect(dst: PixelRect, src_w: f32, src_h: f32, mode: FitMode) -> PixelRect {
    if src_w <= 0.0 || src_h <= 0.0 || dst.w <= 0.0 || dst.h <= 0.0 {
        return dst;
    }
    let src_ratio = src_w / src_h;
    let scale = match mode {
        FitMode::Fit => {
            let w = dst.w;
            let h = w / src_ratio;
            if h > dst.h {
                let h2 = dst.h;
                let w2 = h2 * src_ratio;
                (w2, h2)
            } else {
                (w, h)
            }
        }
        FitMode::Fill => {
            let w = dst.w;
            let h = w / src_ratio;
            if h < dst.h {
                let h2 = dst.h;
                let w2 = h2 * src_ratio;
                (w2, h2)
            } else {
                (w, h)
            }
        }
    };
    PixelRect {
        x: dst.x + (dst.w - scale.0) * 0.5,
        y: dst.y + (dst.h - scale.1) * 0.5,
        w: scale.0,
        h: scale.1,
    }
}

/// Aspect ratio (w/h) implied by the destination rect when a layer has no
/// known source size yet.
pub fn dst_aspect(dst: PixelRect) -> f32 {
    if dst.h <= 0.0 { 1.0 } else { dst.w / dst.h }
}

/// True if the two aspect ratios are equal within a small tolerance.
pub fn aspect_ratios_close(a: f32, b: f32) -> bool {
    if !a.is_finite() || !b.is_finite() || b == 0.0 {
        return false;
    }
    ((a / b) - 1.0).abs() < 0.005
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f32, y: f32, w: f32, h: f32) -> PixelRect {
        PixelRect { x, y, w, h }
    }

    #[test]
    fn canvas_16_9_is_centered() {
        let out = CanvasAspect::Widescreen.canvas_rect(r(0.0, 0.0, 1000.0, 1000.0));
        assert!((out.w - 1000.0).abs() < 0.01);
        assert!((out.h - 562.5).abs() < 0.01);
        assert!((out.y - 218.75).abs() < 0.01);
    }

    #[test]
    fn canvas_fill_window_returns_available() {
        let avail = r(10.0, 20.0, 300.0, 400.0);
        let out = CanvasAspect::FillWindow.canvas_rect(avail);
        assert_eq!(out, avail);
    }

    #[test]
    fn fit_mode_letterboxes_wide_source() {
        let dst = r(0.0, 0.0, 100.0, 100.0);
        let out = fit_rect(dst, 1920.0, 1080.0, FitMode::Fit);
        assert!((out.w - 100.0).abs() < 0.01);
        assert!((out.h - 56.25).abs() < 0.01);
        assert!((out.y - 21.875).abs() < 0.01);
    }

    #[test]
    fn fill_mode_overflows_narrow_source() {
        let dst = r(0.0, 0.0, 100.0, 100.0);
        let out = fit_rect(dst, 1920.0, 1080.0, FitMode::Fill);
        assert!((out.h - 100.0).abs() < 0.01);
        assert!((out.w - 177.7778).abs() < 0.01);
        assert!((out.x + 88.89).abs() < 0.05);
    }

    #[test]
    fn fit_rect_handles_degenerate_input() {
        let dst = r(0.0, 0.0, 100.0, 100.0);
        assert_eq!(fit_rect(dst, 0.0, 100.0, FitMode::Fit), dst);
        assert_eq!(fit_rect(dst, 100.0, 0.0, FitMode::Fill), dst);
    }

    #[test]
    fn normalized_layer_maps_into_canvas_offsets() {
        let canvas = r(50.0, 25.0, 800.0, 450.0);
        let out = layer_pixel_rect(NormalizedRect::new(0.5, 0.5, 0.5, 0.5), canvas);
        assert!((out.x - 450.0).abs() < 1e-3);
        assert!((out.y - 250.0).abs() < 1e-3);
        assert!((out.w - 400.0).abs() < 1e-3);
        assert!((out.h - 225.0).abs() < 1e-3);
    }

    #[test]
    fn aspect_ratio_lookup_is_stable() {
        assert!(aspect_ratios_close(16.0 / 9.0, CanvasAspect::Widescreen.ratio().unwrap()));
        assert!(!aspect_ratios_close(1.0, CanvasAspect::Widescreen.ratio().unwrap()));
        assert_eq!(CanvasAspect::FillWindow.ratio(), None);
    }
}
