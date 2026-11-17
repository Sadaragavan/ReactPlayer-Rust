//! The player: canvas, transport, layers panel.
//!
//! The canvas is an `egui_wgpu` callback: every visible layer becomes one quad,
//! painted in ascending z, with the frame's YUV planes as textures. The only
//! CPU-side per-frame work here is deciding rectangles and UVs; no frame is
//! ever converted or copied.
//!
//! Interaction is done against the same rectangles: hit-testing walks the layers
//! in descending z and skips hidden and locked ones, dragging snaps to the
//! canvas edges, centre lines and other layers, and the eight handles resize
//! with an optional aspect lock.

use std::sync::Arc;

use eframe::egui::{self, Align, Layout, PointerButton, RichText, Sense};

use crate::app::{App, DragState};
use crate::engine::video_frame::VideoFrame;
use crate::gpu::renderer::{CompositorCallback, PixelRect, QuadDesc};
use crate::gpu::renderer::CompareMode as QuadMode;
use crate::model::geom::{
    hit_handle, resize_with_handle, snap_move, Crop, Handle, NormRect, Point, Size, MIN_LAYER_SIZE,
};
use crate::model::session::{CanvasAspect, FitMode, Layer, Session};
use crate::model::timeline;
use crate::ui::{theme, widgets};

/// Where the canvas paints its rectangles.
const HANDLE_SIZE: f32 = 7.0;
const SNAP_TOLERANCE_PX: f32 = 8.0;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    if app.session.is_none() {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(120.0);
                    ui.label(
                        RichText::new("No session open")
                            .color(theme::TEXT)
                            .size(20.0),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Start one from the home screen, or press Demo.")
                            .color(theme::MUTED)
                            .size(12.5),
                    );
                    ui.add_space(16.0);
                    if widgets::primary(ui, "Home").clicked() {
                        app.go(crate::app::Screen::Home);
                    }
                });
            });
        return;
    }

    if app.theater {
        // Theater: canvas and a minimal transport, nothing else. Escape leaves.
        transport(app, ui, true);
        canvas(app, ui);
    } else {
        top_bar(app, ui);
        transport(app, ui, false);
        if app.layers_panel {
            layers_panel(app, ui);
        }
        canvas(app, ui);
    }
}

fn top_bar(app: &mut App, ui: &mut egui::Ui) {
    egui::Panel::top("player-top")
        .exact_size(46.0)
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::symmetric(14, 7)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("← Home").color(theme::MUTED).size(12.0))
                            .fill(theme::BG)
                            .stroke(egui::Stroke::NONE),
                    )
                    .clicked()
                {
                    app.go(crate::app::Screen::Home);
                }
                ui.add_space(4.0);
                // Session name, editable on double click.
                let naming = app.renaming.clone();
                if let Some((id, mut buffer)) = naming.filter(|(id, _)| {
                    app.session.as_ref().map(|s| s.id.as_str()) == Some(id.as_str())
                }) {
                    let response =
                        ui.add(egui::TextEdit::singleline(&mut buffer).desired_width(240.0));
                    let done = response.lost_focus()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if done {
                        if let Some(session) = app.session.as_mut() {
                            if !buffer.trim().is_empty() {
                                session.name = buffer.trim().to_owned();
                                session.touch();
                            }
                        }
                        app.renaming = None;
                        app.save_current();
                    } else {
                        app.renaming = Some((id, buffer));
                    }
                } else {
                    let name = app
                        .session
                        .as_ref()
                        .map(|session| session.name.clone())
                        .unwrap_or_default();
                    let response = ui.label(RichText::new(name).color(theme::TEXT).size(14.0));
                    if response.double_clicked() {
                        if let Some(session) = app.session.as_ref() {
                            app.renaming = Some((session.id.clone(), session.name.clone()));
                        }
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Studio").clicked() {
                        app.open_studio(ui.ctx());
                    }
                    if ui.button("Save").clicked() {
                        app.save_current();
                        app.toasts.info("Saved.");
                    }
                    if ui.button("Presets").clicked() {
                        app.go(crate::app::Screen::Presets);
                    }
                    // Aspect picker.
                    let current = app
                        .session
                        .as_ref()
                        .map(|session| session.canvas_aspect)
                        .unwrap_or(CanvasAspect::Ratio16x9);
                    egui::ComboBox::from_id_salt("canvas-aspect")
                        .selected_text(RichText::new(current.label()).size(11.5))
                        .width(96.0)
                        .show_ui(ui, |ui| {
                            for aspect in CanvasAspect::ALL {
                                let mut selected = current == aspect;
                                if ui
                                    .selectable_label(selected, aspect.label())
                                    .clicked()
                                {
                                    selected = true;
                                    if let Some(session) = app.session.as_mut() {
                                        session.canvas_aspect = aspect;
                                        session.touch();
                                    }
                                }
                                let _ = selected;
                            }
                        });
                });
            });
        });
}

// ---- canvas --------------------------------------------------------------

fn canvas(app: &mut App, ui: &mut egui::Ui) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::same(if app.theater { 0 } else { 12 })),
        )
        .show(ui, |ui| {
            let available = ui.available_rect_before_wrap();
            let aspect = app
                .session
                .as_ref()
                .map(|session| session.canvas_aspect)
                .unwrap_or(CanvasAspect::Ratio16x9);
            let size = aspect.size_in(Size::new(available.width(), available.height()));
            let rect = egui::Rect::from_center_size(
                available.center(),
                egui::vec2(size.w.max(32.0), size.h.max(18.0)),
            );
            let response = ui.allocate_rect(rect, Sense::click_and_drag());
            let ppp = ui.ctx().pixels_per_point();

            // Everything below works in points and converts once, at the end.
            let canvas_size = Size::new(rect.width(), rect.height());
            interact(app, ui, &response, rect, canvas_size);

            draw_frames(app, ui, rect, ppp);
            draw_selection(app, ui, rect);
            draw_guides(app, ui, rect);

            // Right-click menu, on whatever layer is under the cursor.
            let point = ui
                .ctx()
                .input(|input| input.pointer.interact_pos())
                .map(|pos| to_norm(rect, pos));
            if response.clicked_by(PointerButton::Secondary) {
                if let (Some(session), Some(point)) = (app.session.as_ref(), point) {
                    if let Some(index) = session.hit_test(point) {
                        let id = session.layers[index].id.clone();
                        app.select(&id);
                    }
                }
            }
            let _ = response.context_menu(|ui| context_menu(app, ui));
        });
}

/// Pointer handling: select, drag to move, drag a handle to resize.
fn interact(
    app: &mut App,
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    canvas_size: Size,
) {
    let _ = ui;
    let pointer = response.ctx.input(|input| input.pointer.interact_pos());
    let Some(pointer) = pointer else {
        return;
    };
    let local = to_norm(rect, pointer);
    let locked = app.canvas.lock_layout;

    if response.drag_started_by(PointerButton::Primary) && !locked {
        // A handle on the selected layer wins over everything.
        let mut started = false;
        if let Some(layer) = app.selected_layer() {
            let box_px = layer.rect.to_px(canvas_size);
            if !layer.locked {
                if let Some(handle) = hit_handle(box_px, to_px_point(rect, pointer), 10.0) {
                    app.canvas.drag = Some(DragState {
                        layer: layer.id.clone(),
                        handle: Some(handle),
                        start_rect: layer.rect,
                        start: local,
                        moved: false,
                    });
                    started = true;
                }
            }
        }
        if !started {
            if let Some(session) = app.session.as_ref() {
                if let Some(index) = session.hit_test(local) {
                    let layer = &session.layers[index];
                    let id = layer.id.clone();
                    let start_rect = layer.rect;
                    let drag_locked = layer.locked;
                    app.select(&id);
                    if !drag_locked {
                        app.canvas.drag = Some(DragState {
                            layer: id,
                            handle: None,
                            start_rect,
                            start: local,
                            moved: false,
                        });
                    }
                } else {
                    app.selection = None;
                }
            }
        }
    }

    if response.dragged_by(PointerButton::Primary) {
        let Some(drag) = app.canvas.drag.as_ref() else {
            return;
        };
        let delta = Point {
            x: local.x - drag.start.x,
            y: local.y - drag.start.y,
        };
        let handle = drag.handle;
        let start_rect = drag.start_rect;
        let layer_id = drag.layer.clone();
        let aspect_lock = app
            .session
            .as_ref()
            .and_then(|session| session.layer(&layer_id))
            .and_then(|layer| layer.aspect_lock);
        let others: Vec<NormRect> = app
            .session
            .as_ref()
            .map(|session| {
                session
                    .layers
                    .iter()
                    .filter(|layer| layer.id != layer_id && layer.visible)
                    .map(|layer| layer.rect)
                    .collect()
            })
            .unwrap_or_default();
        let tolerance = app.settings.snap_sensitivity_px.max(1.0);

        let (new_rect, guides) = match handle {
            Some(handle) => {
                let delta_px = Point {
                    x: delta.x * canvas_size.w,
                    y: delta.y * canvas_size.h,
                };
                let rect = resize_with_handle(
                    start_rect,
                    handle,
                    delta_px,
                    canvas_size,
                    aspect_lock,
                );
                (rect, (Vec::new(), Vec::new()))
            }
            None => {
                let moved = NormRect {
                    x: start_rect.x + delta.x,
                    y: start_rect.y + delta.y,
                    ..start_rect
                };
                let snap = snap_move(moved, canvas_size, &others, tolerance);
                (snap.rect.clamped(), (snap.guides_x, snap.guides_y))
            }
        };

        app.canvas.guides_x = guides.0;
        app.canvas.guides_y = guides.1;
        if let Some(session) = app.session.as_mut() {
            if let Some(layer) = session.layer_mut(&layer_id) {
                layer.rect = new_rect;
            }
        }
        if let Some(drag) = app.canvas.drag.as_mut() {
            drag.moved = true;
        }
    }

    if response.drag_stopped() {
        if app.canvas.drag.take().is_some() {
            app.canvas.guides_x.clear();
            app.canvas.guides_y.clear();
            if let Some(session) = app.session.as_mut() {
                session.touch();
            }
            app.save_current();
        }
    }

    // Double click on a layer brings it forward one step.
    if response.double_clicked_by(PointerButton::Primary) {
        if let Some(session) = app.session.as_mut() {
            if let Some(index) = session.hit_test(local) {
                let id = session.layers[index].id.clone();
                session.bring_forward(&id);
                session.touch();
                app.selection = Some(id);
            }
        }
    }

    // A click without a drag selects.
    if response.clicked_by(PointerButton::Primary) && app.canvas.drag.is_none() {
        if let Some(session) = app.session.as_ref() {
            if let Some(index) = session.hit_test(local) {
                let id = session.layers[index].id.clone();
                app.select(&id);
            }
        }
    }
}

/// Build and submit the compositor callbacks for this frame.
fn draw_frames(app: &App, ui: &egui::Ui, rect: egui::Rect, ppp: f32) {
    let Some(session) = app.session.as_ref() else {
        return;
    };
    let Some(runtime) = app.runtime.as_ref() else {
        return;
    };
    let clip = PixelRect::from_points(rect, ppp);
    let mut quads: Vec<QuadDesc> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for index in session.paint_order() {
        let layer = &session.layers[index];
        if !layer.visible || layer.opacity <= 0.001 {
            continue;
        }
        let Some(frame) = runtime.frame(&layer.id) else {
            missing.push(layer.name.clone());
            continue;
        };
        let box_rect = egui::Rect::from_min_size(
            egui::pos2(
                rect.left() + layer.rect.x * rect.width(),
                rect.top() + layer.rect.y * rect.height(),
            ),
            egui::vec2(
                layer.rect.w * rect.width(),
                layer.rect.h * rect.height(),
            ),
        );
        let (draw_rect, crop) = fit_rect_and_uv(layer, &frame, box_rect);
        quads.push(QuadDesc {
            key: layer_key(&layer.id),
            rect: PixelRect::from_points(draw_rect, ppp),
            clip,
            crop,
            opacity: layer.opacity.clamp(0.0, 1.0),
            frame,
            other: None,
            compare: QuadMode::Single,
        });
    }

    if !quads.is_empty() {
        ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(
            rect,
            CompositorCallback::new(quads),
        ));
    }

    // Placeholders for layers whose first frame has not landed yet.
    for index in session.paint_order() {
        let layer = &session.layers[index];
        if !layer.visible {
            continue;
        }
        if !missing.iter().any(|name| name == &layer.name) {
            continue;
        }
        let box_rect = egui::Rect::from_min_size(
            egui::pos2(
                rect.left() + layer.rect.x * rect.width(),
                rect.top() + layer.rect.y * rect.height(),
            ),
            egui::vec2(
                layer.rect.w * rect.width(),
                layer.rect.h * rect.height(),
            ),
        );
        ui.painter()
            .rect_filled(box_rect, 0.0, egui::Color32::from_gray(12));
        ui.painter().rect_stroke(
            box_rect,
            0.0,
            theme::hairline(theme::LINE_STRONG),
            egui::StrokeKind::Inside,
        );
        let note = runtime
            .error(&layer.id)
            .unwrap_or_else(|| "opening…".to_owned());
        ui.painter().text(
            box_rect.center(),
            egui::Align2::CENTER_CENTER,
            note,
            egui::FontId::proportional(11.0),
            if runtime.error(&layer.id).is_some() {
                theme::DANGER
            } else {
                theme::MUTED
            },
        );
    }
}

/// Selection outline plus the eight resize handles.
fn draw_selection(app: &App, ui: &egui::Ui, rect: egui::Rect) {
    let Some(session) = app.session.as_ref() else {
        return;
    };
    let painter = ui.painter();
    let Some(id) = app.selection.as_ref() else {
        return;
    };
    let Some(layer) = session.layer(id) else {
        return;
    };
    let box_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.left() + layer.rect.x * rect.width(),
            rect.top() + layer.rect.y * rect.height(),
        ),
        egui::vec2(
            layer.rect.w * rect.width(),
            layer.rect.h * rect.height(),
        ),
    );
    painter.rect_stroke(
        box_rect,
        0.0,
        egui::Stroke::new(1.0, theme::ACCENT),
        egui::StrokeKind::Outside,
    );
    for handle in Handle::ALL {
        let (ax, ay) = handle.anchor();
        let centre = egui::pos2(
            box_rect.left() + ax * box_rect.width(),
            box_rect.top() + ay * box_rect.height(),
        );
        let handle_rect =
            egui::Rect::from_center_size(centre, egui::vec2(HANDLE_SIZE, HANDLE_SIZE));
        painter.rect_filled(handle_rect, 0.0, theme::ACCENT);
    }
    // Name, bottom-left, so a crowded canvas stays readable.
    painter.text(
        egui::pos2(box_rect.left() + 2.0, box_rect.bottom() - 2.0),
        egui::Align2::LEFT_BOTTOM,
        &layer.name,
        egui::FontId::proportional(10.5),
        theme::ACCENT,
    );
}

/// Snap guides, 1px accent lines.
fn draw_guides(app: &App, ui: &egui::Ui, rect: egui::Rect) {
    let painter = ui.painter();
    for x in &app.canvas.guides_x {
        let px = rect.left() + x * rect.width();
        painter.vline(
            px,
            rect.y_range(),
            egui::Stroke::new(1.0, theme::ACCENT),
        );
    }
    for y in &app.canvas.guides_y {
        let py = rect.top() + y * rect.height();
        painter.hline(
            rect.x_range(),
            py,
            egui::Stroke::new(1.0, theme::ACCENT),
        );
    }
}

fn context_menu(app: &mut App, ui: &mut egui::Ui) {
    let Some(id) = app.selection.clone() else {
        ui.label(RichText::new("No layer selected").color(theme::MUTED).size(11.5));
        return;
    };
    // Snapshot what the menu needs, so every button below can borrow the
    // session mutably without fighting the borrow checker.
    let Some(snapshot) = app.session.as_ref().and_then(|session| session.layer(&id)) else {
        return;
    };
    let name = snapshot.name.clone();
    let mut crop = snapshot.crop;
    let mut opacity = snapshot.opacity;
    let mut fit = snapshot.fit;
    let mut locked = snapshot.locked;
    let mut visible = snapshot.visible;
    let master = app
        .session
        .as_ref()
        .and_then(|session| session.master_audio.clone());
    let is_master = master.as_deref() == Some(id.as_str());

    ui.label(RichText::new(name).color(theme::TEXT).size(12.5).strong());
    ui.separator();

    if ui.button("Copy this frame").clicked() {
        app.copy_frame();
        ui.close();
    }
    ui.separator();
    if ui.button("Bring to front").clicked() {
        if let Some(session) = app.session.as_mut() {
            session.bring_to_front(&id);
            session.touch();
        }
        ui.close();
    }
    if ui.button("Bring forward").clicked() {
        if let Some(session) = app.session.as_mut() {
            session.bring_forward(&id);
            session.touch();
        }
        ui.close();
    }
    if ui.button("Send backward").clicked() {
        if let Some(session) = app.session.as_mut() {
            session.send_backward(&id);
            session.touch();
        }
        ui.close();
    }
    if ui.button("Send to back").clicked() {
        if let Some(session) = app.session.as_mut() {
            session.send_to_back(&id);
            session.touch();
        }
        ui.close();
    }
    ui.separator();

    // Crop sliders.
    ui.label(RichText::new("Crop").color(theme::MUTED).size(11.0));
    let before = crop;
    widgets::percent(ui, "Left", &mut crop.left);
    widgets::percent(ui, "Top", &mut crop.top);
    widgets::percent(ui, "Right", &mut crop.right);
    widgets::percent(ui, "Bottom", &mut crop.bottom);
    if crop != before {
        if let Some(layer) = app.session.as_mut().and_then(|s| s.layer_mut(&id)) {
            layer.crop = crop;
        }
    }
    ui.separator();

    if widgets::percent(ui, "Opacity", &mut opacity).changed() {
        if let Some(layer) = app.session.as_mut().and_then(|s| s.layer_mut(&id)) {
            layer.opacity = opacity;
        }
    }

    if ui.button(format!("Fit: {}", fit.label())).clicked() {
        fit = fit.toggled();
        if let Some(layer) = app.session.as_mut().and_then(|s| s.layer_mut(&id)) {
            layer.fit = fit;
        }
    }

    if ui
        .button(if is_master {
            "Master audio: this layer"
        } else {
            "Use as master audio"
        })
        .clicked()
    {
        if let Some(session) = app.session.as_mut() {
            session.master_audio = Some(id.clone());
        }
        ui.close();
    }

    if ui
        .button(if locked { "Unlock layer" } else { "Lock layer" })
        .clicked()
    {
        locked = !locked;
        if let Some(layer) = app.session.as_mut().and_then(|s| s.layer_mut(&id)) {
            layer.locked = locked;
        }
    }

    if ui
        .button(if visible { "Hide layer" } else { "Show layer" })
        .clicked()
    {
        visible = !visible;
        if let Some(layer) = app.session.as_mut().and_then(|s| s.layer_mut(&id)) {
            layer.visible = visible;
        }
    }

    ui.separator();
    if ui.button("Reset transform").clicked() {
        if let Some(layer) = app.session.as_mut().and_then(|s| s.layer_mut(&id)) {
            layer.rect = NormRect::FULL;
            layer.crop = Crop::ZERO;
            layer.opacity = 1.0;
            layer.aspect_lock = None;
        }
        ui.close();
    }
    if ui.button("Remove layer").clicked() {
        if let Some(session) = app.session.as_mut() {
            session.layers.retain(|layer| layer.id != id);
            session.normalize_z();
            session.touch();
        }
        app.selection = None;
        app.sync_engine();
        ui.close();
    }
}

// ---- transport -----------------------------------------------------------

fn transport(app: &mut App, ui: &mut egui::Ui, theater: bool) {
    let height = if theater { 64.0 } else { 104.0 };
    egui::Panel::bottom("transport")
        .exact_size(height)
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::symmetric(14, 8)),
        )
        .show(ui, |ui| {
            let (playing, position, union) = match (app.session.as_ref(), app.runtime.as_ref()) {
                (Some(session), Some(runtime)) => (
                    runtime.is_playing(),
                    runtime.position_us(),
                    session.union(),
                ),
                (Some(session), None) => (false, 0, session.union()),
                _ => (false, 0, timeline::Union::default()),
            };

            // ---- scrubber ------------------------------------------
            let scrub = scrubber(ui, app, union, position);
            if let Some(display_us) = scrub {
                app.seek_display(display_us);
            }
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                // Play / pause.
                let label = if playing { "❚❚" } else { "▶" };
                if ui
                    .add_sized(
                        [46.0, 28.0],
                        egui::Button::new(RichText::new(label).color(theme::BG).size(13.0))
                            .fill(theme::ACCENT)
                            .stroke(egui::Stroke::new(1.0, theme::ACCENT))
                            .corner_radius(theme::RADIUS),
                    )
                    .clicked()
                {
                    app.toggle_play();
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(widgets::timecode(union.display_us(position)))
                        .color(theme::TEXT)
                        .size(12.0)
                        .monospace(),
                );
                ui.label(
                    RichText::new(format!("/ {}", widgets::timecode(union.duration_us())))
                        .color(theme::MUTED)
                        .size(11.0)
                        .monospace(),
                );
                ui.add_space(10.0);

                // Seek steppers.
                for (label, delta_us, hint) in [
                    ("-100", -100_000, "Back 100 ms"),
                    ("-10", -10_000, "Back 10 ms"),
                    ("-1", -1_000, "Back 1 ms"),
                    ("+1", 1_000, "Forward 1 ms"),
                    ("+10", 10_000, "Forward 10 ms"),
                    ("+100", 100_000, "Forward 100 ms"),
                ] {
                    if ui
                        .add(
                            egui::Button::new(RichText::new(label).color(theme::MUTED).size(10.5))
                                .fill(theme::BG)
                                .stroke(theme::hairline(theme::LINE))
                                .corner_radius(theme::RADIUS)
                                .min_size(egui::vec2(34.0, 24.0)),
                        )
                        .on_hover_text(hint)
                        .clicked()
                    {
                        // A stepper is a relative seek on the union timeline.
                        let target = position + delta_us;
                        app.seek_display(union.display_us(target));
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Window controls.
                    if ui
                        .add(
                            egui::Button::new(RichText::new("⛶").color(theme::MUTED).size(12.0))
                                .fill(theme::BG)
                                .stroke(theme::hairline(theme::LINE))
                                .corner_radius(theme::RADIUS),
                        )
                        .on_hover_text("Full screen (F)")
                        .clicked()
                    {
                        let fullscreen =
                            ui.ctx().input(|input| input.viewport().fullscreen.unwrap_or(false));
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                    }
                    let theater_label = if app.theater { "Exit theater" } else { "Theater" };
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(theater_label).color(theme::MUTED).size(11.0),
                            )
                            .fill(theme::BG)
                            .stroke(theme::hairline(theme::LINE))
                            .corner_radius(theme::RADIUS)
                            .min_size(egui::vec2(78.0, 24.0)),
                        )
                        .clicked()
                    {
                        app.theater = !app.theater;
                    }
                    if !theater {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Layers").color(if app.layers_panel {
                                        theme::TEXT
                                    } else {
                                        theme::MUTED
                                    })
                                    .size(11.0),
                                )
                                .fill(if app.layers_panel {
                                    theme::FILL_ACTIVE
                                } else {
                                    theme::BG
                                })
                                .stroke(theme::hairline(if app.layers_panel {
                                    theme::ACCENT
                                } else {
                                    theme::LINE
                                }))
                                .corner_radius(theme::RADIUS)
                                .min_size(egui::vec2(60.0, 24.0)),
                            )
                            .on_hover_text("Layers panel (P)")
                            .clicked()
                        {
                            app.layers_panel = !app.layers_panel;
                        }
                    }
                    // Link toggle.
                    let linked = app
                        .session
                        .as_ref()
                        .map(|session| session.is_linked)
                        .unwrap_or(true);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(if linked { "Linked" } else { "Unlinked" })
                                    .color(if linked { theme::ACCENT } else { theme::MUTED })
                                    .size(11.0),
                            )
                            .fill(theme::BG)
                            .stroke(theme::hairline(if linked { theme::ACCENT } else { theme::LINE }))
                            .corner_radius(theme::RADIUS)
                            .min_size(egui::vec2(70.0, 24.0)),
                        )
                        .on_hover_text("Link the timelines (L)")
                        .clicked()
                    {
                        if let Some(session) = app.session.as_mut() {
                            session.is_linked = !session.is_linked;
                        }
                    }
                    // Master volume.
                    let mut volume = app
                        .session
                        .as_ref()
                        .and_then(|session| session.master())
                        .map(|layer| layer.volume)
                        .unwrap_or(1.0);
                    if widgets::slider(ui, "Vol", &mut volume, 0.0..=1.0, "").changed() {
                        if let Some(session) = app.session.as_mut() {
                            let id = session.master().map(|layer| layer.id.clone());
                            if let Some(layer) = id.and_then(|id| session.layer_mut(&id)) {
                                layer.volume = volume;
                            }
                        }
                    }
                    // Hardware badge for the selected layer.
                    if let Some(layer) = app.selected_layer() {
                        let backend = app
                            .runtime
                            .as_ref()
                            .map(|runtime| runtime.backend(&layer.id))
                            .unwrap_or(crate::engine::HwBackend::Software);
                        let converted = app
                            .runtime
                            .as_ref()
                            .map(|runtime| runtime.converted(&layer.id))
                            .unwrap_or(false);
                        let text = if converted {
                            format!("{} · converted", backend.label())
                        } else {
                            backend.label().to_owned()
                        };
                        widgets::badge(ui, &text, backend.is_hardware());
                    }
                });
            });
        });
}

/// The scrubber: a playhead over the union timeline with one span per layer.
/// Returns a display position when the user scrubs.
fn scrubber(
    ui: &mut egui::Ui,
    app: &App,
    union: timeline::Union,
    position: i64,
) -> Option<i64> {
    let height = 26.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::BG);
    painter.rect_stroke(
        rect,
        0.0,
        theme::hairline(theme::LINE),
        egui::StrokeKind::Inside,
    );

    if let Some(session) = app.session.as_ref() {
        // One thin bar per layer, in paint order, inside the track area.
        let bars = session.layers.len().max(1);
        let bar_h = ((rect.height() - 6.0) / bars as f32).clamp(1.5, 5.0);
        for (row, index) in session.paint_order().into_iter().enumerate() {
            let layer = &session.layers[index];
            let span = timeline::span_of(layer);
            let x0 = rect.left() + union.fraction(span.start_us) * rect.width();
            let x1 = rect.left() + union.fraction(span.end_us) * rect.width();
            let y = rect.top() + 3.0 + row as f32 * bar_h;
            let colour = if app.selection.as_deref() == Some(layer.id.as_str()) {
                theme::ACCENT
            } else if layer.visible {
                theme::LINE_STRONG
            } else {
                theme::LINE
            };
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x0, y), egui::vec2((x1 - x0).max(1.0), bar_h - 1.0)),
                0.0,
                colour,
            );
        }
        // The playhead.
        let x = rect.left() + union.fraction(position) * rect.width();
        painter.vline(x, rect.y_range(), egui::Stroke::new(1.0, theme::ACCENT));
    }

    let pointer = response.interact_pointer_pos()?;
    if response.clicked() || response.dragged() {
        let fraction = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        return Some((fraction as f64 * union.duration_us() as f64) as i64);
    }
    None
}

// ---- layers panel --------------------------------------------------------

fn layers_panel(app: &mut App, ui: &mut egui::Ui) {
    egui::Panel::right("layers")
        .exact_size(268.0)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::same(12))
                .stroke(theme::hairline(theme::LINE)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Layers").color(theme::TEXT).size(13.0).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .small_button(if app.canvas.lock_layout { "Locked" } else { "Unlocked" })
                        .on_hover_text("Lock the layout so rectangles cannot be dragged")
                        .clicked()
                    {
                        app.canvas.lock_layout = !app.canvas.lock_layout;
                    }
                });
            });
            ui.add_space(6.0);
            widgets::hairline(ui);
            ui.add_space(6.0);

            let Some(session) = app.session.as_ref() else {
                return;
            };
            // Topmost first: the panel reads like the stack looks.
            let mut order = session.paint_order();
            order.reverse();
            let rows: Vec<(String, String, bool, bool, bool)> = order
                .iter()
                .map(|index| {
                    let layer = &session.layers[*index];
                    (
                        layer.id.clone(),
                        layer.name.clone(),
                        layer.visible,
                        layer.locked,
                        app.selection.as_deref() == Some(layer.id.as_str()),
                    )
                })
                .collect();
            let count = rows.len();

            for (display_row, (id, name, visible, locked, selected)) in
                rows.into_iter().enumerate()
            {
                let dropped = ui
                    .dnd_drop_zone::<String, _>(
                        egui::Frame::new()
                            .fill(if selected { theme::FILL_ACTIVE } else { theme::BG })
                            .stroke(if selected {
                                egui::Stroke::new(1.0, theme::ACCENT)
                            } else {
                                theme::hairline(theme::LINE)
                            })
                            .corner_radius(theme::RADIUS)
                            .inner_margin(egui::Margin::same(8)),
                        |ui| {
                            ui.set_width(ui.available_width());
                            let id_for_drag = id.clone();
                            ui.dnd_drag_source(
                                egui::Id::new(("layer-drag", id_for_drag.clone())),
                                id_for_drag,
                                |ui| {
                                    ui.horizontal(|ui| {
                                        let colour = if visible { theme::TEXT } else { theme::MUTED };
                                        if ui
                                            .add(
                                                egui::Label::new(
                                                    RichText::new(&name).color(colour).size(12.0),
                                                )
                                                .sense(Sense::click()),
                                            )
                                            .clicked()
                                        {
                                            app.select(&id);
                                        }
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if ui
                                                    .small_button(if visible { "👁" } else { "—" })
                                                    .on_hover_text("Show or hide")
                                                    .clicked()
                                                {
                                                    if let Some(layer) = app
                                                        .session
                                                        .as_mut()
                                                        .and_then(|s| s.layer_mut(&id))
                                                    {
                                                        layer.visible = !layer.visible;
                                                    }
                                                }
                                                if ui
                                                    .small_button(if locked { "🔒" } else { "🔓" })
                                                    .on_hover_text("Lock this layer")
                                                    .clicked()
                                                {
                                                    if let Some(layer) = app
                                                        .session
                                                        .as_mut()
                                                        .and_then(|s| s.layer_mut(&id))
                                                    {
                                                        layer.locked = !layer.locked;
                                                    }
                                                }
                                            },
                                        );
                                    });
                                },
                            );
                        },
                    )
                    .1;

                if let Some(dragged) = dropped {
                    // Dropping onto a row moves the dragged layer to that slot.
                    let target_paint = count - 1 - display_row;
                    if let Some(session) = app.session.as_mut() {
                        reorder(session, &dragged, target_paint);
                        session.touch();
                    }
                }
                ui.add_space(4.0);
            }

            ui.add_space(8.0);
            widgets::hairline(ui);
            ui.add_space(8.0);
            // Per-track facts: resolution, frame rate, offset.
            if let Some(session) = app.session.as_ref() {
                for layer in &session.layers {
                    if app.selection.as_deref() != Some(layer.id.as_str()) {
                        continue;
                    }
                    widgets::kv(ui, "Source", &layer.source.display());
                    widgets::kv(
                        ui,
                        "Size",
                        &format!("{}x{} @ {:.3} fps", layer.width, layer.height, fps(layer)),
                    );
                    widgets::kv(ui, "Offset", &widgets::offset(layer.offset_us));
                    widgets::kv(ui, "Duration", &widgets::timecode(layer.duration_us));
                    if let Some(runtime) = app.runtime.as_ref() {
                        let backend = runtime.backend(&layer.id);
                        widgets::kv(ui, "Decode", backend.label());
                        if let Some(error) = runtime.error(&layer.id) {
                            ui.label(RichText::new(error).color(theme::DANGER).size(11.0));
                        }
                    }
                }
            }
        });
}

/// Move `id` to `target_paint` (0 = back) and re-densify z.
pub fn reorder(session: &mut Session, id: &str, target_paint: usize) {
    let order = session.paint_order();
    let mut ids: Vec<String> = order.iter().map(|i| session.layers[*i].id.clone()).collect();
    let Some(from) = ids.iter().position(|other| other == id) else {
        return;
    };
    let moved = ids.remove(from);
    let to = target_paint.min(ids.len());
    ids.insert(to, moved);
    for (z, layer_id) in ids.iter().enumerate() {
        if let Some(layer) = session.layer_mut(layer_id) {
            layer.z_index = z as i32;
        }
    }
}

// ---- geometry helpers ----------------------------------------------------

/// Maps a point in window coordinates to canvas-relative `0..1` units.
pub fn to_norm(rect: egui::Rect, pos: egui::Pos2) -> Point {
    Point {
        x: ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(-1.0, 2.0),
        y: ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(-1.0, 2.0),
    }
}

fn to_px_point(rect: egui::Rect, pos: egui::Pos2) -> Point {
    Point {
        x: pos.x - rect.left(),
        y: pos.y - rect.top(),
    }
}

/// The rectangle a layer's picture lands in, and the UV rect to sample.
///
/// `Contain` letterboxes inside the layer's box; `Cover` fills the box and
/// widens the sampled region instead. Both respect the layer's crop.
pub fn fit_rect_and_uv(
    layer: &Layer,
    frame: &Arc<VideoFrame>,
    box_rect: egui::Rect,
) -> (egui::Rect, [f32; 4]) {
    let (view_w, view_h) = layer.crop.visible(frame.width.max(1), frame.height.max(1));
    let src_aspect = view_w.max(1) as f32 / view_h.max(1) as f32;
    let box_aspect = (box_rect.width() / box_rect.height().max(1e-3)).max(1e-3);
    match layer.fit {
        FitMode::Contain => {
            let (w, h) = if src_aspect > box_aspect {
                (box_rect.width(), box_rect.width() / src_aspect)
            } else {
                (box_rect.height() * src_aspect, box_rect.height())
            };
            let rect = egui::Rect::from_center_size(box_rect.center(), egui::vec2(w, h));
            (rect, layer.crop.uv())
        }
        FitMode::Cover => (box_rect, cover_uv(layer.crop, src_aspect, box_aspect)),
    }
}

/// Widen a crop so the sampled region has `box_aspect` when drawn `Cover`.
pub fn cover_uv(crop: Crop, src_aspect: f32, box_aspect: f32) -> [f32; 4] {
    let [left, top, right, bottom] = crop.uv();
    let usable_w = (right - left).max(1e-4);
    let usable_h = (bottom - top).max(1e-4);
    let cropped_aspect = src_aspect * usable_w / usable_h;
    if cropped_aspect > box_aspect {
        let keep = (box_aspect / cropped_aspect).clamp(0.0, 1.0);
        let centre = (left + right) * 0.5;
        let half = usable_w * keep * 0.5;
        [centre - half, top, centre + half, bottom]
    } else {
        let keep = (cropped_aspect / box_aspect).clamp(0.0, 1.0);
        let centre = (top + bottom) * 0.5;
        let half = usable_h * keep * 0.5;
        [left, centre - half, right, centre + half]
    }
}

fn fps(layer: &Layer) -> f32 {
    if layer.fps_den == 0 {
        0.0
    } else {
        layer.fps_num as f32 / layer.fps_den as f32
    }
}

/// A stable cache key per layer, so the compositor reuses its textures.
fn layer_key(id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The minimum a layer rectangle may be, exposed for the tooltip on the handles.
pub fn min_size() -> f32 {
    MIN_LAYER_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::video_frame::{ColorRange, FrameFormat, PlaneBuf};
    use crate::model::geom::apply_aspect;
    use crate::model::session::{MediaSource, Role};

    fn frame(w: u32, h: u32) -> Arc<VideoFrame> {
        VideoFrame::from_planes(
            FrameFormat::Nv12,
            w,
            h,
            [
                PlaneBuf::new(vec![0; (w * h) as usize]),
                PlaneBuf::new(vec![128; (w * h / 2) as usize]),
                PlaneBuf::new(Vec::new()),
            ],
            [w as usize, w as usize, 0],
            0,
            ColorRange::Limited,
        )
        .into()
    }

    fn layer(fit: FitMode) -> Layer {
        let mut layer = Layer::new(
            "l",
            Role::Reaction,
            MediaSource::file("/tmp/x.mp4"),
            NormRect::new(0.0, 0.0, 0.5, 0.5),
            0,
        );
        layer.fit = fit;
        layer
    }

    #[test]
    fn contain_letterboxes_a_wide_source_into_a_square_box() {
        let layer = layer(FitMode::Contain);
        let frame = frame(1920, 1080);
        let box_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0));
        let (rect, uv) = fit_rect_and_uv(&layer, &frame, box_rect);
        assert!((rect.width() - 400.0).abs() < 0.01);
        assert!((rect.height() - 225.0).abs() < 0.01);
        assert_eq!(uv, [0.0, 0.0, 1.0, 1.0]);
        assert!((rect.center() - box_rect.center()).length() < 0.01);
    }

    #[test]
    fn contain_pillarboxes_a_tall_source() {
        let layer = layer(FitMode::Contain);
        let frame = frame(1080, 1920);
        let box_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0));
        let (rect, _) = fit_rect_and_uv(&layer, &frame, box_rect);
        assert!((rect.height() - 400.0).abs() < 0.01);
        assert!((rect.width() - 225.0).abs() < 0.01);
    }

    #[test]
    fn cover_fills_the_box_and_crops_the_long_axis() {
        let layer = layer(FitMode::Cover);
        let frame = frame(1920, 1080);
        let box_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0));
        let (rect, uv) = fit_rect_and_uv(&layer, &frame, box_rect);
        assert_eq!(rect, box_rect);
        // A 16:9 source in a square box keeps 9/16 of its width.
        assert!((uv[0] - (0.5 - 0.28125)).abs() < 1e-4, "{uv:?}");
        assert!((uv[2] - (0.5 + 0.28125)).abs() < 1e-4, "{uv:?}");
        assert_eq!(uv[1], 0.0);
        assert_eq!(uv[3], 1.0);
    }

    #[test]
    fn cover_respects_an_existing_crop() {
        let mut layer = layer(FitMode::Cover);
        layer.crop = Crop {
            left: 0.25,
            right: 0.25,
            ..Crop::ZERO
        };
        let frame = frame(1000, 1000);
        let box_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 400.0));
        let (_, uv) = fit_rect_and_uv(&layer, &frame, box_rect);
        // The used width is 0.25..0.75; the box is taller than wide, so the
        // height shrinks, not the width.
        assert!((uv[0] - 0.25).abs() < 1e-4, "{uv:?}");
        assert!((uv[2] - 0.75).abs() < 1e-4, "{uv:?}");
        assert!(uv[1] > 0.0 && uv[3] < 1.0, "{uv:?}");
    }

    #[test]
    fn normalized_points_round_trip() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(200.0, 100.0));
        let point = to_norm(rect, egui::pos2(200.0, 100.0));
        assert!((point.x - 0.5).abs() < 1e-5);
        assert!((point.y - 0.5).abs() < 1e-5);
    }

    #[test]
    fn layer_keys_are_stable_and_distinct() {
        assert_eq!(layer_key("layer_a"), layer_key("layer_a"));
        assert_ne!(layer_key("layer_a"), layer_key("layer_b"));
    }

    #[test]
    fn reordering_densifies_z() {
        let mut session = Session::new("t");
        for (index, name) in ["a", "b", "c"].iter().enumerate() {
            session.layers.push(Layer::new(
                *name,
                Role::Reaction,
                MediaSource::file("/tmp/x.mp4"),
                NormRect::FULL,
                index as i32,
            ));
        }
        let a = session.layers[0].id.clone();
        reorder(&mut session, &a, 2);
        assert_eq!(session.z_of(&a), Some(2));
        let mut z: Vec<i32> = session.layers.iter().map(|layer| layer.z_index).collect();
        z.sort_unstable();
        assert_eq!(z, vec![0, 1, 2]);
        // Dropping outside the range clamps instead of panicking.
        reorder(&mut session, &a, 99);
        assert_eq!(session.z_of(&a), Some(2));
    }

    #[test]
    fn aspect_locked_resize_keeps_the_ratio() {
        let canvas = Size::new(1600.0, 900.0);
        let rect = NormRect::new(0.1, 0.1, 0.4, 0.4);
        let resized = resize_with_handle(
            rect,
            Handle::BottomRight,
            Point { x: 40.0, y: 0.0 },
            canvas,
            Some(16.0 / 9.0),
        );
        assert!((resized.aspect_px(canvas) - 16.0 / 9.0).abs() < 0.01);
    }

    #[test]
    fn aspect_helper_snaps_to_the_target_ratio() {
        let canvas = Size::new(1000.0, 1000.0);
        let rect = NormRect::new(0.0, 0.0, 0.5, 0.5);
        let fitted = apply_aspect(rect, 2.0, canvas);
        assert!((fitted.aspect_px(canvas) - 2.0).abs() < 0.01);
    }
}
