//! The dismissible message strip that appears under the top bar when
//! something needs the user's attention (missing media, a failed demo, ...).

use eframe::egui::{self, Align, Layout, RichText, Stroke, Ui};

use crate::app::App;
use crate::ui::theme;
use crate::ui::widgets;

/// Draw the banner if one is set.
pub fn show(app: &mut App, ui: &mut Ui) {
    let Some(banner) = app.banner.clone() else {
        return;
    };
    let color = if banner.is_error {
        theme::ERROR
    } else {
        theme::TEXT_DIM
    };
    egui::Panel::top("banner")
        .frame(
            egui::Frame::NONE
                .fill(if banner.is_error {
                    theme::ERROR_DIM
                } else {
                    theme::SURFACE
                })
                .inner_margin(egui::Margin::symmetric(14, 7)),
        )
        .show(ui, |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.label(RichText::new("!").size(13.0).strong().color(color));
                ui.label(RichText::new(&banner.message).size(12.0).color(color));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::mini_button(ui, "×") {
                        app.banner = None;
                    }
                });
            });
            let rect = ui.max_rect();
            ui.painter().hline(
                rect.x_range(),
                rect.bottom(),
                Stroke::new(1.0, theme::BORDER),
            );
        });
}
