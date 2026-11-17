//! The small vocabulary the screens are written in: flat square buttons, one
//! primary action per view, chips for choices, hairlines instead of boxes.
//!
//! Every widget here takes the theme's colours from [`theme`], so a screen never
//! names a colour. Nothing has a shadow, a radius or a gradient.

use eframe::egui::{
    self, Align, Button, Color32, Layout, Response, RichText, Sense, Stroke, Ui, Vec2,
};

use super::theme::{self, ACCENT, BG, DANGER, FILL_ACTIVE, FILL_HOVER, LINE, LINE_STRONG, MUTED, TEXT};

/// The fill of a primary button: the accent, with black text on it.
fn primary_button(text: &str) -> Button<'_> {
    Button::new(RichText::new(text).color(BG).size(13.0))
        .fill(ACCENT)
        .stroke(Stroke::new(1.0, ACCENT))
        .corner_radius(theme::RADIUS)
        .min_size(Vec2::new(0.0, 30.0))
}

fn secondary_button(text: &str) -> Button<'_> {
    Button::new(RichText::new(text).color(TEXT).size(13.0))
        .fill(BG)
        .stroke(theme::hairline(LINE))
        .corner_radius(theme::RADIUS)
        .min_size(Vec2::new(0.0, 30.0))
}

fn ghost_button(text: &str) -> Button<'_> {
    Button::new(RichText::new(text).color(MUTED).size(12.5))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .corner_radius(theme::RADIUS)
        .min_size(Vec2::new(0.0, 26.0))
}

fn danger_button(text: &str) -> Button<'_> {
    Button::new(RichText::new(text).color(DANGER).size(13.0))
        .fill(BG)
        .stroke(theme::hairline(DANGER))
        .corner_radius(theme::RADIUS)
        .min_size(Vec2::new(0.0, 30.0))
}

/// The one action a view exists for.
pub fn primary(ui: &mut Ui, text: &str) -> Response {
    ui.add(primary_button(text))
}

/// Everything next to the primary action.
pub fn secondary(ui: &mut Ui, text: &str) -> Response {
    ui.add(secondary_button(text))
}

/// Quiet, inline actions: "Reset", "Change".
pub fn ghost(ui: &mut Ui, text: &str) -> Response {
    ui.add(ghost_button(text))
}

/// Destructive actions, always after a confirmation.
pub fn danger(ui: &mut Ui, text: &str) -> Response {
    ui.add(danger_button(text))
}

/// A full-width row that behaves like a button, for lists and cards.
pub fn row(ui: &mut Ui, selected: bool, add: impl FnOnce(&mut Ui)) -> Response {
    let fill = if selected { FILL_ACTIVE } else { BG };
    egui::Frame::new()
        .fill(fill)
        .stroke(theme::hairline(if selected { ACCENT } else { LINE }))
        .corner_radius(theme::RADIUS)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        })
        .response
}

/// A toggle: 1px border, accent when on. Used for layout choices, fit modes and
/// every other either-or.
pub fn chip(ui: &mut Ui, on: bool, text: &str) -> Response {
    let (fill, stroke, colour) = if on {
        (FILL_ACTIVE, Stroke::new(1.0, ACCENT), TEXT)
    } else {
        (BG, theme::hairline(LINE), MUTED)
    };
    ui.add(
        Button::new(RichText::new(text).color(colour).size(12.0))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(theme::RADIUS)
            .min_size(Vec2::new(0.0, 26.0)),
    )
}

/// A small caption with a hairline border; used for hardware badges and counts.
pub fn badge(ui: &mut Ui, text: &str, accent: bool) -> Response {
    let colour = if accent { ACCENT } else { MUTED };
    let stroke = if accent {
        Stroke::new(1.0, ACCENT)
    } else {
        theme::hairline(LINE)
    };
    egui::Frame::new()
        .fill(BG)
        .stroke(stroke)
        .corner_radius(theme::RADIUS)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(colour).size(10.5));
        })
        .response
}

/// A heading with a hairline underneath, the only structural separator used.
pub fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.label(RichText::new(title).color(TEXT).size(15.0).strong());
    ui.add_space(2.0);
    hairline(ui);
    ui.add_space(8.0);
    add(ui);
    ui.add_space(10.0);
}

/// A 1px rule across the available width.
pub fn hairline(ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, theme::hairline(LINE));
}

/// A key on the left, its value on the right, both on one line.
pub fn kv(ui: &mut Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).color(MUTED).size(12.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).color(TEXT).size(12.0));
        });
    });
}

/// A label above a slider, with the value on the right.
pub fn slider(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
) -> Response {
    let mut response = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(MUTED).size(12.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let text = if suffix.is_empty() {
                format!("{value:.2}")
            } else {
                format!("{value:.2}{suffix}")
            };
            ui.label(RichText::new(text).color(TEXT).size(12.0));
            response = Some(ui.add(egui::Slider::new(value, range).show_value(false)));
        });
    });
    response.unwrap_or_else(|| ui.label(""))
}

/// A slider over 0..1, for opacity, wipe position and crop edges.
pub fn percent(ui: &mut Ui, label: &str, value: &mut f32) -> Response {
    slider(ui, label, value, 0.0..=1.0, "")
}

/// A single-line text field that fills the width.
pub fn text_edit(ui: &mut Ui, value: &mut String, hint: &str) -> Response {
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .margin(Vec2::new(8.0, 6.0)),
    )
}

/// The display name of a media file, for lists.
pub fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Pad a hover area so a click target is comfortable without changing layout.
pub fn clickable(ui: &mut Ui, size: Vec2) -> Response {
    ui.allocate_response(size, Sense::click())
}

/// Format a duration as `m:ss.mmm` for transport readouts.
pub fn timecode(us: i64) -> String {
    let total_ms = (us.max(0) / 1_000) as u64;
    let minutes = total_ms / 60_000;
    let seconds = (total_ms % 60_000) / 1_000;
    let millis = total_ms % 1_000;
    format!("{minutes}:{seconds:02}.{millis:03}")
}

/// Format a signed offset as `+1.500s`.
pub fn offset(us: i64) -> String {
    let sign = if us < 0 { '-' } else { '+' };
    let abs = us.abs();
    format!("{sign}{}.{:03}s", abs / 1_000_000, (abs % 1_000_000) / 1_000)
}

/// The hover fill for a list row, exposed so screens can paint their own rows.
pub fn hover_fill(hovered: bool, selected: bool) -> Color32 {
    if selected {
        FILL_ACTIVE
    } else if hovered {
        FILL_HOVER
    } else {
        BG
    }
}

/// The border colour a row should use.
pub fn row_border(selected: bool) -> Stroke {
    if selected {
        Stroke::new(1.0, ACCENT)
    } else {
        theme::hairline(LINE_STRONG)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecode_formats_milli_seconds() {
        assert_eq!(timecode(0), "0:00.000");
        assert_eq!(timecode(1_500_000), "0:01.500");
        assert_eq!(timecode(65_432_000), "1:05.432");
        assert_eq!(timecode(-5), "0:00.000");
    }

    #[test]
    fn offsets_are_signed() {
        assert_eq!(offset(1_500_000), "+1.500s");
        assert_eq!(offset(-250_000), "-0.250s");
        assert_eq!(offset(0), "+0.000s");
    }

    #[test]
    fn row_colours_follow_state() {
        assert_eq!(hover_fill(false, false), BG);
        assert_eq!(hover_fill(true, false), FILL_HOVER);
        assert_eq!(hover_fill(true, true), FILL_ACTIVE);
        assert_eq!(row_border(true).color, ACCENT);
        assert_eq!(row_border(false).color, LINE_STRONG);
    }

    #[test]
    fn file_names_fall_back_to_the_whole_path() {
        assert_eq!(
            file_name(std::path::Path::new("/videos/take 2.mp4")),
            "take 2.mp4"
        );
        assert_eq!(file_name(std::path::Path::new("/")), "/");
    }
}
