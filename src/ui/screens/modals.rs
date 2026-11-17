//! Floating layers drawn above the screens: the New Session modal, the cheat
//! sheet, the Alignment Studio, the Settings window, and the layer context
//! menu.

use eframe::egui::{self, RichText};

use crate::app::App;
use crate::ui::screens::{alignment, context_menu, new_session, settings};
use crate::ui::shortcuts::CHEAT_SHEET;
use crate::ui::theme;
use crate::ui::widgets;

/// Draw every floating element the app has open.
pub fn show_all(app: &mut App, ctx: &egui::Context) {
    new_session::show(app, ctx);
    settings::show(app, ctx);
    alignment::show(app, ctx);
    context_menu::show(app, ctx);
    cheat_sheet(app, ctx);
}

fn cheat_sheet(app: &mut App, ctx: &egui::Context) {
    if !app.show_cheats {
        return;
    }
    let mut open = true;
    egui::Window::new("Keyboard shortcuts")
        .id(egui::Id::new("cheat-sheet"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(
            egui::Frame::NONE
                .fill(theme::SURFACE)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .inner_margin(egui::Margin::same(16)),
        )
        .show(ctx, |ui| {
            for row in CHEAT_SHEET {
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(120.0, 18.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                RichText::new(row.keys)
                                    .font(egui::FontId::monospace(11.5))
                                    .color(theme::ACCENT),
                            );
                        },
                    );
                    ui.label(
                        RichText::new(row.what)
                            .size(12.0)
                            .color(theme::TEXT_DIM),
                    );
                });
            }
            ui.add_space(6.0);
            widgets::dim(
                ui,
                "Shortcuts are ignored while a text field has focus.",
            );
        });
    if !open {
        app.show_cheats = false;
    }
}
