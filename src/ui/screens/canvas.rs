//! The compositing canvas: letterboxed to the session aspect, draws every
//! visible layer through the GPU compositor, and hosts the editor overlays
//! (selection handles, snap guides, drag gestures).

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::app::App;
use crate::engine::hwaccel::DecodeBackend;
use crate::gpu::compositor::{self, LayerPaint, PaintList};
use crate::gpu::renderer::FrameUpload;
use crate::model::layout::{canvas_rect as aspect_canvas_rect, fit_rect, layer_pixel_rect, FitMode};
use crate::model::{Handle, NormalizedRect, PixelRect, Size};
use crate::ui::screens::canvas_tools;
use crate::ui::theme;

/// Draw the central canvas area.
pub fn show(app: &mut App, ui: &mut Ui) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(theme::BG)
                .inner_margin(egui::Margin::same(0)),
        )
        .show(ui, |ui| {
            let available = ui.available_rect_before_wrap();
            if available.width() < 40.0 || available.height() < 40.0 {
                return;
            }
            let inset = if app.theater { 8.0 } else { 18.0 };
            let avail = shrink(available, inset);

            // The canvas keeps the chosen aspect inside the available space,
            // centred, and is what every normalized rect maps into.
            let pixel = PixelRect {
                x: 0.0,
                y: 0.0,
                w: avail.width(),
                h: avail.height(),
            };
            let canvas = aspect_canvas_rect(app.session.aspect, pixel);
            let canvas_rect = Rect::from_min_size(
                Pos2::new(avail.left() + canvas.x, avail.top() + canvas.y),
                Vec2::new(canvas.w, canvas.h),
            );
            app.canvas_rect = canvas_rect;

            let response = ui.allocate_rect(canvas_rect, Sense::click_and_drag());
            app.canvas_hovered = response.hovered();

            // Canvas background: pure black, or the user's wallpaper.
            compositor::paint_canvas_background(ui.painter(), canvas_rect, theme::BG);
            draw_wallpaper(app, ui, canvas_rect);

            // Build the paint list from the runtime's presentation state.
            let draws = build_draws(app, canvas_rect);
            app.paint_list = draws;
            compositor::submit(ui.painter(), &app.paint_list);

            // Overlays: guides first (so handles sit on top of them), then the
            // selected layer's handles, then interactions.
            draw_snap_guides(app, ui, canvas_rect);
            draw_selection(app, ui, canvas_rect);
            interactions(app, ui, canvas_rect, &response);

            // Border around the canvas, 1 px.
            ui.painter().rect_stroke(
                canvas_rect,
                0.0,
                Stroke::new(1.0, theme::BORDER),
                StrokeKind::Outside,
            );

            // Corner readout: canvas size + union duration.
            let size = app.session.aspect.label();
            let text = format!(
                "{}  ·  {} × {} px  ·  {}",
                size,
                canvas_rect.width().round() as i64,
                canvas_rect.height().round() as i64,
                format_duration(app.union.duration_us()),
            );
            ui.painter().text(
                Pos2::new(canvas_rect.left() + 8.0, canvas_rect.top() - 12.0),
                egui::Align2::LEFT_CENTER,
                text,
                egui::FontId::monospace(10.5),
                theme::TEXT_DIM,
            );

            // Alignment studio overlay (drawn above the layers it compares).
            canvas_tools::studio_overlay(app, ui, canvas_rect);
        });
}

fn shrink(rect: Rect, by: f32) -> Rect {
    let r = rect.shrink(by);
    if r.width() > 20.0 && r.height() > 20.0 {
        r
    } else {
        rect
    }
}

/// Draw the wallpaper image behind the layers, if configured.
fn draw_wallpaper(app: &mut App, ui: &mut Ui, canvas_rect: Rect) {
    let Some(path) = app.persisted.preferences.wallpaper.clone() else {
        return;
    };
    let key = format!("wallpaper-{}", path);
    let texture = ui.ctx().data_mut(|d| {
        d.get_temp::<egui::TextureHandle>(egui::Id::new(&key))
    });
    let texture = match texture {
        Some(t) => t,
        None => {
            let Ok(img) = image::open(&path) else {
                return;
            };
            let rgba = img.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            let handle = ui
                .ctx()
                .load_texture(&key, color, egui::TextureOptions::LINEAR);
            ui.ctx()
                .data_mut(|d| d.insert_temp(egui::Id::new(&key), handle.clone()));
            handle
        }
    };
    let fitted = fit_rect(
        PixelRect {
            x: canvas_rect.left(),
            y: canvas_rect.top(),
            w: canvas_rect.width(),
            h: canvas_rect.height(),
        },
        texture.size()[0] as f32,
        texture.size()[1] as f32,
        FitMode::Fill,
    );
    let rect = Rect::from_min_size(
        Pos2::new(fitted.x, fitted.y),
        Vec2::new(fitted.w, fitted.h),
    );
    let clipped = rect.intersect(canvas_rect);
    ui.painter().with_clip_rect(canvas_rect).image(
        texture.id(),
        clipped,
        egui::Rect::from_min_max(
            egui::pos2(
                (clipped.left() - rect.left()) / rect.width(),
                (clipped.top() - rect.top()) / rect.height(),
            ),
            egui::pos2(
                (clipped.right() - rect.left()) / rect.width(),
                (clipped.bottom() - rect.top()) / rect.height(),
            ),
        ),
        Color32::WHITE,
    );
}

/// Turn the runtime's per-layer frames into GPU draws.
fn build_draws(app: &mut App, canvas_rect: Rect) -> PaintList {
    let canvas_size = Size::new(canvas_rect.width(), canvas_rect.height());
    let studio = app.studio.comparison_for(app);
    let mut list = PaintList::build(&app.session, |id| {
        let layer = app.session.layer(id)?;
        let px = layer_pixel_rect(layer.rect, PixelRect {
            x: canvas_rect.left(),
            y: canvas_rect.top(),
            w: canvas_size.w,
            h: canvas_size.h,
        });
        let rect = Rect::from_min_size(
            Pos2::new(px.x, px.y),
            Vec2::new(px.w.max(1.0), px.h.max(1.0)),
        );
        Some(rect)
    });

    // Attach frames, crops, and opacities. The studio compare is attached to
    // the layer it compares *into* (layer A).
    for paint in &mut list.layers {
        let Some(layer) = app.session.layer(paint.draw.layer_id) else {
            continue;
        };
        paint.draw.crop = layer.crop;
        paint.draw.opacity = layer.opacity;
        paint.visible = layer.visible;
        if let Some(state) = app.runtime.layers.get(&layer.id)
            && let Some(presented) = state.presented.as_ref()
        {
            paint.draw.upload = Some(FrameUpload {
                frame: std::sync::Arc::new(presented.frame.clone()),
                range: presented.color,
            });
        }
        if let Some((a_id, compare)) = studio.as_ref()
            && *a_id == layer.id
        {
            paint.draw.compare = Some(compare.clone());
        }
    }
    list
}

fn draw_snap_guides(app: &App, ui: &mut Ui, canvas_rect: Rect) {
    for &(vertical, pos) in &app.snap_guides {
        let stroke = Stroke::new(1.0, theme::ACCENT);
        if vertical {
            let x = canvas_rect.left() + pos * canvas_rect.width();
            ui.painter().vline(x, canvas_rect.y_range(), stroke);
        } else {
            let y = canvas_rect.top() + pos * canvas_rect.height();
            ui.painter().hline(canvas_rect.x_range(), y, stroke);
        }
    }
}

fn draw_selection(app: &App, ui: &mut Ui, canvas_rect: Rect) {
    let Some(id) = app.selected else {
        return;
    };
    let Some(layer) = app.session.layer(id) else {
        return;
    };
    let px = layer_pixel_rect(
        layer.rect,
        PixelRect {
            x: canvas_rect.left(),
            y: canvas_rect.top(),
            w: canvas_rect.width(),
            h: canvas_rect.height(),
        },
    );
    let rect = Rect::from_min_size(
        Pos2::new(px.x, px.y),
        Vec2::new(px.w.max(1.0), px.h.max(1.0)),
    );
    ui.painter().rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, theme::ACCENT),
        StrokeKind::Inside,
    );
    if layer.locked || app.lock_layout {
        return;
    }
    for handle in Handle::ALL {
        let (hx, hy) = handle_position(rect, handle);
        let center = Pos2::new(hx, hy);
        let handle_rect = Rect::from_center_size(center, Vec2::splat(9.0));
        ui.painter().rect_filled(handle_rect, 0.0, theme::ACCENT);
    }
}

/// Where one handle sits on a pixel rect.
pub fn handle_position(rect: Rect, handle: Handle) -> (f32, f32) {
    let (l, c, r, t, m, b) = (
        rect.left(),
        rect.center().x,
        rect.right(),
        rect.top(),
        rect.center().y,
        rect.bottom(),
    );
    match handle {
        Handle::TopLeft => (l, t),
        Handle::Top => (c, t),
        Handle::TopRight => (r, t),
        Handle::Right => (r, m),
        Handle::BottomRight => (r, b),
        Handle::Bottom => (c, b),
        Handle::BottomLeft => (l, b),
        Handle::Left => (l, m),
    }
}

fn interactions(app: &mut App, ui: &mut Ui, canvas_rect: Rect, response: &egui::Response) {
    let Some(pointer) = response.interact_pointer_pos() else {
        if response.drag_stopped() {
            app.drag = None;
        }
        return;
    };
    let norm = (
        ((pointer.x - canvas_rect.left()) / canvas_rect.width()).clamp(0.0, 1.0),
        ((pointer.y - canvas_rect.top()) / canvas_rect.height()).clamp(0.0, 1.0),
    );

    if response.drag_started() {
        app.snap_guides.clear();
        // A drag on a handle of the selected layer resizes; anywhere else
        // inside a layer moves it; empty space starts a marquee-free deselect.
        let mut started = None;
        if let Some(id) = app.selected
            && app.session.layer(id).is_some_and(|l| !l.locked)
            && !app.lock_layout
        {
            let layer = app.session.layer(id).cloned().unwrap();
            let px = layer_pixel_rect(
                layer.rect,
                PixelRect {
                    x: canvas_rect.left(),
                    y: canvas_rect.top(),
                    w: canvas_rect.width(),
                    h: canvas_rect.height(),
                },
            );
            let rect = Rect::from_min_size(
                Pos2::new(px.x, px.y),
                Vec2::new(px.w.max(1.0), px.h.max(1.0)),
            );
            let px_pos = Pos2::new(
                canvas_rect.left() + norm.0 * canvas_rect.width(),
                canvas_rect.top() + norm.1 * canvas_rect.height(),
            );
            if let Some(handle) = Handle::hit_test(
                PixelRect {
                    x: rect.left(),
                    y: rect.top(),
                    w: rect.width(),
                    h: rect.height(),
                },
                px_pos.x,
                px_pos.y,
                7.0,
            ) {
                started = Some((id, Some(handle), layer.rect));
            } else if layer.rect.contains(norm.0, norm.1) {
                started = Some((id, None, layer.rect));
            }
        }
        if started.is_none()
            && let Some(hit) = app.session.hit_test(norm.0, norm.1)
        {
            let rect = app
                .session
                .layer(hit)
                .map(|l| l.rect)
                .unwrap_or_else(NormalizedRect::full);
            app.selected = Some(hit);
            started = Some((hit, None, rect));
        }
        if let Some((id, handle, start_rect)) = started {
            app.selected = Some(id);
            app.offset_target = crate::app::OffsetTarget::Layer(id);
            app.drag = Some(crate::app::DragState {
                layer_id: id,
                handle,
                start_pos: norm,
                start_rect,
                moved: false,
            });
        } else {
            app.selected = None;
        }
    }

    if response.dragged()
        && let Some(drag) = app.drag
    {
        let dx = norm.0 - drag.start_pos.0;
        let dy = norm.1 - drag.start_pos.1;
        let moved = (dx.abs() + dy.abs()) > 0.0015;
        // Shift locks the aspect ratio when resizing by a corner.
        let aspect = ui
            .input(|i| i.modifiers.shift)
            .then(|| drag.start_rect.w / drag.start_rect.h.max(1e-6));
        let new_rect = match drag.handle {
            None => {
                let mut r = drag.start_rect;
                r.x = (drag.start_rect.x + dx).clamp(0.0, 1.0 - r.w);
                r.y = (drag.start_rect.y + dy).clamp(0.0, 1.0 - r.h);
                r
            }
            Some(handle) => Handle::resize(handle, drag.start_rect, dx, dy, aspect, 0.02),
        };
        // Snap edges/centres while dragging.
        let snapped = canvas_tools::apply_snapping(
            app,
            new_rect,
            drag.handle.is_none(),
            crate::ui::screens::canvas_tools::SnapTargets::default(),
        );
        app.snap_guides = snapped.1;
        if let Some(layer) = app.session.layer_mut(drag.layer_id) {
            layer.rect = snapped.0.clamped_to_canvas().with_min_size(0.02);
        }
        if let Some(d) = app.drag.as_mut() {
            d.moved = d.moved || moved;
        }
        ui.ctx().request_repaint();
    }

    if response.drag_stopped() {
        if let Some(drag) = app.drag.take()
            && drag.moved
        {
            app.session.normalize_z();
            app.save_current_session();
        }
        app.snap_guides.clear();
    }

    // A plain click inside the canvas (no drag) selects with the top-most
    // layer under the cursor, skipping hidden and locked layers.
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let x = (pos.x - canvas_rect.left()) / canvas_rect.width();
        let y = (pos.y - canvas_rect.top()) / canvas_rect.height();
        if let Some(hit) = app.session.hit_test(x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)) {
            app.selected = Some(hit);
            app.offset_target = crate::app::OffsetTarget::Layer(hit);
        }
    }

    // Right click opens the layer context menu.
    if response.secondary_clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let x = (pos.x - canvas_rect.left()) / canvas_rect.width();
        let y = (pos.y - canvas_rect.top()) / canvas_rect.height();
        if let Some(hit) = app.session.hit_test(x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)) {
            app.selected = Some(hit);
            app.context_layer = Some(hit);
        }
    }
}

/// "1:23.456" from microseconds.
pub fn format_duration(us: i64) -> String {
    let sign = if us < 0 { "-" } else { "" };
    let us = us.abs();
    let total_ms = us / 1_000;
    let minutes = total_ms / 60_000;
    let seconds = (total_ms / 1_000) % 60;
    let millis = total_ms % 1_000;
    format!("{sign}{minutes}:{seconds:02}.{millis:03}")
}

/// The hardware badge text for one backend.
pub fn backend_badge(backend: DecodeBackend, converted: bool) -> &'static str {
    if converted {
        return "converted";
    }
    backend.badge()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_formats_with_milliseconds() {
        assert_eq!(format_duration(0), "0:00.000");
        assert_eq!(format_duration(1_500_000), "0:01.500");
        assert_eq!(format_duration(83_456_000), "1:23.456");
        assert_eq!(format_duration(-500_000), "-0:00.500");
        assert_eq!(format_duration(3_600_000_000), "60:00.000");
    }

    #[test]
    fn handles_sit_on_the_rect_corners_and_edges() {
        let rect = Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(110.0, 220.0));
        assert_eq!(handle_position(rect, Handle::TopLeft), (10.0, 20.0));
        assert_eq!(handle_position(rect, Handle::BottomRight), (110.0, 220.0));
        assert_eq!(handle_position(rect, Handle::Top), (60.0, 20.0));
        assert_eq!(handle_position(rect, Handle::Left), (10.0, 120.0));
        assert_eq!(handle_position(rect, Handle::Right), (110.0, 120.0));
        assert_eq!(handle_position(rect, Handle::Bottom), (60.0, 220.0));
        assert_eq!(handle_position(rect, Handle::TopRight), (110.0, 20.0));
        assert_eq!(handle_position(rect, Handle::BottomLeft), (10.0, 220.0));
    }

    #[test]
    fn converted_badge_wins_over_backend() {
        assert_eq!(backend_badge(DecodeBackend::Software, false), "SW");
        assert_eq!(backend_badge(DecodeBackend::Vaapi, false), "VAAPI");
        assert_eq!(backend_badge(DecodeBackend::Vaapi, true), "converted");
    }
}
