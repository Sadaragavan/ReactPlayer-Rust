//! Small, reusable widgets used by every screen.
//!
//! Every widget in this module is deliberately flat: 1 px borders, no corner
//! radius, no shadows, one accent colour. Anything that needs a look other
//! than the theme's gets it here and nowhere else.

use eframe::egui::{
    self, Align, CornerRadius, Layout, Response, RichText, Sense, Stroke, StrokeKind, Ui, Vec2,
};

use crate::ui::theme;

/// The theme's corner radius: always zero (square corners everywhere).
pub const RADIUS: CornerRadius = CornerRadius::ZERO;

/// Fill for an ordinary button.
pub fn button(u: &mut Ui, text: impl Into<String>) -> Response {
    let text: String = text.into();
    let galley = u.painter().layout_no_wrap(
        text.clone(),
        egui::FontId::proportional(13.0),
        theme::TEXT,
    );
    let size = Vec2::new(galley.size().x + 24.0, 30.0);
    let (rect, response) = u.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let pressed = response.is_pointer_button_down_on();
    let bg = if pressed {
        theme::SURFACE_RAISED
    } else if hovered {
        theme::SURFACE_HOVER
    } else {
        theme::SURFACE
    };
    u.painter().rect_filled(rect, RADIUS, bg);
    u.painter().rect_stroke(
        rect,
        RADIUS,
        Stroke::new(1.0, if hovered { theme::TEXT_DIM } else { theme::BORDER }),
        StrokeKind::Inside,
    );
    u.painter()
        .galley(rect.center() - galley.size() * 0.5, galley);
    response
}

/// The accent (primary action) button: filled `#FACC15`, black label.
pub fn accent_button(u: &mut Ui, text: impl Into<String>) -> Response {
    let text: String = text.into();
    let galley = u.painter().layout_no_wrap(
        text.clone(),
        egui::FontId::proportional(13.0),
        theme::TEXT,
    );
    let size = Vec2::new(galley.size().x + 28.0, 30.0);
    let (rect, response) = u.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let pressed = response.is_pointer_button_down_on();
    let fill = if pressed {
        theme::ACCENT_DARK
    } else if hovered {
        theme::ACCENT_LIGHT
    } else {
        theme::ACCENT
    };
    u.painter().rect_filled(rect, RADIUS, fill);
    u.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(13.0),
        theme::BG,
    );
    response
}

/// A destructive button: red outline, red label.
pub fn danger_button(u: &mut Ui, text: impl Into<String>) -> Response {
    let text: String = text.into();
    let galley = u.painter().layout_no_wrap(
        text.clone(),
        egui::FontId::proportional(13.0),
        theme::TEXT,
    );
    let size = Vec2::new(galley.size().x + 24.0, 30.0);
    let (rect, response) = u.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    u.painter().rect_filled(
        rect,
        RADIUS,
        if hovered {
            theme::ERROR_DIM
        } else {
            theme::SURFACE
        },
    );
    u.painter().rect_stroke(
        rect,
        RADIUS,
        Stroke::new(1.0, theme::ERROR),
        StrokeKind::Inside,
    );
    u.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(13.0),
        theme::ERROR,
    );
    response
}

/// A small chip / badge with an optional accent outline.
pub fn badge(u: &mut Ui, text: impl Into<String>, accent: bool) -> Response {
    let text: String = text.into();
    let galley = u.painter().layout_no_wrap(
        text.clone(),
        egui::FontId::monospace(11.0),
        theme::TEXT,
    );
    let size = Vec2::new(galley.size().x + 14.0, 20.0);
    let (rect, response) = u.allocate_exact_size(size, Sense::hover());
    u.painter().rect_filled(rect, RADIUS, theme::SURFACE);
    u.painter().rect_stroke(
        rect,
        RADIUS,
        Stroke::new(
            1.0,
            if accent { theme::ACCENT } else { theme::BORDER },
        ),
        StrokeKind::Inside,
    );
    u.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::monospace(11.0),
        if accent { theme::ACCENT } else { theme::TEXT_DIM },
    );
    response
}

/// A section heading: uppercase, dim, letter-spaced by hand.
pub fn section_label(u: &mut Ui, text: &str) {
    u.add_space(2.0);
    u.label(
        RichText::new(text.to_uppercase())
            .size(10.0)
            .color(theme::TEXT_DIM)
            .strong(),
    );
    u.add_space(2.0);
}

/// A 1 px horizontal rule.
pub fn rule(u: &mut Ui) {
    let (rect, _) = u.allocate_exact_size(Vec2::new(u.available_width(), 1.0), Sense::hover());
    u.painter()
        .rect_filled(rect, RADIUS, theme::BORDER);
}

/// A toggle rendered as a small square with a filled inner square when on.
pub fn checkbox(u: &mut Ui, on: &mut bool, label: &str) -> Response {
    let size = Vec2::new(u.available_width(), 22.0);
    let (rect, response) = u.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
    }
    let box_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.center().y - 7.0),
        Vec2::splat(14.0),
    );
    u.painter().rect_filled(box_rect, RADIUS, theme::SURFACE);
    u.painter().rect_stroke(
        box_rect,
        RADIUS,
        Stroke::new(1.0, if *on { theme::ACCENT } else { theme::BORDER }),
        StrokeKind::Inside,
    );
    if *on {
        u.painter().rect_filled(
            box_rect.shrink(4.0),
            RADIUS,
            theme::ACCENT,
        );
    }
    u.painter().text(
        egui::pos2(box_rect.right() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        if response.hovered() {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        },
    );
    response
}

/// A one-line text field with a dim label above it.
pub fn labelled_text(
    u: &mut Ui,
    id: egui::Id,
    label: &str,
    value: &mut String,
    hint: &str,
) -> Response {
    u.label(
        RichText::new(label)
            .size(11.0)
            .color(theme::TEXT_DIM),
    );
    let response = u.add(
        egui::TextEdit::singleline(value)
            .id(id)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Body),
    );
    response
}

/// A dim secondary line of text.
pub fn dim(u: &mut Ui, text: impl Into<String>) {
    u.label(
        RichText::new(text.into())
            .size(11.0)
            .color(theme::TEXT_DIM),
    );
}

/// A monospaced value shown right-aligned in a fixed-width slot.
pub fn value_slot(u: &mut Ui, text: &str, width: f32) {
    let (rect, _) = u.allocate_exact_size(Vec2::new(width, 18.0), Sense::hover());
    u.painter().text(
        rect.right_center(),
        egui::Align2::RIGHT_CENTER,
        text,
        egui::FontId::monospace(12.0),
        theme::TEXT,
    );
}

/// A horizontal row of equal-width items.
pub fn row<R>(u: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    u.with_layout(Layout::left_to_right(Align::Center), add)
}

/// A spinner-free "working" indicator: three dim dots.
pub fn working(u: &mut Ui, what: &str) {
    u.horizontal(|ui| {
        let color = theme::ACCENT;
        for i in 0..3 {
            let t = u.input(|i| i.time) as f32;
            let phase = ((t * 2.0 + i as f32 * 0.3).sin() * 0.5 + 0.5) * 0.8 + 0.2;
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(4.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, RADIUS, color.gamma_multiply(phase));
        }
        ui.label(RichText::new(what).size(11.0).color(theme::TEXT_DIM));
    });
}

/// A square icon-only toggle button.
pub fn icon_toggle(u: &mut Ui, glyph: &str, active: bool, tooltip: &str) -> bool {
    let (rect, response) = u.allocate_exact_size(Vec2::splat(26.0), Sense::click());
    let hovered = response.hovered();
    let fg = if active {
        theme::ACCENT
    } else if hovered {
        theme::TEXT
    } else {
        theme::TEXT_DIM
    };
    if active {
        u.painter().rect_filled(rect, RADIUS, theme::SURFACE);
    }
    if hovered {
        u.painter().rect_stroke(
            rect,
            RADIUS,
            Stroke::new(1.0, theme::BORDER),
            StrokeKind::Inside,
        );
    }
    u.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(14.0),
        fg,
    );
    if !tooltip.is_empty() {
        response.clone().on_hover_text(tooltip);
    }
    response.clicked()
}

/// A compact numeric stepper: "−  value  +".
///
/// Returns `Some(-1)` for the minus button, `Some(1)` for plus, else `None`.
pub fn stepper(u: &mut Ui, value: &str, width: f32) -> Option<i32> {
    let mut clicked = None;
    u.horizontal(|ui| {
        if mini_button(ui, "−") {
            clicked = Some(-1);
        }
        value_slot(ui, value, width);
        if mini_button(ui, "+") {
            clicked = Some(1);
        }
    });
    clicked
}

/// A 20x20 bare glyph button.
pub fn mini_button(u: &mut Ui, glyph: &str) -> bool {
    let (rect, response) = u.allocate_exact_size(Vec2::splat(20.0), Sense::click());
    let color = if response.hovered() {
        theme::ACCENT
    } else {
        theme::TEXT_DIM
    };
    u.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::monospace(13.0),
        color,
    );
    response.clicked()
}

/// A clickable dim text label used as a lightweight button.
pub fn mini_label(u: &mut Ui, text: &str) -> bool {
    let response = u.add(
        egui::Label::new(
            RichText::new(text)
                .size(11.5)
                .color(theme::TEXT_DIM),
        )
        .sense(Sense::click()),
    );
    if response.hovered() {
        u.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

/// A small pill used for picking one of a few options; accent when selected.
pub fn chip(u: &mut Ui, text: &str, selected: bool) -> bool {
    let galley = u.painter().layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(11.5),
        theme::TEXT,
    );
    let size = Vec2::new(galley.size().x + 16.0, 22.0);
    let (rect, response) = u.allocate_exact_size(size, Sense::click());
    let (bg, border, fg) = if selected {
        (theme::ACCENT, theme::ACCENT, theme::BG)
    } else if response.hovered() {
        (theme::SURFACE_HOVER, theme::TEXT_DIM, theme::TEXT)
    } else {
        (theme::SURFACE, theme::BORDER, theme::TEXT_DIM)
    };
    u.painter().rect_filled(rect, RADIUS, bg);
    u.painter()
        .rect_stroke(rect, RADIUS, Stroke::new(1.0, border), StrokeKind::Inside);
    u.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(11.5),
        fg,
    );
    response.clicked()
}

/// A selectable list row: accent left bar when selected.
pub fn selectable_row(u: &mut Ui, selected: bool, height: f32, add: impl FnOnce(&mut Ui)) -> egui::Response {
    let (rect, response) = u.allocate_exact_size(
        Vec2::new(u.available_width(), height),
        Sense::click(),
    );
    if selected || response.hovered() {
        u.painter().rect_filled(
            rect,
            RADIUS,
            if selected {
                theme::SURFACE_HOVER
            } else {
                theme::SURFACE
            },
        );
    }
    if selected {
        u.painter().vline(
            rect.left() + 1.5,
            rect.y_range(),
            Stroke::new(3.0, theme::ACCENT),
        );
    }
    let mut content = u.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(Vec2::new(10.0, 4.0))));
    add(&mut content);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_is_zero_everywhere() {
        // The aesthetic calls for hard edges; a non-zero radius here would
        // leak into every widget.
        assert_eq!(RADIUS, CornerRadius::ZERO);
        assert_eq!(RADIUS.nw, 0);
        assert_eq!(RADIUS.se, 0);
    }

    #[test]
    fn theme_has_exactly_one_accent() {
        // Guards against a second saturated colour creeping into the theme.
        assert_eq!(theme::ACCENT, Color32::from_rgb(0xFA, 0xCC, 0x15));
        assert_eq!(theme::ACCENT_LIGHT.r, theme::ACCENT.r);
        assert_eq!(theme::ACCENT_DARK.r, theme::ACCENT.r);
        assert!(theme::ACCENT_LIGHT.g >= theme::ACCENT.g);
        assert!(theme::ACCENT_DARK.g <= theme::ACCENT.g);
    }

    #[test]
    fn text_colours_match_the_specification() {
        assert_eq!(theme::BG, Color32::BLACK);
        assert_eq!(theme::TEXT, Color32::WHITE);
        assert_eq!(theme::TEXT_DIM, Color32::from_rgb(0xA1, 0xA1, 0xAA));
    }

    #[test]
    fn destructive_colour_is_reserved_for_errors() {
        assert_ne!(theme::ERROR, theme::ACCENT);
        assert_eq!(theme::ERROR, Color32::from_rgb(0xF8, 0x71, 0x71));
    }
}
