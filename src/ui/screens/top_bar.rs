//! The always-visible top bar: navigation, session identity, and global
//! toggles.

use eframe::egui::{self, Align, Layout, RichText, Stroke, Ui};

use crate::app::{App, Screen};
use crate::ui::theme;
use crate::ui::widgets;

/// Height of the bar.
pub const HEIGHT: f32 = theme::TOP_BAR_HEIGHT;

/// Draw the top bar into a fixed-height panel at the top of `ui`.
pub fn show(app: &mut App, ui: &mut Ui) {
    egui::Panel::top("top-bar")
        .exact_size(HEIGHT)
        .frame(
            egui::Frame::NONE
                .fill(theme::BG)
                .inner_margin(egui::Margin::symmetric(14, 0)),
        )
        .show(ui, |ui| {
            ui.set_height(HEIGHT);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                // Wordmark: the product name in white with the accent dot.
                ui.label(
                    RichText::new("reaction")
                        .size(15.0)
                        .strong()
                        .color(theme::TEXT),
                );
                ui.label(RichText::new("player").size(15.0).color(theme::TEXT_DIM));
                ui.label(RichText::new("•").size(15.0).color(theme::ACCENT));

                ui.add_space(18.0);
                nav_button(app, ui, "Home", Screen::Home);
                nav_button(app, ui, "Editor", Screen::Editor);

                ui.add_space(14.0);
                if app.screen == Screen::Editor {
                    let name = app.session.name.clone();
                    ui.label(
                        RichText::new(name)
                            .size(13.0)
                            .color(theme::TEXT_DIM),
                    );
                }

                // Right-aligned controls.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::icon_toggle(ui, "?", app.show_cheats, "Keyboard shortcuts") {
                        app.show_cheats = !app.show_cheats;
                    }
                    if widgets::icon_toggle(ui, "⚙", app.show_settings, "Settings") {
                        app.show_settings = !app.show_settings;
                        if app.show_settings {
                            app.settings_draft = Some(app.persisted.preferences.clone());
                        }
                    }
                    if app.screen == Screen::Editor {
                        if widgets::icon_toggle(ui, "▤", app.show_layers, "Layers panel") {
                            app.show_layers = !app.show_layers;
                        }
                        if widgets::icon_toggle(ui, "▣", app.theater, "Theater mode") {
                            app.theater = !app.theater;
                        }
                    }
                    // Audio device state: this is a fact about the machine, so
                    // it is shown rather than hidden behind a menu.
                    let (label, accent) = if app.runtime.audio.device_available() {
                        (format!("{} Hz", app.runtime.audio.sample_rate), false)
                    } else {
                        ("no audio device".to_string(), true)
                    };
                    widgets::badge(ui, label, accent);
                });
            });
            // Hairline under the bar.
            let rect = ui.max_rect();
            ui.painter().hline(
                rect.x_range(),
                rect.bottom() - 0.5,
                Stroke::new(1.0, theme::BORDER),
            );

        });
}

fn nav_button(app: &mut App, ui: &mut Ui, label: &str, screen: Screen) {
    let active = app.screen == screen;
    let text = RichText::new(label).size(13.0).color(if active {
        theme::ACCENT
    } else {
        theme::TEXT_DIM
    });
    let response = ui.add(egui::Button::new(text).frame(false));
    if response.clicked() {
        if screen == Screen::Editor && app.screen != Screen::Editor {
            app.bind_runtime();
        }
        app.screen = screen;
    }
    if active {
        let rect = response.rect;
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() + 6.0,
            Stroke::new(1.0, theme::ACCENT),
        );
    }
}
