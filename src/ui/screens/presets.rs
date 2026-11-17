//! Presets and the way in to the Alignment Studio.
//!
//! A preset is a layout, not a session: applying one moves rectangles, crops,
//! opacity and paint order, and leaves names, offsets and volume alone.

use eframe::egui::{self, Align, Layout, RichText};

use super::header;
use crate::app::App;
use crate::model::presets::LayoutType;
use crate::ui::{theme, widgets};

pub fn show(app: &mut App, ctx: &egui::Context) {
    header(app, ctx, "Presets");

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::same(24)),
        )
        .show(ctx, |ui| {
            let has_session = app.session.is_some();
            ui.columns(2, |columns| {
                // ---- the library -----------------------------------
                let ui = &mut columns[0];
                ui.label(
                    RichText::new("Saved layouts")
                        .color(theme::TEXT)
                        .size(15.0)
                        .strong(),
                );
                ui.add_space(4.0);
                widgets::hairline(ui);
                ui.add_space(8.0);
                if !has_session {
                    ui.label(
                        RichText::new("Open a session to apply a layout to it.")
                            .color(theme::MUTED)
                            .size(11.5),
                    );
                    ui.add_space(6.0);
                }

                let presets = app.presets.clone();
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height() - 12.0)
                    .show(ui, |ui| {
                        let mut delete: Option<String> = None;
                        for preset in &presets {
                            egui::Frame::new()
                                .fill(theme::BG)
                                .stroke(theme::hairline(theme::LINE))
                                .corner_radius(theme::RADIUS)
                                .inner_margin(egui::Margin::same(10))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.label(
                                                RichText::new(&preset.name)
                                                    .color(theme::TEXT)
                                                    .size(13.5),
                                            );
                                            let kind = if preset.built_in {
                                                preset.layout.label().to_owned()
                                            } else {
                                                format!(
                                                    "{} · {} slots · custom",
                                                    preset.layout.label(),
                                                    preset.slots.len()
                                                )
                                            };
                                            ui.label(
                                                RichText::new(kind).color(theme::MUTED).size(11.0),
                                            );
                                        });
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if ui
                                                    .add_enabled(
                                                        has_session,
                                                        egui::Button::new("Apply"),
                                                    )
                                                    .clicked()
                                                {
                                                    app.apply_preset(&preset.id);
                                                }
                                                if !preset.built_in
                                                    && ui.small_button("Delete").clicked()
                                                {
                                                    delete = Some(preset.id.clone());
                                                }
                                            },
                                        );
                                    });
                                });
                            ui.add_space(6.0);
                        }
                        if let Some(id) = delete {
                            app.delete_preset(&id);
                        }
                    });

                // ---- save the current arrangement -------------------
                let ui = &mut columns[1];
                ui.label(
                    RichText::new("Save this arrangement")
                        .color(theme::TEXT)
                        .size(15.0)
                        .strong(),
                );
                ui.add_space(4.0);
                widgets::hairline(ui);
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Captures every layer's rectangle, crop, opacity, fit and paint order.",
                    )
                    .color(theme::MUTED)
                    .size(11.5),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut app.preset_name)
                            .hint_text("Preset name")
                            .desired_width(240.0),
                    );
                    if ui
                        .add_enabled(
                            has_session && !app.preset_name.trim().is_empty(),
                            egui::Button::new("Save"),
                        )
                        .clicked()
                    {
                        let name = app.preset_name.trim().to_owned();
                        app.save_preset(&name);
                        app.preset_name.clear();
                    }
                });

                ui.add_space(18.0);
                ui.label(
                    RichText::new("Alignment Studio")
                        .color(theme::TEXT)
                        .size(15.0)
                        .strong(),
                );
                ui.add_space(4.0);
                widgets::hairline(ui);
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "Put two layers side by side, step each one independently, then compare \
                         them with an Onion Skin overlap or a Wipe Line. The measured difference \
                         becomes the reaction's offset with one click.",
                    )
                    .color(theme::MUTED)
                    .size(11.5),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            has_session,
                            egui::Button::new(
                                RichText::new("Open the studio").color(theme::BG).size(13.0),
                            )
                            .fill(theme::ACCENT)
                            .stroke(egui::Stroke::new(1.0, theme::ACCENT))
                            .corner_radius(theme::RADIUS)
                            .min_size(egui::vec2(160.0, 30.0)),
                        )
                        .clicked()
                    {
                        app.open_studio(ctx);
                    }
                    if let Some(delta) = app.studio.delta_us() {
                        ui.label(
                            RichText::new(format!(
                                "Last measurement: offset_B = {} ms",
                                delta / 1_000
                            ))
                            .color(theme::ACCENT)
                            .size(11.5),
                        );
                    }
                });

                ui.add_space(18.0);
                widgets::kv(
                    ui,
                    "Layout types",
                    &format!("{} built-in", LayoutType::CHOOSABLE.len()),
                );
                widgets::kv(ui, "Presets on this machine", &app.presets.len().to_string());
                widgets::kv(
                    ui,
                    "Sign convention",
                    "offset_B = pts_B - pts_A",
                );
            });
        });
}
