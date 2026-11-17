//! The session library: cards with a thumbnail, the union length and the layer
//! count, plus rename, duplicate, delete, export and import.

use eframe::egui::{self, Align, Layout, RichText};

use super::header;
use crate::app::{App, Screen};
use crate::ui::{theme, widgets};

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let mut toast: Option<String> = None;
    let ctx = ui.ctx().clone();
    header(app, ui, "Sessions");

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(theme::BG)
                .inner_margin(egui::Margin::same(24)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Button::new(RichText::new("New session").color(theme::BG).size(13.0))
                            .fill(theme::ACCENT)
                            .stroke(egui::Stroke::new(1.0, theme::ACCENT))
                            .corner_radius(theme::RADIUS)
                            .min_size(egui::vec2(140.0, 30.0)),
                    )
                    .clicked()
                {
                    app.new_session_with_files(Vec::new());
                }
                if ui.button("Import .rplay.json…").clicked() {
                    app.import_session(&ctx);
                }
                if ui.button("Refresh").clicked() {
                    app.refresh_sessions();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} saved", app.sessions.len()))
                            .color(theme::MUTED)
                            .size(11.5),
                    );
                });
            });
            ui.add_space(12.0);
            widgets::hairline(ui);
            ui.add_space(12.0);

            if app.sessions.is_empty() {
                // Say what actually works on this machine: the demo needs
                // FFmpeg installed, and pointing at a button that cannot run is
                // worse than saying nothing.
                let hint = if app.ffmpeg_ready {
                    "Nothing saved yet. Start a session, or press Demo on the home screen."
                } else {
                    "Nothing saved yet. Start a session on the home screen, or save a copy of \"Demo\" there once FFmpeg is installed."
                };
                ui.label(RichText::new(hint).color(theme::MUTED).size(12.5));
                if !app.ffmpeg_ready {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("FFmpeg is not on the PATH, so nothing can be decoded yet.")
                            .color(theme::DANGER)
                            .size(11.5),
                    );
                }
                return;
            }

            // Keep the list in its own scroll area so the header stays put.
            egui::ScrollArea::vertical().show(ui, |ui| {
                let summaries = app.sessions.clone();
                for summary in summaries {
                    let selected = app
                        .session
                        .as_ref()
                        .map(|session| session.id == summary.id)
                        .unwrap_or(false);
                    egui::Frame::new()
                        .fill(theme::BG)
                        .stroke(if selected {
                            egui::Stroke::new(1.0, theme::ACCENT)
                        } else {
                            theme::hairline(theme::LINE)
                        })
                        .corner_radius(theme::RADIUS)
                        .inner_margin(egui::Margin::same(12))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                // Thumbnail, or a black box of the same size.
                                let thumb_size = egui::vec2(128.0, 72.0);
                                let (rect, _) =
                                    ui.allocate_exact_size(thumb_size, egui::Sense::hover());
                                ui.painter().rect_filled(rect, 0.0, theme::BG);
                                ui.painter().rect_stroke(
                                    rect,
                                    0.0,
                                    theme::hairline(theme::LINE),
                                    egui::StrokeKind::Inside,
                                );
                                if let Some(path) = summary.thumbnail.as_ref()
                                    && let Some(texture) = thumbnail_texture(&ctx, path) {
                                    let uv = egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    );
                                    ui.painter()
                                        .image(texture.id(), rect, uv, egui::Color32::WHITE);
                                }
                                ui.add_space(12.0);

                                ui.vertical(|ui| {
                                    if let Some((id, name)) = app.renaming.clone() {
                                        if id == summary.id {
                                            let mut buffer = name;
                                            let response = ui.add(
                                                egui::TextEdit::singleline(&mut buffer)
                                                    .desired_width(260.0),
                                            );
                                            let commit = response.lost_focus()
                                                && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                            if commit {
                                                rename(app, &id, &buffer);
                                                app.renaming = None;
                                            } else {
                                                app.renaming = Some((id, buffer));
                                            }
                                        } else {
                                            ui.label(
                                                RichText::new(&summary.name)
                                                    .color(theme::TEXT)
                                                    .size(15.0),
                                            );
                                        }
                                    } else {
                                        ui.label(
                                            RichText::new(&summary.name)
                                                .color(theme::TEXT)
                                                .size(15.0),
                                        );
                                    }
                                    ui.label(
                                        RichText::new(format!(
                                            "{} · {} layers · {}",
                                            widgets::timecode(summary.duration_us),
                                            summary.layer_count,
                                            relative_time(summary.updated_at),
                                        ))
                                        .color(theme::MUTED)
                                        .size(11.5),
                                    );
                                    if summary.missing_media > 0 {
                                        ui.label(
                                            RichText::new(format!(
                                                "{} file(s) missing; open the session to relink",
                                                summary.missing_media
                                            ))
                                            .color(theme::DANGER)
                                            .size(11.5),
                                        );
                                    }
                                });

                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.button("Delete").clicked() {
                                        app.confirm.delete_session = Some(summary.id.clone());
                                    }
                                    if ui.button("Duplicate").clicked() {
                                        duplicate(app, &summary.id);
                                    }
                                    if ui.button("Rename").clicked() {
                                        app.renaming =
                                            Some((summary.id.clone(), summary.name.clone()));
                                    }
                                    if ui.button("Open").clicked() {
                                        match app.store.load_session(&summary.id) {
                                            Ok(session) => {
                                                app.open_session(session, &ctx);
                                                app.go(Screen::Player);
                                            }
                                            Err(message) => toast = Some(message),
                                        }
                                    }
                                });
                            });
                        });
                    ui.add_space(8.0);
                }
            });
        });

    if let Some(message) = toast {
        app.toasts.error(message);
    }
}

fn rename(app: &mut App, id: &str, name: &str) {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        app.toasts.error("A session needs a name.");
        return;
    }
    match app.store.load_session(id) {
        Ok(mut session) => {
            session.name = trimmed.to_owned();
            session.touch();
            if let Err(message) = app.store.save_session(&mut session) {
                app.toasts.error(message);
                return;
            }
            if app.session.as_ref().map(|open| open.id.as_str()) == Some(id)
                && let Some(open) = app.session.as_mut() {
                open.name = trimmed.to_owned();
            }
            app.refresh_sessions();
            app.toasts.info("Renamed.");
        }
        Err(message) => app.toasts.error(message),
    }
}

fn duplicate(app: &mut App, id: &str) {
    match app.store.load_session(id) {
        Ok(session) => {
            let mut copy = session.duplicated();
            match app.store.save_session(&mut copy) {
                Ok(()) => {
                    app.refresh_sessions();
                    app.toasts.success("Duplicated.");
                }
                Err(message) => app.toasts.error(message),
            }
        }
        Err(message) => app.toasts.error(message),
    }
}

/// Load a thumbnail once per path and keep it in egui's texture cache.
fn thumbnail_texture(ctx: &egui::Context, path: &std::path::Path) -> Option<egui::TextureHandle> {
    let key = format!("thumb:{}", path.display());
    if let Some(texture) = ctx.data(|data| data.get_temp::<egui::TextureHandle>(egui::Id::unique(&key)))
    {
        return Some(texture);
    }
    let image = image::open(path).ok()?.to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    let texture = ctx.load_texture(key.clone(), color, egui::TextureOptions::LINEAR);
    ctx.data_mut(|data| data.insert_temp(egui::Id::unique(&key), texture.clone()));
    Some(texture)
}

/// "just now", "12 min ago", "yesterday", then the day count.
pub fn relative_time(then_ms: i64) -> String {
    let now = crate::model::session::now_ms();
    let seconds = ((now - then_ms) / 1_000).max(0);
    match seconds {
        0..=59 => "just now".to_owned(),
        60..=3_599 => format!("{} min ago", seconds / 60),
        3_600..=86_399 => format!("{} h ago", seconds / 3_600),
        86_400..=172_799 => "yesterday".to_owned(),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_read_naturally() {
        let now = crate::model::session::now_ms();
        assert_eq!(relative_time(now), "just now");
        assert_eq!(relative_time(now - 5 * 60_000), "5 min ago");
        assert_eq!(relative_time(now - 3 * 3_600_000), "3 h ago");
        assert_eq!(relative_time(now - 30 * 3_600_000), "yesterday");
        assert_eq!(relative_time(now - 5 * 86_400_000), "5 days ago");
        // A clock that went backwards never divides by something odd.
        assert_eq!(relative_time(now + 10_000), "just now");
    }
}
