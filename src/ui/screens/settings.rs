//! The Settings window: hardware mode, seek step, keep-sound-going, snap
//! sensitivity, wallpaper, and the local-data reset.

use eframe::egui::{self, RichText};

use crate::app::App;
use crate::model::session::HardwareAccel;
use crate::ui::theme;
use crate::ui::widgets;

/// Look for the Windows/macOS/Linux defaults a user might want, for the
/// wallpaper picker's hint text.
pub fn wallpaper_hint() -> &'static str {
    "Any image the `image` crate can read (PNG/JPEG/WebP). Fit is Fill."
}

/// Draw the settings window.
pub fn show(app: &mut App, ctx: &egui::Context) {
    if !app.show_settings {
        return;
    }
    let mut open = true;
    let mut draft = app
        .settings_draft
        .clone()
        .unwrap_or_else(|| app.persisted.preferences.clone());
    let mut reset_requested = false;

    egui::Window::new("Settings")
        .id(egui::Id::new("settings-window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(400.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(
            egui::Frame::NONE
                .fill(theme::SURFACE)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .inner_margin(egui::Margin::same(16)),
        )
        .show(ctx, |ui| {
            widgets::section_label(ui, "Decoding");
            ui.horizontal_wrapped(|ui| {
                for mode in HardwareAccel::ALL {
                    if widgets::chip(ui, mode.label(), draft.hardware == mode) {
                        draft.hardware = mode;
                    }
                }
            });
            widgets::dim(ui, mode_help(draft.hardware));
            ui.add_space(10.0);

            widgets::section_label(ui, "Playback");
            ui.horizontal(|ui| {
                widgets::dim(ui, "Seek step");
                let mut ms = draft.seek_step_ms as f64;
                let resp = ui.add(
                    egui::DragValue::new(&mut ms)
                        .speed(100.0)
                        .range(100.0..=600_000.0)
                        .suffix(" ms"),
                );
                if resp.changed() {
                    draft.seek_step_ms = ms.round() as u64;
                }
                widgets::dim(ui, "used by ←/→ while playing");
            });
            ui.add_space(4.0);
            widgets::checkbox(ui, &mut draft.keep_sound_going, "Keep the sound going");
            widgets::dim(
                ui,
                "Lends audio to the highest-z active layer when the master layer has no footage.",
            );
            ui.add_space(10.0);

            widgets::section_label(ui, "Canvas");
            ui.horizontal(|ui| {
                widgets::dim(ui, "Snap sensitivity");
                let mut px = draft.snap_px;
                let resp = ui.add(
                    egui::DragValue::new(&mut px)
                        .speed(0.5)
                        .range(0.0..=64.0)
                        .suffix(" px"),
                );
                if resp.changed() {
                    draft.snap_px = px;
                }
                widgets::dim(ui, "0 disables snapping");
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                widgets::dim(ui, "Wallpaper");
                if widgets::button(ui, "Choose…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("image", &["png", "jpg", "jpeg", "webp", "bmp"])
                        .pick_file()
                {
                    draft.wallpaper = Some(path.to_string_lossy().to_string());
                }
                if widgets::button(ui, "Clear").clicked() {
                    draft.wallpaper = None;
                }
            });
            if let Some(w) = draft.wallpaper.as_ref() {
                widgets::dim(ui, w.clone());
            }
            widgets::dim(ui, wallpaper_hint());
            ui.add_space(12.0);

            widgets::rule(ui);
            ui.add_space(6.0);
            widgets::section_label(ui, "Local data");
            widgets::dim(
                ui,
                format!("{}", app.store.paths.data_dir.display()),
            );
            ui.add_space(4.0);
            if widgets::danger_button(ui, "Clear local app data…").clicked() {
                reset_requested = true;
            }
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                if widgets::accent_button(ui, "Save").clicked() {
                    save(app, &draft);
                }
                if widgets::button(ui, "Close").clicked() {
                    app.show_settings = false;
                    app.settings_draft = None;
                }
            });
        });

    if !open {
        app.show_settings = false;
        app.settings_draft = None;
    } else if app.show_settings {
        app.settings_draft = Some(draft);
    }

    if reset_requested {
        confirm_clear(app, ctx);
    }
}

fn save(app: &mut App, draft: &crate::model::Preferences) {
    let mut prefs = draft.clone().sanitized();
    prefs.last_session = app.persisted.preferences.last_session.clone();
    prefs.window_size = app.persisted.preferences.window_size;
    app.persisted.preferences = prefs.clone();
    app.runtime.apply_preferences(&prefs);
    app.save_preferences();
    // Hardware mode only takes effect on newly opened media, which is honest:
    // re-open the current session so the change is actually applied.
    app.bind_runtime();
    app.notice("Settings saved");
}

fn mode_help(mode: HardwareAccel) -> &'static str {
    match mode {
        HardwareAccel::Auto => "Use hardware decode when it works, silently fall back to software.",
        HardwareAccel::HardwarePreferred => {
            "Try hardware first and keep it even if initialisation is slow; still falls back on failure."
        }
        HardwareAccel::SoftwareOnly => "Never touch a GPU decoder. Also the right choice for debugging.",
    }
}

fn confirm_clear(app: &mut App, ctx: &egui::Context) {
    let mut open = true;
    let mut confirmed = false;
    egui::Window::new("Clear local app data?")
        .id(egui::Id::new("clear-data-confirm"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(
            egui::Frame::NONE
                .fill(theme::SURFACE)
                .stroke(egui::Stroke::new(1.0, theme::ERROR))
                .inner_margin(egui::Margin::same(16)),
        )
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.label(
                RichText::new("This deletes every session, preset, thumbnail, and preference.")
                    .size(13.0)
                    .color(theme::TEXT),
            );
            widgets::dim(ui, "Generated demo clips are removed too. This cannot be undone.");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if widgets::danger_button(ui, "Delete everything").clicked() {
                    confirmed = true;
                }
                if widgets::button(ui, "Cancel").clicked() {
                    confirmed = false;
                }
            });
        });
    if confirmed {
        match app.store.paths.clear() {
            Ok(()) => {
                app.notice("Local app data cleared");
                app.sessions.clear();
                app.persisted = crate::app::PersistedState {
                    preferences: crate::model::Preferences::default(),
                    presets: crate::model::PresetLibrary::default(),
                };
                app.session = crate::model::Session::new(
                    crate::model::store::generate_id(),
                    "Untitled",
                    crate::model::CanvasAspect::Widescreen,
                );
                app.show_settings = false;
                app.settings_draft = None;
                app.screen = crate::app::Screen::Home;
                app.bind_runtime();
            }
            Err(e) => app.error(format!("Could not clear local data: {e}")),
        }
    }
    let _ = open;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_help_text() {
        for m in HardwareAccel::ALL {
            let help = mode_help(m);
            assert!(!help.is_empty());
            assert!(help.ends_with('.'), "{help}");
        }
    }

    #[test]
    fn wallpaper_hint_mentions_the_fit_mode() {
        assert!(wallpaper_hint().contains("Fill"));
    }
}
