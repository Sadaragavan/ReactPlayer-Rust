//! The home screen: continue, new session, load, presets, demo, settings.

use eframe::egui::{self, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::app::{App, Screen};
use crate::engine::demo::{self, DemoMedia};
use crate::model::layout::CanvasAspect;
use crate::model::session::Session;
use crate::model::store::generate_id;
use crate::ui::screens::new_session;
use crate::ui::theme;
use crate::ui::widgets;

/// Draw the home screen into the remaining space.
pub fn show(app: &mut App, ui: &mut Ui) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(theme::BG)
                .inner_margin(egui::Margin::same(0)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(48.0);
                    ui.horizontal(|ui| {
                        ui.add_space(48.0);
                        ui.vertical(|ui| {
                            ui.set_max_width(760.0);
                            header(ui);
                            ui.add_space(26.0);
                            actions(app, ui);
                            ui.add_space(36.0);
                            sessions_section(app, ui);
                            ui.add_space(48.0);
                        });
                    });
                });
        });
}

fn header(ui: &mut Ui) {
    ui.label(
        RichText::new("Frame-accurate reactions.")
            .size(34.0)
            .strong()
            .color(theme::TEXT),
    );
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Stack up to four videos, trim their offsets to the millisecond, and record \
             the exact moment a reaction lands.",
        )
        .size(13.0)
        .color(theme::TEXT_DIM),
    );
    ui.add_space(10.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(52.0, 2.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::ACCENT);
}

fn actions(app: &mut App, ui: &mut Ui) {
    // "Continue" resumes the last session; "New session" opens the modal.
    let last = app
        .persisted
        .preferences
        .last_session
        .clone()
        .filter(|id| app.sessions.iter().any(|s| &s.id == id));
    let last_name = last
        .as_ref()
        .and_then(|id| app.sessions.iter().find(|s| &s.id == id))
        .map(|s| s.name.clone());

    ui.horizontal_wrapped(|ui| {
        match (&last, &last_name) {
            (Some(id), Some(name)) => {
                if widgets::accent_button(ui, format!("Continue \"{name}\"")).clicked() {
                    let id = id.clone();
                    app.open_session(&id);
                }
            }
            _ => {
                if widgets::accent_button(ui, "New session").clicked() {
                    app.bind_runtime();
                    app.screen = Screen::Editor;
                }
            }
        }
        if widgets::button(ui, "New session…").clicked() {
            app.new_session = Some(new_session::NewSessionForm::default());
        }
        if widgets::button(ui, "Load session…").clicked() {
            import_dialog(app);
        }
        if widgets::button(ui, "Demo").clicked() {
            load_demo(app);
        }
        if widgets::button(ui, "Presets & Alignment").clicked() {
            app.screen = Screen::Editor;
            app.studio.open = true;
        }
        if widgets::button(ui, "Settings").clicked() {
            app.settings_draft = Some(app.persisted.preferences.clone());
            app.show_settings = true;
        }
    });
}

/// Build the demo session from the generated clips.
pub fn load_demo(app: &mut App) {
    let dir = app.store.paths.demo_dir();
    match demo::ensure_demo_media(&dir) {
        Ok(media) => {
            let mut session = Session::new(generate_id(), "Demo", CanvasAspect::Widescreen);
            session.layers = demo_layers(&media);
            session.master_audio_layer = Some(session.layers[0].id);
            session.normalize_z();
            app.session = session;
            app.selected = None;
            app.screen = Screen::Editor;
            app.rebuild_union();
            app.save_current_session();
            app.bind_runtime();
            app.notice("Demo ready: reaction flash is offset by +1500 ms");
        }
        Err(e) => app.error(e.to_string()),
    }
}

/// The two demo layers, with the reaction layer offset by the clip's flash
/// difference.
pub fn demo_layers(media: &DemoMedia) -> Vec<crate::model::session::LayerConfig> {
    use crate::model::session::{LayerConfig, LayerRole};
    use crate::model::NormalizedRect;
    let mut source = LayerConfig::new(
        1,
        "Source (testsrc2)".to_string(),
        LayerRole::Source,
        media.source.to_string_lossy().to_string(),
        NormalizedRect::new(0.0, 0.0, 0.5, 1.0),
        0,
    );
    source.fit = crate::model::FitMode::Fit;
    let mut reaction = LayerConfig::new(
        2,
        "Reaction (flash)".to_string(),
        LayerRole::Reaction1,
        media.reaction.to_string_lossy().to_string(),
        NormalizedRect::new(0.5, 0.0, 0.5, 1.0),
        1,
    );
    reaction.fit = crate::model::FitMode::Fit;
    reaction.offset_us = demo::DEMO_REACTION_OFFSET_US;
    vec![source, reaction]
}

fn import_dialog(app: &mut App) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("reaction-player session", &["json"])
        .set_title("Import a .rplay.json session")
        .pick_file()
    else {
        return;
    };
    match app.store.import_session(&path) {
        Ok(session) => {
            app.sessions = app.store.list_sessions();
            app.notice(format!("Imported \"{}\"", session.name));
        }
        Err(e) => app.error(format!("Could not import {}: {e}", path.display())),
    }
}

fn sessions_section(app: &mut App, ui: &mut Ui) {
    widgets::section_label(ui, "Sessions");
    if app.sessions.is_empty() {
        widgets::dim(
            ui,
            "No saved sessions yet. The Demo button builds one from generated clips.",
        );
        return;
    }
    let cards: Vec<(String, String, usize, String)> = app
        .sessions
        .iter()
        .map(|s| {
            (
                s.id.clone(),
                s.name.clone(),
                s.layer_count,
                s.thumbnail.clone().unwrap_or_default(),
            )
        })
        .collect();

    let mut pending: Option<CardAction> = None;
    let thumb_dir = app.store.paths.thumbnails_dir();
    for (id, name, layers, thumb) in cards {
        let mut action = None;
        card(ui, &id, &name, layers, &thumb, &thumb_dir, &mut action);
        if let Some(a) = action {
            pending = Some(a);
        }
    }
    if let Some(action) = pending {
        match action {
            CardAction::Open(id) => app.open_session(&id),
            CardAction::Duplicate(id) => match app.store.duplicate_session(&id) {
                Ok(s) => {
                    app.sessions = app.store.list_sessions();
                    app.notice(format!("Duplicated as \"{}\"", s.name));
                }
                Err(e) => app.error(format!("Could not duplicate: {e}")),
            },
            CardAction::Delete(id) => match app.store.delete_session(&id) {
                Ok(()) => {
                    app.sessions = app.store.list_sessions();
                    if app.persisted.preferences.last_session.as_deref() == Some(id.as_str()) {
                        app.persisted.preferences.last_session = None;
                        app.save_preferences();
                    }
                    app.notice("Session deleted");
                }
                Err(e) => app.error(format!("Could not delete: {e}")),
            },
            CardAction::Export(id) => export_session(app, &id),
        }
    }
}

enum CardAction {
    Open(String),
    Duplicate(String),
    Delete(String),
    Export(String),
}

fn card(
    ui: &mut Ui,
    id: &str,
    name: &str,
    layers: usize,
    thumb: &str,
    thumb_dir: &std::path::Path,
    action: &mut Option<CardAction>,
) {
    let width = ui.available_width().min(760.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 74.0), Sense::click());
    let hovered = response.hovered();
    ui.painter().rect_filled(
        rect,
        0.0,
        if hovered {
            theme::SURFACE_HOVER
        } else {
            theme::SURFACE
        },
    );
    ui.painter().rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, if hovered { theme::TEXT_DIM } else { theme::BORDER }),
        StrokeKind::Inside,
    );

    // Thumbnail, if one was generated.
    let thumb_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 10.0, rect.top() + 10.0),
        Vec2::new(94.0, 54.0),
    );
    let thumb_path = if thumb.is_empty() {
        None
    } else {
        Some(thumb_dir.join(thumb))
    };
    let mut drew_thumb = false;
    if let Some(path) = thumb_path
        && path.exists()
        && let Ok(img) = image::open(&path)
    {
        let rgba = img.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
        let texture = ui
            .ctx()
            .load_texture(&format!("thumb-{thumb}"), color, Default::default());
        ui.painter().image(
            texture.id(),
            thumb_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        drew_thumb = true;
    }
    if !drew_thumb {
        ui.painter().rect_filled(thumb_rect, 0.0, theme::BG);
        ui.painter().rect_stroke(
            thumb_rect,
            0.0,
            Stroke::new(1.0, theme::BORDER),
            StrokeKind::Inside,
        );
        ui.painter().text(
            thumb_rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{layers}L"),
            egui::FontId::monospace(13.0),
            theme::TEXT_DIM,
        );
    }

    ui.painter().text(
        egui::pos2(thumb_rect.right() + 14.0, rect.top() + 24.0),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(14.0),
        theme::TEXT,
    );
    ui.painter().text(
        egui::pos2(thumb_rect.right() + 14.0, rect.top() + 46.0),
        egui::Align2::LEFT_CENTER,
        if layers == 1 {
            "1 layer".to_string()
        } else {
            format!("{layers} layers")
        },
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
    );

    // Row actions on the right.
    let mut x = rect.right() - 10.0;
    for (label, act) in [
        ("✕", 0u8),
        ("Export", 1),
        ("Copy", 2),
        ("Open", 3),
    ] {
        let galley = ui.painter().layout_no_wrap(
            label.to_string(),
            egui::FontId::proportional(12.0),
            theme::TEXT,
        );
        let w = galley.size().x + 16.0;
        let button_rect = egui::Rect::from_min_size(
            egui::pos2(x - w, rect.center().y - 12.0),
            Vec2::new(w, 24.0),
        );
        let resp = ui.interact(
            button_rect,
            ui.scope_id().with((id, act)),
            Sense::click(),
        );
        ui.painter().rect_stroke(
            button_rect,
            0.0,
            Stroke::new(
                1.0,
                if resp.hovered() {
                    theme::ACCENT
                } else {
                    theme::BORDER
                },
            ),
            StrokeKind::Inside,
        );
        ui.painter().text(
            button_rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            if resp.hovered() {
                theme::ACCENT
            } else {
                theme::TEXT
            },
        );
        if resp.clicked() {
            *action = Some(match act {
                3 => CardAction::Open(id.to_string()),
                2 => CardAction::Duplicate(id.to_string()),
                1 => CardAction::Export(id.to_string()),
                _ => CardAction::Delete(id.to_string()),
            });
        }
        x -= w + 6.0;
    }

    // Double-click anywhere on the card opens it.
    if response.double_clicked() {
        *action = Some(CardAction::Open(id.to_string()));
    }
}

fn export_session(app: &mut App, id: &str) {
    let session = match app.store.load_session(id) {
        Ok(s) => s,
        Err(e) => {
            app.error(format!("Could not read the session: {e}"));
            return;
        }
    };
    let Some(dest) = rfd::FileDialog::new()
        .set_file_name(format!("{}.rplay.json", sanitize_name(&session.name)))
        .add_filter("reaction-player session", &["json"])
        .save_file()
    else {
        return;
    };
    match app.store.export_session(&session, &dest) {
        Ok(()) => app.notice(format!("Exported to {}", dest.display())),
        Err(e) => app.error(format!("Could not export: {e}")),
    }
}

/// Make a session name safe for a file name.
pub fn sanitize_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "session".to_string()
    } else {
        trimmed
    }
}

/// Compute where a card's layout sits; exposed for tests.
pub fn card_height() -> f32 {
    74.0
}

/// Kept public for the home screen's tests.
pub fn layout_within(width: f32) -> bool {
    width <= 760.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn demo_layers_offset_the_reaction() {
        let media = DemoMedia::paths_in(std::path::Path::new("/tmp/demo"));
        let layers = demo_layers(&media);
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].offset_us, 0);
        assert_eq!(layers[1].offset_us, demo::DEMO_REACTION_OFFSET_US);
        assert_eq!(layers[0].role, crate::model::LayerRole::Source);
        assert_eq!(layers[1].role, crate::model::LayerRole::Reaction1);
        // The two halves tile the canvas exactly.
        assert!((layers[0].rect.w + layers[1].rect.w - 1.0).abs() < 1e-6);
        assert!((layers[0].rect.x - 0.0).abs() < 1e-6);
        assert!((layers[1].rect.x - 0.5).abs() < 1e-6);
    }

    #[test]
    fn demo_layer_sources_point_into_the_demo_dir() {
        let media = DemoMedia::paths_in(std::path::Path::new("/data/demo"));
        let layers = demo_layers(&media);
        assert_eq!(layers[0].source, "/data/demo/source-testsrc2.mp4");
        assert_eq!(layers[1].source, "/data/demo/reaction-flash.mp4");
    }

    #[test]
    fn names_are_sanitized_for_files() {
        assert_eq!(sanitize_name("My Session"), "My-Session");
        assert_eq!(sanitize_name("  ...  "), "session");
        assert_eq!(sanitize_name("a/b\\c"), "a-b-c");
        assert_eq!(sanitize_name("keep_this-1"), "keep_this-1");
    }

    #[test]
    fn card_layout_constants() {
        assert_eq!(card_height(), 74.0);
        assert!(layout_within(760.0));
        assert!(!layout_within(1000.0));
    }

    #[test]
    fn demo_media_lives_under_data_dir() {
        let media = DemoMedia::paths_in(&PathBuf::from("/home/u/.local/share/reaction-player/demo"));
        assert!(media.source.ends_with("demo/source-testsrc2.mp4"));
    }
}
