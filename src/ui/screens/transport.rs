//! The transport bar: union-timeline scrubber with per-layer spans, play/pause,
//! link, master volume, offset steppers (showing milliseconds), the hardware
//! badge, and the theater / fullscreen / layers toggles.

use eframe::egui::{self, Align, Layout, Pos2, Rect, Sense, Vec2};

use crate::app::{App, OffsetTarget};
use crate::ui::screens::canvas::format_duration;
use crate::ui::theme;
use crate::ui::widgets;

/// Height of the bar.
pub const HEIGHT: f32 = theme::TRANSPORT_HEIGHT;

/// Draw the transport bar at the bottom.
pub fn show(app: &mut App, ui: &mut Ui) {
    egui::Panel::bottom("transport")
        .exact_size(HEIGHT)
        .frame(
            egui::Frame::NONE
                .fill(theme::BG)
                .inner_margin(egui::Margin::symmetric(14, 8)),
        )
        .show(ui, |ui| {
            ui.set_height(HEIGHT - 16.0);
            scrubber(app, ui);
            ui.add_space(6.0);
            controls(app, ui);
        });
}

fn scrubber(app: &mut App, ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click_and_drag());
    let union = app.union;
    let start = union.start_us;
    let span = union.duration_us().max(1) as f32;

    // Track background.
    let track_rect = Rect::from_min_size(
        Pos2::new(rect.left(), rect.top() + 20.0),
        Vec2::new(rect.width(), 4.0),
    );
    ui.painter().rect_filled(track_rect, 0.0, theme::SURFACE);

    // Per-layer spans, stacked as thin bands. Offsets make them differ, which
    // is precisely the information the scrubber needs to show.
    let layers = app.session.layers.clone();
    for (i, layer) in layers.iter().enumerate() {
        let Some(state) = app.runtime.layers.get(&layer.id) else {
            continue;
        };
        let duration = state.duration_us();
        if duration <= 0 {
            continue;
        }
        let span_start = -layer.offset_us;
        let span_end = duration - layer.offset_us;
        let x0 = rect.left() + ((span_start - start) as f32 / span) * rect.width();
        let x1 = rect.left() + ((span_end - start) as f32 / span) * rect.width();
        let band = Rect::from_min_size(
            Pos2::new(x0.max(rect.left()), rect.top() + 3.0 + (i as f32 % 3.0) * 5.0),
            Vec2::new((x1 - x0).max(1.0).min(rect.right() - x0.max(rect.left())), 3.0),
        );
        let color = if app.selected == Some(layer.id) {
            theme::ACCENT
        } else {
            theme::TEXT_DIM.gamma_multiply(0.45)
        };
        ui.painter().rect_filled(band, 0.0, color);
    }

    // Playhead.
    let t = app.runtime.clock.position_us();
    let progress = ((t - start) as f32 / span).clamp(0.0, 1.0);
    let px = rect.left() + progress * rect.width();
    ui.painter().rect_filled(
        Rect::from_min_size(
            Pos2::new(px - 1.0, rect.top() + 1.0),
            Vec2::new(2.0, 24.0),
        ),
        0.0,
        theme::ACCENT,
    );

    // Scrub.
    if response.clicked() || response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let frac = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let target = start + (frac * span) as i64;
            app.runtime.seek_timeline(target);
        }
    }

    // Time readouts.
    ui.painter().text(
        Pos2::new(rect.left(), rect.top() + 12.0),
        egui::Align2::LEFT_CENTER,
        format_duration(t - start),
        egui::FontId::monospace(11.0),
        theme::TEXT,
    );
    ui.painter().text(
        Pos2::new(rect.right(), rect.top() + 12.0),
        egui::Align2::RIGHT_CENTER,
        format_duration(span as i64),
        egui::FontId::monospace(11.0),
        theme::TEXT_DIM,
    );
}

fn controls(app: &mut App, ui: &mut Ui) {
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        // Play / pause: a triangle or two bars, drawn by hand so it stays
        // exactly on the theme's grid.
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
        ui.painter().rect_filled(rect, 0.0, theme::ACCENT);
        let c = rect.center();
        if app.runtime.is_playing() {
            for dx in [-4.0f32, 2.0] {
                ui.painter().rect_filled(
                    Rect::from_min_size(Pos2::new(c.x + dx, c.y - 6.0), Vec2::new(3.0, 12.0)),
                    0.0,
                    theme::BG,
                );
            }
        } else {
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(c.x - 4.0, c.y - 7.0),
                    Pos2::new(c.x + 7.0, c.y),
                    Pos2::new(c.x - 4.0, c.y + 7.0),
                ],
                theme::BG,
                egui::PathStroke::NONE,
            ));
        }
        if resp.clicked() {
            app.apply_action(crate::ui::shortcuts::Action::TogglePlay);
        }
        resp.on_hover_text("Play / pause (Space)");

        ui.add_space(6.0);
        for (glyph, forward) in [("⏴", false), ("⏵", true)] {
            if widgets::button(ui, glyph).clicked() {
                let step = app.persisted.preferences.seek_step_us();
                app.runtime.step_timeline(if forward { step } else { -step });
            }
        }

        ui.add_space(10.0);
        let link_label = if app.session.linked { "LINK" } else { "UNLINKED" };
        if widgets::chip(ui, link_label, app.session.linked) {
            app.apply_action(crate::ui::shortcuts::Action::ToggleLink);
        }

        ui.add_space(12.0);

        // Master volume.
        widgets::dim(ui, "VOL");
        let mut vol = app.runtime.master_volume;
        let slider = egui::Slider::new(&mut vol, 0.0..=1.0)
            .show_value(false)
            .trailing_fill(true);
        let resp = ui.add_sized(Vec2::new(96.0, 18.0), slider);
        if resp.changed() {
            app.runtime.master_volume = vol;
            app.runtime.audio.set_master_volume(vol);
        }
        widgets::value_slot(ui, &format!("{}%", (vol * 100.0).round()), 34.0);

        ui.add_space(8.0);
        let mute = if app.runtime.master_muted { "MUTED" } else { "MUTE" };
        if widgets::chip(ui, mute, app.runtime.master_muted) {
            app.apply_action(crate::ui::shortcuts::Action::ToggleMasterMute);
        }

        ui.add_space(14.0);
        offset_target(app, ui);

        // Right side.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::icon_toggle(ui, "⛶", false, "Fullscreen (F)") {
                app.apply_action(crate::ui::shortcuts::Action::ToggleFullscreen);
            }
            if widgets::icon_toggle(ui, "▤", app.show_layers, "Layers panel") {
                app.show_layers = !app.show_layers;
            }
            if widgets::icon_toggle(ui, "▣", app.theater, "Theater mode (T)") {
                app.theater = !app.theater;
            }
            hardware_badge(app, ui);
            widgets::dim(ui, &format!("{} layers", app.session.layers.len()));
        });
    });
}

fn offset_target(app: &mut App, ui: &mut Ui) {
    let current = match app.offset_target {
        OffsetTarget::Layer(id) => app.session.layer(id).map(|l| l.offset_us).unwrap_or(0),
        OffsetTarget::All => app.session.layers.first().map(|l| l.offset_us).unwrap_or(0),
    };
    let scope = match app.offset_target {
        OffsetTarget::Layer(id) => app
            .session
            .layer(id)
            .map(|l| l.label.clone())
            .unwrap_or_else(|| "layer".into()),
        OffsetTarget::All => "all".to_string(),
    };
    widgets::dim(ui, &format!("OFFSET ({scope})"));
    if widgets::mini_button(ui, "−") {
        app.apply_action(crate::ui::shortcuts::Action::NudgeOffset(-1_000));
    }
    widgets::value_slot(ui, &format!("{:+} ms", current / 1_000), 74.0);
    if widgets::mini_button(ui, "+") {
        app.apply_action(crate::ui::shortcuts::Action::NudgeOffset(1_000));
    }
}

fn hardware_badge(app: &mut App, ui: &mut Ui) {
    // The badge reports the best backend in use across layers, plus how many
    // layers were converted, so one glance answers "is hardware decode on?".
    let mut best = crate::engine::DecodeBackend::Software;
    let mut converted = 0usize;
    let mut unknown = 0usize;
    for id in app.session.layers.iter().map(|l| l.id) {
        match app.runtime.layers.get(&id).and_then(|s| s.info.as_ref()) {
            Some(info) => {
                if info.backend.rank() > best.rank() {
                    best = info.backend;
                }
                if info.converted {
                    converted += 1;
                }
            }
            None => unknown += 1,
        }
    }
    let text = if unknown == app.session.layers.len() {
        "HW: probing".to_string()
    } else if converted > 0 {
        format!("{} · {converted} converted", best.badge())
    } else {
        best.badge().to_string()
    };
    widgets::badge(ui, text, !matches!(best, crate::engine::DecodeBackend::Software));
}
