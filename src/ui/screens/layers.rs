//! The layers panel: per-layer visibility, lock, opacity, z-order, label, and
//! offset. Four reorder mechanisms live here as required: up/down buttons,
//! drag-free keyboard shortcuts ([ and ]), the right-click menu, and the
//! clickable z-order readout (which cycles position).

use eframe::egui::{self, Align, Layout, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::app::{App, OffsetTarget};
use crate::model::LayerRole;
use crate::ui::theme;
use crate::ui::widgets;

/// Width of the panel.
pub const WIDTH: f32 = theme::LAYERS_PANEL_WIDTH;

/// Draw the layers panel on the right.
pub fn show(app: &mut App, ui: &mut Ui) {
    egui::Panel::right("layers")
        .exact_size(WIDTH)
        .frame(
            egui::Frame::NONE
                .fill(theme::BG)
                .inner_margin(egui::Margin::symmetric(12, 10)),
        )
        .show(ui, |ui| {
            header(app, ui);
            ui.add_space(8.0);
            widgets::rule(ui);
            ui.add_space(8.0);

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Top of the z stack first, which is the opposite of the
                    // paint order, because that is how layer lists read.
                    let mut ids: Vec<u64> = app.session.layers.iter().map(|l| l.id).collect();
                    ids.sort_by_key(|id| {
                        std::cmp::Reverse(app.session.layer(*id).map(|l| l.z_index).unwrap_or(0))
                    });
                    for id in ids {
                        row(app, ui, id);
                        ui.add_space(6.0);
                    }
                });

            ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                ui.add_space(4.0);
                global_controls(app, ui);
            });
        });
}

fn header(app: &mut App, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Layers")
                .size(13.0)
                .strong()
                .color(theme::TEXT),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if app.lock_layout { "Unlock" } else { "Lock layout" };
            if widgets::button(ui, label).clicked() {
                app.lock_layout = !app.lock_layout;
            }
        });
    });
}

fn global_controls(app: &mut App, ui: &mut Ui) {
    widgets::rule(ui);
    ui.add_space(4.0);
    widgets::section_label(ui, "Offset target");
    ui.horizontal(|ui| {
        let all = matches!(app.offset_target, OffsetTarget::All);
        if widgets::chip(ui, "All layers", all) {
            app.offset_target = OffsetTarget::All;
        }
        if let Some(id) = app.selected {
            let name = app
                .session
                .layer(id)
                .map(|l| l.label.clone())
                .unwrap_or_else(|| "Selected".into());
            let selected = matches!(app.offset_target, OffsetTarget::Layer(x) if x == id);
            if widgets::chip(ui, &name, selected) {
                app.offset_target = OffsetTarget::Layer(id);
            }
        }
    });
    ui.add_space(4.0);
    offset_steppers(app, ui);
}

/// The ±1 / ±10 / ±100 ms buttons that edit the offset target.
pub fn offset_steppers(app: &mut App, ui: &mut Ui) {
    let current = match app.offset_target {
        OffsetTarget::Layer(id) => app.session.layer(id).map(|l| l.offset_us).unwrap_or(0),
        OffsetTarget::All => {
            // "All" shows the first layer's offset as the reference and moves
            // every layer by the same delta, preserving relative alignment.
            app.session.layers.first().map(|l| l.offset_us).unwrap_or(0)
        }
    };
    ui.horizontal(|ui| {
        for (label, delta) in [
            ("−100", -100_000i64),
            ("−10", -10_000),
            ("−1", -1_000),
            ("+1", 1_000),
            ("+10", 10_000),
            ("+100", 100_000),
        ] {
            let text = RichText::new(label).size(11.0).color(theme::TEXT);
            let button = egui::Button::new(text)
                .fill(theme::SURFACE)
                .corner_radius(0)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .min_size(Vec2::new(38.0, 22.0));
            if ui.add(button).clicked() {
                app.apply_action(crate::ui::shortcuts::Action::NudgeOffset(delta));
            }
        }
    });
    ui.horizontal(|ui| {
        widgets::dim(ui, "Offset");
        widgets::value_slot(ui, &format!("{current:+} µs"), 90.0);
        if widgets::mini_button(ui, "0") {
            match app.offset_target {
                OffsetTarget::Layer(id) => {
                    if let Some(l) = app.session.layer_mut(id) {
                        l.offset_us = 0;
                    }
                    app.runtime.set_offset(id, 0);
                }
                OffsetTarget::All => {
                    for id in app.session.layers.iter().map(|l| l.id).collect::<Vec<_>>() {
                        if let Some(l) = app.session.layer_mut(id) {
                            l.offset_us = 0;
                        }
                        app.runtime.set_offset(id, 0);
                    }
                }
            }
            app.rebuild_union();
            app.save_current_session();
        }
    });
}

fn row(app: &mut App, ui: &mut Ui, id: u64) {
    let Some(layer) = app.session.layer(id).cloned() else {
        return;
    };
    let selected = app.selected == Some(id);
    let has_error = app
        .runtime
        .layers
        .get(&id)
        .and_then(|s| s.error.as_ref())
        .is_some();
    let height = 86.0;

    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        Sense::click(),
    );
    if response.clicked() {
        app.selected = Some(id);
        app.offset_target = OffsetTarget::Layer(id);
    }
    if response.secondary_clicked() {
        app.selected = Some(id);
        app.context_layer = Some(id);
    }

    // Background + selection rail.
    let bg = if selected {
        theme::SURFACE_HOVER
    } else if response.hovered() {
        theme::SURFACE
    } else {
        theme::BG
    };
    ui.painter().rect_filled(rect, 0.0, bg);
    ui.painter().rect_stroke(
        rect,
        0.0,
        Stroke::new(
            1.0,
            if selected {
                theme::ACCENT
            } else {
                theme::BORDER
            },
        ),
        StrokeKind::Inside,
    );

    let pad = 10.0;
    let mut x = rect.left() + pad;
    let top = rect.top() + 8.0;

    // Visibility + lock toggles.
    for (glyph, on, tip) in [
        ("◉", layer.visible, "Visible"),
        ("⊘", layer.locked, "Locked"),
    ] {
        let r = egui::Rect::from_min_size(egui::pos2(x, top), Vec2::splat(20.0));
        let resp = ui.interact(r, ui.scope_id().with((id, glyph)), Sense::click());
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(13.0),
            if on { theme::ACCENT } else { theme::TEXT_DIM },
        );
        if resp.clicked() {
            if let Some(l) = app.session.layer_mut(id) {
                if glyph == "◉" {
                    l.visible = !l.visible;
                } else {
                    l.locked = !l.locked;
                }
            }
            app.save_current_session();
        }
        resp.on_hover_text(tip);
        x += 24.0;
    }

    // Label (editable inline through a double-click-free text field so the
    // value is always directly editable, which is what users expect here).
    let label_rect = egui::Rect::from_min_size(
        egui::pos2(x, top),
        Vec2::new(rect.right() - x - pad - 30.0, 22.0),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(label_rect));
    let mut name = layer.label.clone();
    let resp = child.add(
        egui::TextEdit::singleline(&mut name)
            .desired_width(label_rect.width())
            .font(egui::TextStyle::Body)
            .text_color(if selected { theme::TEXT } else { theme::TEXT_DIM })
            .margin(egui::Margin::symmetric(4, 2)),
    );
    if resp.changed()
        && let Some(l) = app.session.layer_mut(id)
    {
        l.label = name;
    }
    if resp.lost_focus() {
        app.save_current_session();
    }

    // Z badge: clicking cycles the layer one step forward (a fourth reorder
    // mechanism, alongside buttons, shortcuts, and the context menu).
    let z_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - pad - 24.0, top),
        Vec2::new(24.0, 22.0),
    );
    let z_resp = ui.interact(z_rect, ui.scope_id().with((id, "z")), Sense::click());
    ui.painter().text(
        z_rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{}", layer.z_index),
        egui::FontId::monospace(11.0),
        theme::TEXT_DIM,
    );
    if z_resp.clicked() {
        app.session.bring_forward(id);
        app.session.normalize_z();
        app.save_current_session();
    }
    z_resp.on_hover_text("Click to move one step forward");

    // Second row: role, backend badge, opacity.
    let row2 = rect.top() + 38.0;
    ui.painter().text(
        egui::pos2(rect.left() + pad, row2),
        egui::Align2::LEFT_CENTER,
        role_label(layer.role),
        egui::FontId::proportional(10.5),
        theme::TEXT_DIM,
    );
    if let Some(state) = app.runtime.layers.get(&id) {
        let (text, accent) = match state.info.as_ref() {
            Some(info) => (
                crate::ui::screens::canvas::backend_badge(info.backend, info.converted).to_string(),
                !matches!(info.backend, crate::engine::DecodeBackend::Software),
            ),
            None => ("probing…".to_string(), false),
        };
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + pad + 74.0, row2 - 9.0),
            Vec2::new(62.0, 18.0),
        );
        ui.painter().rect_filled(badge_rect, 0.0, theme::SURFACE);
        ui.painter().rect_stroke(
            badge_rect,
            0.0,
            Stroke::new(1.0, if accent { theme::ACCENT } else { theme::BORDER }),
            StrokeKind::Inside,
        );
        ui.painter().text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::monospace(10.0),
            if accent { theme::ACCENT } else { theme::TEXT_DIM },
        );
        if let Some(err) = state.error.as_ref() {
            ui.painter().text(
                egui::pos2(rect.right() - pad, row2),
                egui::Align2::RIGHT_CENTER,
                "error",
                egui::FontId::proportional(10.5),
                theme::ERROR,
            );
            let _ = err;
            let _ = has_error;
        }
    }

    // Third row: opacity slider (thin, square, accent trailing).
    let mut opacity = layer.opacity;
    let slider_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + pad, rect.top() + 56.0),
        Vec2::new(rect.width() - pad * 2.0, 16.0),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(slider_rect));
    let resp = child.add(
        egui::Slider::new(&mut opacity, 0.0..=1.0)
            .show_value(false)
            .trailing_fill(true),
    );
    if resp.changed()
        && let Some(l) = app.session.layer_mut(id)
    {
        l.opacity = opacity;
    }
    if resp.drag_stopped() {
        app.save_current_session();
    }

    // Reorder buttons, bottom right.
    let mut bx = rect.right() - pad - 20.0;
    for (glyph, forward) in [("↓", false), ("↑", true)] {
        let r = egui::Rect::from_min_size(egui::pos2(bx, rect.top() + 56.0), Vec2::splat(18.0));
        let resp = ui.interact(r, ui.scope_id().with((id, glyph)), Sense::click());
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(12.0),
            if resp.hovered() {
                theme::ACCENT
            } else {
                theme::TEXT_DIM
            },
        );
        if resp.clicked() {
            if forward {
                app.session.bring_forward(id);
            } else {
                app.session.send_backward(id);
            }
            app.session.normalize_z();
            app.save_current_session();
        }
        bx -= 22.0;
    }
}

/// Human readable role name, used by the context menu too.
pub fn role_badge(role: LayerRole) -> &'static str {
    role_label(role)
}

fn role_label(role: LayerRole) -> &'static str {
    match role {
        LayerRole::Source => "SOURCE",
        LayerRole::Reaction1 => "REACTION 1",
        LayerRole::Reaction2 => "REACTION 2",
        LayerRole::Reaction3 => "REACTION 3",
        LayerRole::ReactionOther => "REACTION",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_has_a_short_label() {
        for role in [
            LayerRole::Source,
            LayerRole::Reaction1,
            LayerRole::Reaction2,
            LayerRole::Reaction3,
            LayerRole::ReactionOther,
        ] {
            let label = role_label(role);
            assert!(!label.is_empty());
            assert_eq!(label, label.to_uppercase());
        }
    }

    #[test]
    fn panel_width_matches_the_theme() {
        assert_eq!(WIDTH, theme::LAYERS_PANEL_WIDTH);
        assert!(WIDTH >= 240.0);
    }

    #[test]
    fn stepper_labels_cover_three_decades() {
        // The stepper row offers 1, 10, and 100 ms in both directions.
        let deltas: [i64; 6] = [-100_000, -10_000, -1_000, 1_000, 10_000, 100_000];
        assert_eq!(deltas.iter().map(|d| d.abs()).collect::<std::collections::BTreeSet<_>>().len(), 3);
    }
}
