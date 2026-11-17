//! The home screen: the one place with a title treatment, and five ways in.
//!
//! Everything sits on the wallpaper (or black), in a single centred column. The
//! accent appears exactly once: on the button that continues the last session.

use eframe::egui::{self, Align, Layout, RichText};

use super::{background, status_line};
use crate::app::{App, Screen};
use crate::ui::theme;

pub fn show(app: &mut App, ctx: &egui::Context) {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::BG))
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            background(app, ui, rect);

            ui.allocate_ui_with_layout(
                rect.size(),
                Layout::top_down(Align::Center),
                |ui| {
                    ui.add_space((rect.height() * 0.16).clamp(40.0, 160.0));
                    ui.label(
                        RichText::new("REACTION PLAYER")
                            .color(theme::TEXT)
                            .size(46.0)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Frame-accurate multi-stream playback and alignment")
                            .color(theme::MUTED)
                            .size(13.0),
                    );
                    ui.add_space(36.0);

                    let width = 340.0;
                    let has_last = app
                        .settings
                        .last_session
                        .as_ref()
                        .map(|id| app.sessions.iter().any(|summary| &summary.id == id))
                        .unwrap_or(false);

                    if has_last {
                        if ui
                            .add_sized(
                                [width, 38.0],
                                egui::Button::new(
                                    RichText::new("Continue where you left off")
                                        .color(theme::BG)
                                        .size(13.5),
                                )
                                .fill(theme::ACCENT)
                                .stroke(egui::Stroke::new(1.0, theme::ACCENT))
                                .corner_radius(theme::RADIUS),
                            )
                            .clicked()
                        {
                            continue_last(app, ctx);
                        }
                        ui.add_space(8.0);
                    }
                    if ui
                        .add_sized(
                            [width, 38.0],
                            egui::Button::new(
                                RichText::new("New session").color(theme::TEXT).size(13.5),
                            )
                            .fill(theme::BG)
                            .stroke(egui::Stroke::new(
                                1.0,
                                if has_last { theme::LINE } else { theme::ACCENT },
                            ))
                            .corner_radius(theme::RADIUS),
                        )
                        .clicked()
                    {
                        app.new_session_with_files(Vec::new());
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let gap = (width - 2.0 * 166.0 - 8.0) / 2.0;
                        ui.add_space(gap.max(0.0));
                        if ui
                            .add_sized(
                                [166.0, 34.0],
                                egui::Button::new(
                                    RichText::new("Load session").color(theme::TEXT).size(12.5),
                                )
                                .fill(theme::BG)
                                .stroke(theme::hairline(theme::LINE))
                                .corner_radius(theme::RADIUS),
                            )
                            .clicked()
                        {
                            app.refresh_sessions();
                            app.go(Screen::Sessions);
                        }
                        if ui
                            .add_sized(
                                [166.0, 34.0],
                                egui::Button::new(
                                    RichText::new("Presets & Alignment Studio")
                                        .color(theme::TEXT)
                                        .size(12.5),
                                )
                                .fill(theme::BG)
                                .stroke(theme::hairline(theme::LINE))
                                .corner_radius(theme::RADIUS),
                            )
                            .clicked()
                        {
                            app.go(Screen::Presets);
                        }
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let gap = (width - 2.0 * 166.0 - 8.0) / 2.0;
                        ui.add_space(gap.max(0.0));
                        if ui
                            .add_sized(
                                [166.0, 34.0],
                                egui::Button::new(
                                    RichText::new("Demo").color(theme::TEXT).size(12.5),
                                )
                                .fill(theme::BG)
                                .stroke(theme::hairline(theme::LINE))
                                .corner_radius(theme::RADIUS),
                            )
                            .on_hover_text(
                                "Generates two synthetic clips with ffmpeg and opens them, \
                                 with the reaction's flash 1500 ms after the source's.",
                            )
                            .clicked()
                        {
                            app.open_demo(ctx);
                        }
                        if ui
                            .add_sized(
                                [166.0, 34.0],
                                egui::Button::new(
                                    RichText::new("Settings").color(theme::TEXT).size(12.5),
                                )
                                .fill(theme::BG)
                                .stroke(theme::hairline(theme::LINE))
                                .corner_radius(theme::RADIUS),
                            )
                            .clicked()
                        {
                            app.go(Screen::Settings);
                        }
                    });

                    ui.add_space(18.0);
                    if let Some(session) = app.session.as_ref() {
                        ui.label(
                            RichText::new(format!("Open: {}", session.name))
                                .color(theme::MUTED)
                                .size(11.5),
                        );
                    }

                    // Footer pinned to the bottom of the screen.
                    let footer = rect.bottom() - 28.0;
                    let here = ui.cursor().min.y;
                    if footer > here {
                        ui.add_space(footer - here);
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        status_line(app, ui);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(16.0);
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Keyboard shortcuts  ?")
                                            .color(theme::MUTED)
                                            .size(10.5),
                                    )
                                    .fill(theme::BG)
                                    .stroke(egui::Stroke::NONE),
                                )
                                .clicked()
                            {
                                app.sheet = true;
                            }
                        });
                    });
                },
            );
        });
}

fn continue_last(app: &mut App, ctx: &egui::Context) {
    let Some(id) = app.settings.last_session.clone() else {
        app.new_session_with_files(Vec::new());
        return;
    };
    match app.store.load_session(&id) {
        Ok(session) => {
            app.open_session(session, ctx);
            app.go(Screen::Player);
        }
        Err(message) => {
            app.toasts.error(format!("Could not open that session: {message}"));
            app.go(Screen::Sessions);
        }
    }
}
