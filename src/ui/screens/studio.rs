//! The Alignment Studio: two captures, stepped independently, compared with an
//! Onion Skin overlap or a Wipe Line, and the difference written into the
//! session as an offset.
//!
//! Sign convention, the same as everywhere else: with A as the reference and B
//! as the other capture, `offset_B = pts_B - pts_A`. Applying it makes
//! `target_pts(B) == pts_B` when the clock reads `pts_A`.

use eframe::egui::{self, Align, Layout, RichText, Sense};

use super::header;
use crate::app::{App, CompareMode, Screen};
use crate::gpu::renderer::{
    CompareMode as QuadMode, CompositorCallback, PixelRect, QuadDesc,
};
use crate::model::geom::Size;
use crate::ui::{theme, widgets};

pub fn show(app: &mut App, ctx: &egui::Context) {
    if app.session.is_none() {
        header(app, ctx, "Alignment Studio");
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(100.0);
                    ui.label(
                        RichText::new("Open a session to align it")
                            .color(theme::TEXT)
                            .size(18.0),
                    );
                    ui.add_space(10.0);
                    if ui.button("Home").clicked() {
                        app.go(Screen::Home);
                    }
                });
            });
        return;
    }

    header(app, ctx, "Alignment Studio");

    // ---- controls ------------------------------------------------------
    egui::TopBottomPanel::top("studio-controls")
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::symmetric(16, 10)),
        )
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                let names: Vec<(String, String)> = app
                    .session
                    .as_ref()
                    .map(|session| {
                        session
                            .layers
                            .iter()
                            .map(|layer| (layer.id.clone(), layer.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();

                ui.label(RichText::new("A (reference)").color(theme::MUTED).size(11.5));
                combo(ui, "studio-a", &mut app.studio.a, &names);
                ui.add_space(8.0);
                ui.label(RichText::new("B (other)").color(theme::MUTED).size(11.5));
                combo(ui, "studio-b", &mut app.studio.b, &names);

                ui.add_space(14.0);
                for (mode, label) in [
                    (CompareMode::Onion, "Onion Skin"),
                    (CompareMode::Wipe, "Wipe Line"),
                ] {
                    let on = app.studio.mode == mode;
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(label)
                                    .color(if on { theme::TEXT } else { theme::MUTED })
                                    .size(12.0),
                            )
                            .fill(if on { theme::FILL_ACTIVE } else { theme::BG })
                            .stroke(if on {
                                egui::Stroke::new(1.0, theme::ACCENT)
                            } else {
                                theme::hairline(theme::LINE)
                            })
                            .corner_radius(theme::RADIUS),
                        )
                        .clicked()
                    {
                        app.studio.mode = mode;
                    }
                }
                ui.add_space(10.0);
                match app.studio.mode {
                    CompareMode::Onion => {
                        let mut mix = app.studio.onion_mix;
                        if widgets::slider(ui, "Overlap", &mut mix, 0.0..=1.0, "").changed() {
                            app.studio.onion_mix = mix;
                        }
                    }
                    CompareMode::Wipe => {
                        let mut percent = app.studio.wipe_percent;
                        if widgets::slider(ui, "Wipe", &mut percent, 0.0..=1.0, "").changed() {
                            app.studio.wipe_percent = percent;
                        }
                    }
                }

                ui.add_space(14.0);
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Capture (C)").color(theme::BG).size(12.5),
                        )
                        .fill(theme::ACCENT)
                        .stroke(egui::Stroke::new(1.0, theme::ACCENT))
                        .corner_radius(theme::RADIUS)
                        .min_size(egui::vec2(110.0, 28.0)),
                    )
                    .clicked()
                {
                    app.studio_capture();
                }
                if ui.button("Reset").clicked() {
                    app.studio.reset();
                }

                // The measurement, and the one button that keeps it.
                if let Some(delta) = app.studio.delta_us() {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(format!("offset_B = {} ms", delta / 1_000))
                            .color(theme::ACCENT)
                            .size(13.0)
                            .strong(),
                    );
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Apply to session").color(theme::BG).size(12.0),
                            )
                            .fill(theme::ACCENT)
                            .stroke(egui::Stroke::new(1.0, theme::ACCENT))
                            .corner_radius(theme::RADIUS)
                            .min_size(egui::vec2(140.0, 28.0)),
                        )
                        .clicked()
                    {
                        app.studio_apply();
                    }
                }
            });
        });

    // ---- panes ---------------------------------------------------------
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::same(16)),
        )
        .show(ctx, |ui| {
            let ids = (app.studio.a.clone(), app.studio.b.clone());
            ui.columns(2, |columns| {
                pane(app, &mut columns[0], ids.0.as_deref(), "A · reference");
                pane(app, &mut columns[1], ids.1.as_deref(), "B · other");
            });
            ui.add_space(10.0);
            widgets::hairline(ui);
            ui.add_space(8.0);

            // The comparison itself, once both sides have been captured.
            match (app.studio.capture_a.clone(), app.studio.capture_b.clone()) {
                (Some(a), Some(b)) => {
                    ui.label(
                        RichText::new(format!(
                            "A at {} · B at {} · {} frames held each",
                            widgets::timecode(a.pts_us),
                            widgets::timecode(b.pts_us),
                            a.frames.len().min(b.frames.len())
                        ))
                        .color(theme::MUTED)
                        .size(11.0),
                    );
                    ui.add_space(6.0);
                    let available = ui.available_rect_before_wrap();
                    let size = Size::new(available.width(), available.height().min(320.0));
                    let rect = egui::Rect::from_min_size(
                        available.min,
                        egui::vec2(size.w, size.h.max(80.0)),
                    );
                    ui.allocate_rect(rect, Sense::hover());
                    if let (Some(frame_a), Some(frame_b)) = (a.newest(), b.newest()) {
                        let compare = match app.studio.mode {
                            CompareMode::Onion => QuadMode::OnionSkin {
                                mix: app.studio.onion_mix,
                            },
                            CompareMode::Wipe => QuadMode::WipeLine {
                                percent: app.studio.wipe_percent,
                            },
                        };
                        let ppp = ctx.pixels_per_point();
                        let quads = vec![QuadDesc {
                            key: stu_dio(),
                            rect: PixelRect::from_points(rect, ppp),
                            clip: PixelRect::from_points(rect, ppp),
                            crop: [0.0, 0.0, 1.0, 1.0],
                            opacity: 1.0,
                            frame: frame_a,
                            other: Some(frame_b),
                            compare,
                        }];
                        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                            rect,
                            CompositorCallback::new(quads),
                        ));
                    }
                }
                _ => {
                    ui.label(
                        RichText::new(
                            "Step each side to the same moment, then press Capture: the last \
                             frames are kept with their exact timestamps so the comparison can \
                             be re-read without touching the decoders.",
                        )
                        .color(theme::MUTED)
                        .size(11.5),
                    );
                }
            }

            if let Some(note) = app.studio.note.clone() {
                ui.add_space(6.0);
                ui.label(RichText::new(note).color(theme::MUTED).size(11.0));
            }
        });
}

fn combo(
    ui: &mut egui::Ui,
    id: &str,
    selected: &mut Option<String>,
    names: &[(String, String)],
) {
    let label = selected
        .as_ref()
        .and_then(|id| names.iter().find(|(layer_id, _)| layer_id == id))
        .map(|(_, name)| name.clone())
        .unwrap_or_else(|| "Choose a layer".to_owned());
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(label).size(11.5))
        .width(150.0)
        .show_ui(ui, |ui| {
            for (layer_id, name) in names {
                let on = selected.as_deref() == Some(layer_id.as_str());
                if ui.selectable_label(on, name).clicked() {
                    *selected = Some(layer_id.clone());
                }
            }
        });
}

/// One side of the comparison: the layer's current frame and its steppers.
fn pane(app: &mut App, ui: &mut egui::Ui, id: Option<&str>, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).color(theme::MUTED).size(11.5));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(id) = id {
                let pts = app
                    .runtime
                    .as_ref()
                    .map(|runtime| runtime.presented_us(id))
                    .unwrap_or(0);
                ui.label(
                    RichText::new(widgets::timecode(pts))
                        .color(theme::MUTED)
                        .size(11.0)
                        .monospace(),
                );
            }
        });
    });
    ui.add_space(4.0);

    let available = ui.available_size();
    let size = Size::new(available.x, (available.y * 0.62).max(120.0));
    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(size.w, size.h));
    ui.allocate_rect(rect, Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::BG);
    ui.painter().rect_stroke(
        rect,
        0.0,
        theme::hairline(theme::LINE),
        egui::StrokeKind::Inside,
    );

    if let Some(id) = id {
        let frame = app
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.frame(id));
        match frame {
            Some(frame) => {
                let ppp = ui.ctx().pixels_per_point();
                let quads = vec![QuadDesc {
                    key: 1,
                    rect: PixelRect::from_points(rect, ppp),
                    clip: PixelRect::from_points(rect, ppp),
                    crop: [0.0, 0.0, 1.0, 1.0],
                    opacity: 1.0,
                    frame,
                    other: None,
                    compare: QuadMode::Single,
                }];
                ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                    rect,
                    CompositorCallback::new(quads),
                ));
            }
            None => {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "opening…",
                    egui::FontId::proportional(11.0),
                    theme::MUTED,
                );
            }
        }
        ui.add_space(6.0);
        let mut step = 0;
        ui.horizontal(|ui| {
            if ui.button("◀ 10").clicked() {
                step = -10;
            }
            if ui.button("◀ 1").clicked() {
                step = -1;
            }
            if ui.button("1 ▶").clicked() {
                step = 1;
            }
            if ui.button("10 ▶").clicked() {
                step = 10;
            }
        });
        if step != 0 {
            app.studio_step(id, step);
        }
    }
}

/// A hash that cannot collide with a layer key: the compare quad has no layer.
const fn stu_dio() -> u64 {
    0xd10a_1700_0000_0001
}
