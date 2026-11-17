//! The one visual language the whole app is drawn with.
//!
//! Pure `#000000` ground, one accent (`#FACC15`) that only ever marks live or
//! selected state, 1px borders, square corners, no shadows, no blur, no
//! gradients. Everything here is expressed through [`egui::Visuals`] and
//! [`egui::Style`] so third-party widgets (sliders, combos, text edits) inherit
//! it instead of needing per-widget overrides.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Vec2,
};

/// The canvas and every panel background.
pub const BG: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
/// Primary text.
pub const TEXT: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// Secondary text, disabled labels, timings.
pub const MUTED: Color32 = Color32::from_rgb(0xA1, 0xA1, 0xAA);
/// The single accent: live state, selection, playhead, snap guides.
pub const ACCENT: Color32 = Color32::from_rgb(0xFA, 0xCC, 0x15);
/// Hairline border, the normal case.
pub const LINE: Color32 = Color32::from_rgb(0x27, 0x27, 0x2A);
/// Hairline border for floating surfaces and hovered edges.
pub const LINE_STRONG: Color32 = Color32::from_rgb(0x45, 0x45, 0x4C);
/// Hover fill for flat buttons and list rows.
pub const FILL_HOVER: Color32 = Color32::from_rgb(0x17, 0x17, 0x1A);
/// Pressed / active fill.
pub const FILL_ACTIVE: Color32 = Color32::from_rgb(0x24, 0x24, 0x29);
/// Error text (a toast only; never a filled surface).
pub const DANGER: Color32 = Color32::from_rgb(0xF8, 0x71, 0x71);

/// Zero radius, everywhere.
pub const RADIUS: CornerRadius = CornerRadius::ZERO;

/// A 1px hairline.
pub fn hairline(color: Color32) -> Stroke {
    Stroke::new(1.0, color)
}

/// Body text.
pub fn text_style() -> FontId {
    FontId::new(13.5, FontFamily::Proportional)
}

/// A flat, square panel with a 1px border.
pub fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(BG)
        .stroke(hairline(LINE))
        .corner_radius(RADIUS)
        .inner_margin(Margin::same(14))
}

/// A floating surface: still flat and square, with a stronger border so it
/// separates from what is behind it without a shadow.
pub fn float_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(BG)
        .stroke(hairline(LINE_STRONG))
        .corner_radius(RADIUS)
        .inner_margin(Margin::same(16))
}

/// Install the theme on a context. Called once at startup and whenever the
/// style was changed behind our back (egui keeps a zoom factor on it).
pub fn apply(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.animation_time = 0.0;
    style.visuals = visuals();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 6.0);
    style.spacing.window_margin = Margin::same(16);
    style.spacing.menu_margin = Margin::same(4);
    style.spacing.interact_size = Vec2::new(28.0, 24.0);
    style.spacing.slider_width = 140.0;
    style.spacing.indent = 14.0;
    style.spacing.menu_width = 240.0;
    style.text_styles = text_styles();
    ctx.set_style(style);
}

/// The type scale: one family, a handful of sizes.
pub fn text_styles() -> std::collections::BTreeMap<TextStyle, FontId> {
    use TextStyle as T;
    let mut m = std::collections::BTreeMap::new();
    m.insert(T::Heading, FontId::proportional(22.0));
    m.insert(T::Body, FontId::proportional(13.5));
    m.insert(T::Monospace, FontId::monospace(12.0));
    m.insert(T::Button, FontId::proportional(13.0));
    m.insert(T::Small, FontId::proportional(11.0));
    m
}

fn widget(
    bg_fill: Color32,
    bg_stroke: Stroke,
    fg_stroke: Stroke,
) -> egui::style::WidgetVisuals {
    egui::style::WidgetVisuals {
        bg_fill,
        weak_bg_fill: bg_fill,
        bg_stroke,
        corner_radius: RADIUS,
        fg_stroke,
        expansion: 0.0,
    }
}

/// The [`egui::Visuals`] half of the theme, exposed for tests.
pub fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.dark_mode = true;
    v.override_text_color = Some(TEXT);
    v.weak_text_color = Some(MUTED);
    v.weak_text_alpha = 1.0;
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = BG;
    v.faint_bg_color = FILL_HOVER;
    v.code_bg_color = BG;
    v.text_edit_bg_color = Some(BG);
    v.window_corner_radius = RADIUS;
    v.menu_corner_radius = RADIUS;
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.window_stroke = hairline(LINE_STRONG);
    v.window_highlight_topmost = false;
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = ACCENT;
    v.error_fg_color = DANGER;
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, BG);
    v.button_frame = true;
    v.striped = false;
    v.slider_trailing_fill = false;
    v.clip_rect_margin = 0.0;
    v.image_loading_spinners = false;
    v.resize_corner_size = 12.0;

    v.widgets.noninteractive = widget(BG, hairline(LINE), Stroke::new(1.0, TEXT));
    v.widgets.inactive = widget(BG, hairline(LINE), Stroke::new(1.0, TEXT));
    v.widgets.hovered = widget(FILL_HOVER, hairline(LINE_STRONG), Stroke::new(1.0, TEXT));
    v.widgets.active = widget(FILL_ACTIVE, hairline(ACCENT), Stroke::new(1.0, TEXT));
    v.widgets.open = widget(FILL_ACTIVE, hairline(LINE_STRONG), Stroke::new(1.0, TEXT));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_is_black_and_square() {
        let v = visuals();
        assert_eq!(v.panel_fill, Color32::BLACK);
        assert_eq!(v.window_fill, Color32::BLACK);
        assert_eq!(v.extreme_bg_color, Color32::BLACK);
        assert_eq!(v.window_corner_radius, CornerRadius::ZERO);
        assert_eq!(v.widgets.inactive.corner_radius, CornerRadius::ZERO);
        assert_eq!(v.widgets.active.bg_stroke.color, ACCENT);
        assert_eq!(v.override_text_color, Some(TEXT));
        assert_eq!(v.window_shadow, Shadow::NONE);
    }

    #[test]
    fn accent_is_the_only_accent() {
        assert_eq!(ACCENT, Color32::from_rgb(0xFA, 0xCC, 0x15));
        assert_eq!(MUTED, Color32::from_rgb(0xA1, 0xA1, 0xAA));
        assert_eq!(TEXT, Color32::WHITE);
    }
}
