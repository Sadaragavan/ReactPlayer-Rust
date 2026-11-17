//! The right-click menu for a layer: copy the cropped frame, reorder, crop
//! sliders, opacity, master audio, lock, and reset.

use eframe::egui::{self, RichText};

use crate::app::App;
use crate::ui::theme;
use crate::ui::widgets;

/// Draw the context menu if one is open.
pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(id) = app.context_layer else {
        return;
    };
    let Some(layer) = app.session.layer(id).cloned() else {
        app.context_layer = None;
        return;
    };

    let mut close = false;
    // Picking a file is deferred until the window has closed, so the file
    // dialog never opens on top of a window that is about to be torn down.
    let mut pending_media: Option<String> = None;
    egui::Window::new(format!("layer-menu-{id}"))
        .id(egui::Id::unique(("layer-menu", id)))
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .default_width(268.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(
            egui::Frame::NONE
                .fill(theme::SURFACE)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .inner_margin(egui::Margin::same(12)),
        )
        .show(ctx, |ui| {
            ui.set_width(268.0);
            ui.label(
                RichText::new(&layer.label)
                    .size(14.0)
                    .strong()
                    .color(theme::TEXT),
            );
            widgets::dim(ui, crate::ui::screens::layers::role_badge(layer.role));
            ui.add_space(8.0);
            widgets::rule(ui);
            ui.add_space(6.0);

            // Media: what the layer is playing, and how to change it. A new
            // session's layers start with an empty source, so this is the only
            // way to give them one without hand-editing the session JSON.
            let source = if layer.source.trim().is_empty() {
                "no media loaded".to_string()
            } else {
                std::path::Path::new(&layer.source)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| layer.source.clone())
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(source).size(11.5).color(theme::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if layer.source.trim().is_empty() {
                        "Choose file…"
                    } else {
                        "Relink…"
                    };
                    if widgets::mini_label(ui, label)
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter(
                                "video",
                                &["mp4", "mkv", "mov", "webm", "avi", "m4v"],
                            )
                            .set_title("Choose a video for this layer")
                            .pick_file()
                    {
                        pending_media = Some(path.to_string_lossy().to_string());
                        close = true;
                    }
                });
            });

            ui.add_space(8.0);
            widgets::section_label(ui, "Order");

            // Copy the cropped frame.
            if widgets::button(ui, "Copy cropped frame").clicked() {
                copy_frame(app, id);
                close = true;
            }
            widgets::dim(ui, "CPU RGBA, BT.709, exactly the visible crop.");

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if widgets::mini_button(ui, "⤒") {
                    app.session.bring_to_front(id);
                    after_reorder(app);
                }
                if widgets::mini_button(ui, "↑") {
                    app.session.bring_forward(id);
                    after_reorder(app);
                }
                if widgets::mini_button(ui, "↓") {
                    app.session.send_backward(id);
                    after_reorder(app);
                }
                if widgets::mini_button(ui, "⤓") {
                    app.session.send_to_back(id);
                    after_reorder(app);
                }
                widgets::dim(ui, "front → back");
            });

            ui.add_space(8.0);
            widgets::section_label(ui, "Crop");
            let mut crop = layer.crop;
            let mut changed = false;
            for (name, value) in [
                ("Left", &mut crop.left),
                ("Right", &mut crop.right),
                ("Top", &mut crop.top),
                ("Bottom", &mut crop.bottom),
            ] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name).size(11.5).color(theme::TEXT_DIM));
                    changed |= ui
                        .add(
                            egui::Slider::new(value, 0.0..=0.9)
                                .show_value(false)
                                .trailing_fill(true),
                        )
                        .changed();
                });
            }
            if changed {
                crop = crop.sanitized();
                if let Some(l) = app.session.layer_mut(id) {
                    l.crop = crop;
                }
                app.save_current_session();
            }

            ui.add_space(8.0);
            widgets::section_label(ui, "Opacity");
            let mut opacity = layer.opacity;
            if ui
                .add(
                    egui::Slider::new(&mut opacity, 0.0..=1.0)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .changed()
                && let Some(l) = app.session.layer_mut(id)
            {
                l.opacity = opacity;
                app.save_current_session();
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let is_master = app.session.master_audio_layer == Some(id);
                let label = if is_master {
                    "Master audio ✓"
                } else {
                    "Make master audio"
                };
                if widgets::chip(ui, label, is_master) && !is_master {
                    app.session.master_audio_layer = Some(id);
                    app.runtime.master_audio = Some(id);
                    app.save_current_session();
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let lock_label = if layer.locked { "Unlock" } else { "Lock" };
                if widgets::chip(ui, lock_label, layer.locked) {
                    if let Some(l) = app.session.layer_mut(id) {
                        l.locked = !l.locked;
                    }
                    app.save_current_session();
                }
                if widgets::chip(ui, if layer.visible { "Hide" } else { "Show" }, false) {
                    if let Some(l) = app.session.layer_mut(id) {
                        l.visible = !l.visible;
                    }
                    app.save_current_session();
                }
            });

            ui.add_space(8.0);
            if widgets::danger_button(ui, "Reset layer").clicked() {
                if let Some(l) = app.session.layer_mut(id) {
                    l.crop = crate::model::CropRect::none();
                    l.opacity = 1.0;
                    l.offset_us = 0;
                    l.rect = crate::model::NormalizedRect::full();
                }
                app.runtime.set_offset(id, 0);
                app.rebuild_union();
                app.save_current_session();
                app.notice("Layer reset");
                close = true;
            }

            ui.add_space(6.0);
            if widgets::mini_label(ui, "Close") {
                close = true;
            }
        });

    if close {
        app.context_layer = None;
    }
    if let Some(source) = pending_media {
        app.load_layer_media(id, source);
    }
}

fn after_reorder(app: &mut App) {
    app.session.normalize_z();
    app.save_current_session();
}

fn copy_frame(app: &mut App, id: u64) {
    let crop = match app.session.layer(id) {
        Some(l) => l.crop,
        None => return,
    };
    match app.runtime.copy_frame_rgba(id, &crop) {
        Ok(img) => {
            let size = [img.width as usize, img.height as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, &img.data);
            app.egui_ctx.copy_image(color);
            app.notice(format!(
                "Copied {}×{} cropped frame to the clipboard",
                img.width, img.height
            ));
        }
        Err(e) => app.error(format!("Could not copy the frame: {e}")),
    }
}

/// Kept for tests: the crop window used when copying a layer's frame.
pub fn copy_window(crop: &crate::model::CropRect, w: u32, h: u32) -> (u32, u32, u32, u32) {
    crop.to_pixel_window(w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_window_respects_the_crop() {
        let crop = crate::model::CropRect {
            left: 0.25,
            top: 0.0,
            right: 0.25,
            bottom: 0.0,
        };
        assert_eq!(copy_window(&crop, 100, 50), (25, 0, 50, 50));
    }

    #[test]
    fn full_crop_copies_everything() {
        assert_eq!(
            copy_window(&crate::model::CropRect::none(), 1920, 1080),
            (0, 0, 1920, 1080)
        );
    }
}
