//! One module per screen plus the chrome they share.
//!
//! A screen is a function `show(app: &mut App, ui: &mut Ui)`: all the state it
//! draws lives in [`crate::app::App`], so nothing here needs to allocate or own
//! anything beyond the current frame.

pub mod home;
pub mod player;
pub mod presets;
pub mod sessions;
pub mod settings;
pub mod studio;

use eframe::egui::{self, Align, Layout, RichText};

use crate::app::{App, Screen};
use crate::ui::{theme, widgets};

/// The top bar every non-fullscreen screen uses: the title on the left, the
/// navigation on the right, one hairline underneath.
pub fn header(app: &mut App, ui: &mut egui::Ui, title: &str) {
    egui::Panel::top("chrome")
        .exact_size(48.0)
        .show_separator_line(true)
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::symmetric(16, 8)),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(title).color(theme::TEXT).size(15.0).strong());
                ui.add_space(10.0);
                if let Some(session) = app.session.as_ref() {
                    ui.label(
                        RichText::new(&session.name)
                            .color(theme::MUTED)
                            .size(12.5),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (label, screen) in [
                        ("Settings", Screen::Settings),
                        ("Studio", Screen::Studio),
                        ("Presets", Screen::Presets),
                        ("Sessions", Screen::Sessions),
                        ("Home", Screen::Home),
                    ] {
                        let here = app.screen == screen;
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(label)
                                        .color(if here { theme::ACCENT } else { theme::MUTED })
                                        .size(12.0),
                                )
                                .fill(theme::BG)
                                .stroke(if here {
                                    egui::Stroke::new(1.0, theme::ACCENT)
                                } else {
                                    egui::Stroke::NONE
                                })
                                .corner_radius(theme::RADIUS),
                            )
                            .clicked()
                        {
                            if screen == Screen::Studio {
                                app.open_studio(ui.ctx());
                            } else {
                                app.go(screen);
                            }
                        }
                    }
                });
            });
        });
}

/// Paint the wallpaper across a rectangle, darkened so white text stays legible,
/// or plain black when there is none. Never blurred.
pub fn background(app: &App, ui: &egui::Ui, rect: egui::Rect) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, theme::BG);
    if let Some(texture) = app.wallpaper.as_ref() {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(texture.id(), rect, uv, egui::Color32::WHITE);
        // The scrim: 70% black, flat.
        painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(178));
    }
}

/// A centred column of content on a black ground, used by the static screens.
pub fn centered(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.available_height()),
        Layout::top_down(Align::Center),
        add,
    );
}

/// A status line at the bottom of a screen: engine facts worth knowing.
pub fn status_line(app: &App, ui: &mut egui::Ui) {
    let audio = app
        .runtime
        .as_ref()
        .map(|runtime| runtime.audio_status().summary())
        .unwrap_or_else(|| "audio off".to_owned());
    let layers = app.session.as_ref().map(|s| s.layers.len()).unwrap_or(0);
    let text = format!(
        "{} layers · {audio} · ffmpeg {} · ve {}",
        layers,
        ffmpeg_version(),
        env!("CARGO_PKG_VERSION")
    );
    ui.label(RichText::new(text).color(theme::MUTED).size(10.5));
}

/// libavcodec's version as `major.minor.micro`, for the status line.
pub fn ffmpeg_version() -> String {
    let version = ffmpeg_next::codec::version();
    format!(
        "{}.{}.{}",
        version >> 16,
        (version >> 8) & 0xff,
        version & 0xff
    )
}

/// A full-width, left-aligned list row that looks clickable.
pub fn nav_row(ui: &mut egui::Ui, title: &str, subtitle: &str) -> egui::Response {
    let response = ui.add(
        egui::Button::new(
            RichText::new(format!("{title}\n{subtitle}"))
                .color(theme::TEXT)
                .size(12.5),
        )
        .fill(theme::BG)
        .stroke(theme::hairline(theme::LINE))
        .corner_radius(theme::RADIUS)
        .min_size(egui::vec2(ui.available_width(), 52.0)),
    );
    let _ = widgets::timecode(0);
    response
}
