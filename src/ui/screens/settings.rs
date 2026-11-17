//! Preferences: decoding, transport defaults, sound, appearance and the local
//! data escape hatch.

use eframe::egui::{self, RichText};

use super::{ffmpeg_version, header};
use crate::app::App;
use crate::model::settings::HwAccelPref;
use crate::ui::{theme, widgets};

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    header(app, ui, "Settings");
    let mut changed = false;

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::same(24)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.set_max_width(760.0);

                // ---- decoding --------------------------------------
                widgets::section(ui, "Decoding", |ui| {
                    ui.label(
                        RichText::new(
                            "Hardware decoding falls back to software without asking: a file that \
                             the driver refuses simply plays on the CPU, and the layer says so.",
                        )
                        .color(theme::MUTED)
                        .size(11.5),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        for pref in HwAccelPref::ALL {
                            let on = app.settings.hw_accel == pref;
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(pref.label())
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
                                .on_hover_text(pref.description())
                                .clicked()
                            {
                                if app.settings.hw_accel != pref {
                                    app.settings.hw_accel = pref;
                                    changed = true;
                                }
                            }
                        }
                    });
                    ui.add_space(6.0);
                    widgets::kv(ui, "FFmpeg (libavcodec)", &ffmpeg_version());
                    widgets::kv(
                        ui,
                        "Backends tried",
                        "VAAPI, Vulkan, VDPAU (Linux) · D3D11VA, DXVA2 (Windows) · VideoToolbox (macOS)",
                    );
                });

                // ---- transport -------------------------------------
                widgets::section(ui, "Transport", |ui| {
                    ui.horizontal(|ui| {
                        let mut step = app.settings.seek_step_ms as f32 / 1_000.0;
                        ui.label(RichText::new("Seek step").color(theme::MUTED).size(12.0));
                        if ui
                            .add(
                                egui::Slider::new(&mut step, 1.0..=30.0)
                                    .suffix(" s")
                                    .fixed_decimals(1),
                            )
                            .changed()
                        {
                            app.settings.seek_step_ms = (step * 1_000.0).round() as i64;
                            changed = true;
                        }
                    });
                    ui.add_space(4.0);
                    if widgets::chip(
                        ui,
                        app.settings.keep_sound_going,
                        "Keep the sound going",
                    )
                    .on_hover_text(
                        "When the master audio layer has no footage at the playhead, the \
                         highest active layer lends its sound instead of dropping to silence.",
                    )
                    .clicked()
                    {
                        app.settings.keep_sound_going = !app.settings.keep_sound_going;
                        changed = true;
                    }
                    ui.add_space(6.0);
                    let mut volume = app.settings.master_volume;
                    if widgets::slider(ui, "Master volume", &mut volume, 0.0..=1.0, "").changed() {
                        app.settings.master_volume = volume;
                        changed = true;
                    }
                    if let Some(runtime) = app.runtime.as_mut() {
                        if changed {
                            runtime.set_master_volume(app.settings.master_volume);
                        }
                    }
                });

                // ---- editing ---------------------------------------
                widgets::section(ui, "Canvas and editing", |ui| {
                    let mut snap = app.settings.snap_sensitivity_px;
                    if widgets::slider(ui, "Snap sensitivity", &mut snap, 0.0..=24.0, " px")
                        .changed()
                    {
                        app.settings.snap_sensitivity_px = snap;
                        changed = true;
                    }
                    if widgets::chip(ui, app.canvas.lock_layout, "Lock layout")
                        .on_hover_text("Stops every rectangle on the canvas from being dragged.")
                        .clicked()
                    {
                        app.canvas.lock_layout = !app.canvas.lock_layout;
                    }
                    if widgets::chip(ui, app.settings.theater, "Start in theater mode").clicked() {
                        app.settings.theater = !app.settings.theater;
                        changed = true;
                    }
                    if let Some(aspect) = app.session.as_ref().map(|session| session.canvas_aspect) {
                        widgets::kv(ui, "Canvas aspect", aspect.label());
                    }
                });

                // ---- appearance ------------------------------------
                widgets::section(ui, "Appearance", |ui| {
                    let path = app
                        .settings
                        .wallpaper
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "None: plain black".to_owned());
                    widgets::kv(ui, "Wallpaper", &path);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Choose image…").clicked() {
                            app.pick_wallpaper(&ctx);
                        }
                        if ui.button("Reset to black").clicked() {
                            app.settings.wallpaper = None;
                            app.reload_wallpaper(&ctx);
                            changed = true;
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("The wallpaper is never blurred; it is dimmed to 70% black.")
                            .color(theme::MUTED)
                            .size(11.0),
                    );
                });

                // ---- data ------------------------------------------
                widgets::section(ui, "Local data", |ui| {
                    let (files, bytes) = app.store.usage();
                    widgets::kv(ui, "Config", &app.store.config_dir().display().to_string());
                    widgets::kv(ui, "Data", &app.store.data_dir().display().to_string());
                    widgets::kv(
                        ui,
                        "On disk",
                        &format!("{} files, {:.1} MB", files, bytes as f64 / 1_048_576.0),
                    );
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Clear local data…").color(theme::DANGER).size(12.5),
                            )
                            .fill(theme::BG)
                            .stroke(egui::Stroke::new(1.0, theme::DANGER))
                            .corner_radius(theme::RADIUS),
                        )
                        .clicked()
                    {
                        app.confirm.clear_data = true;
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "Reaction Player {}",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .color(theme::MUTED)
                        .size(10.5),
                    );
                });
            });
        });

    if changed {
        app.save_settings();
    }
}
