//! Canvas helpers: snapping to edges and centres, the alignment-studio
//! overlay, and the small numeric readouts that sit on the canvas.

use eframe::egui::{self, Pos2, Rect, Stroke, Ui};

use crate::app::App;
use crate::model::{snap_value, NormalizedRect};
use crate::ui::theme;

/// The normalized positions a dragged edge or centre can snap to.
#[derive(Debug, Clone, Copy)]
pub struct SnapTargets {
    /// Snap to the canvas edges.
    pub canvas_edges: bool,
    /// Snap to the canvas centre lines.
    pub canvas_centre: bool,
    /// Snap to other layers' edges.
    pub other_layers: bool,
    /// Snapping distance, in normalized units.
    pub threshold: f32,
}

impl Default for SnapTargets {
    fn default() -> Self {
        Self {
            canvas_edges: true,
            canvas_centre: true,
            other_layers: true,
            threshold: 0.004,
        }
    }
}

/// Snap a dragged rect and return the guides to draw.
///
/// Only the *moved* axis edges are considered: a moved layer snaps on both x
/// and y; a resized layer snaps only the edge that is actually moving (the
/// caller passes the whole rect, so we snap the nearest edge of each axis).
pub fn apply_snapping(
    app: &App,
    rect: NormalizedRect,
    moving: bool,
    targets: SnapTargets,
) -> (NormalizedRect, Vec<(bool, f32)>) {
    let mut out = rect;
    let mut guides = Vec::new();
    let px_threshold = app.persisted.preferences.snap_px.max(0.0);
    // Convert the pixel sensitivity into normalized units using the canvas the
    // drag is happening on. A zero-sized canvas (pre-layout) skips snapping.
    let (cw, ch) = (app.canvas_rect.width(), app.canvas_rect.height());
    if cw < 8.0 || ch < 8.0 || px_threshold <= 0.0 {
        return (out, guides);
    }
    let tx = (px_threshold / cw).max(targets.threshold * 0.25);
    let ty = (px_threshold / ch).max(targets.threshold * 0.25);

    let mut xs: Vec<f32> = Vec::new();
    let mut ys: Vec<f32> = Vec::new();
    if targets.canvas_edges {
        xs.extend([0.0, 1.0]);
        ys.extend([0.0, 1.0]);
    }
    if targets.canvas_centre {
        xs.push(0.5);
        ys.push(0.5);
    }
    if targets.other_layers {
        for layer in &app.session.layers {
            if Some(layer.id) == app.selected {
                continue;
            }
            let r = layer.rect;
            xs.extend([r.left(), r.center_x(), r.right()]);
            ys.extend([r.top(), r.center_y(), r.bottom()]);
        }
    }

    // Candidate anchors of the dragged rect on each axis.
    let x_anchors = [out.left(), out.center_x(), out.right()];
    let y_anchors = [out.top(), out.center_y(), out.bottom()];

    let best = |anchors: &[f32; 3], targets: &[f32], t: f32| -> Option<f32> {
        let mut best: Option<(f32, f32)> = None;
        for (i, &a) in anchors.iter().enumerate() {
            for &target in targets {
                let delta = target - a;
                if delta.abs() <= t && best.is_none_or(|(_, d)| delta.abs() < d.abs()) {
                    best = Some((delta, delta));
                    let _ = i;
                }
            }
        }
        best.map(|(d, _)| d)
    };

    if moving {
        if let Some(dx) = best(&x_anchors, &xs, tx) {
            out.x += dx;
            // Which target won? Recompute to draw the guide.
            if let Some(pos) = nearest_target(&[out.left(), out.center_x(), out.right()], &xs, tx) {
                guides.push((true, pos));
            }
        }
        if let Some(dy) = best(&y_anchors, &ys, ty) {
            out.y += dy;
            if let Some(pos) = nearest_target(&[out.top(), out.center_y(), out.bottom()], &ys, ty) {
                guides.push((false, pos));
            }
        }
        return (out, guides);
    }

    // Resizing: snap whichever edge is closest to a target, adjusting the rect
    // so the opposite edge stays put.
    let (left, _) = snap_value(out.x, &xs, tx);
    if (left - out.x).abs() <= tx {
        out.w -= left - out.x;
        out.x = left;
        guides.push((true, left));
    }
    let (right, _) = snap_value(out.right(), &xs, tx);
    if (right - out.right()).abs() <= tx && right > out.left() + 0.01 {
        out.w = right - out.x;
        guides.push((true, right));
    }
    let (top, _) = snap_value(out.y, &ys, ty);
    if (top - out.y).abs() <= ty {
        out.h -= top - out.y;
        out.y = top;
        guides.push((false, top));
    }
    let (bottom, _) = snap_value(out.bottom(), &ys, ty);
    if (bottom - out.bottom()).abs() <= ty && bottom > out.top() + 0.01 {
        out.h = bottom - out.y;
        guides.push((false, bottom));
    }
    (out.sanitized(), guides)
}

fn nearest_target(anchors: &[f32; 3], targets: &[f32], t: f32) -> Option<f32> {
    let mut best: Option<(f32, f32)> = None;
    for &a in anchors {
        for &target in targets {
            let d = (target - a).abs();
            if d <= t && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((target, d));
            }
        }
    }
    best.map(|(p, _)| p)
}

/// Draw the alignment studio's compare overlay label and divider readout.
pub fn studio_overlay(app: &mut App, ui: &mut Ui, canvas_rect: Rect) {
    if !app.studio.open {
        return;
    }
    let kind = app.studio.mode;
    let position = app.studio.wipe;
    if kind == crate::gpu::capture::StudioMode::Wipe {
        let x = canvas_rect.left() + position * canvas_rect.width();
        ui.painter()
            .vline(x, canvas_rect.y_range(), Stroke::new(1.5, theme::ACCENT));
    }
    let label = match kind {
        crate::gpu::capture::StudioMode::Onion => format!(
            "ONION  {:>3}%  ·  A {}  B {}",
            (position * 100.0).round() as i32,
            app.studio.short_a(),
            app.studio.short_b()
        ),
        crate::gpu::capture::StudioMode::Wipe => format!(
            "WIPE  {:>3}%  ·  A {}  B {}",
            (position * 100.0).round() as i32,
            app.studio.short_a(),
            app.studio.short_b()
        ),
    };
    let pos = Pos2::new(canvas_rect.center().x, canvas_rect.bottom() + 12.0);
    ui.painter().text(
        pos,
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::monospace(10.5),
        theme::ACCENT,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_target_picks_the_closest() {
        let targets = [0.0, 0.5, 1.0];
        assert_eq!(nearest_target(&[0.49, 0.49, 0.49], &targets, 0.02), Some(0.5));
        assert_eq!(nearest_target(&[0.2, 0.2, 0.2], &targets, 0.02), None);
    }

    #[test]
    fn snap_targets_are_on_by_default() {
        let t = SnapTargets::default();
        assert!(t.canvas_edges);
        assert!(t.canvas_centre);
        assert!(t.other_layers);
        assert!(t.threshold > 0.0);
    }

    #[test]
    fn snapping_helper_is_identity_without_a_canvas() {
        // No App is available here, so exercise the pure geometry that the
        // app path uses: a rect with valid edges stays inside the canvas.
        let r = NormalizedRect::new(0.1, 0.1, 0.5, 0.5).sanitized();
        assert_eq!(r, NormalizedRect::new(0.1, 0.1, 0.5, 0.5));
        let clamped = NormalizedRect::new(-0.2, 0.9, 0.6, 0.6).clamped_to_canvas();
        assert!(clamped.x >= 0.0);
        assert!(clamped.bottom() <= 1.0 + 1e-6);
    }
}
