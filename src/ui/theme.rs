//! The single source of truth for the visual identity of the application.
//!
//! Everything is pure black, white/zinc text and one yellow accent. There are
//! no shadows, no blur, no gradients and no rounded corners anywhere: all
//! widgets are styled through [`egui::Visuals`] and [`egui::Style`] overrides
//! in [`apply`].

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Theme, Vec2, Visuals,
};

/// Pure black background.
pub const BG: Color32 = Color32::BLACK;
/// Primary text.
pub const TEXT: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// Secondary text.
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xA1, 0xA1, 0xAA);
/// The single accent color. Everything interactive highlights in this color.
pub const ACCENT: Color32 = Color32::from_rgb(0xFA, 0xCC, 0x15);
/// 1px border color (slightly lighter than the background so it reads on LCDs).
pub const BORDER: Color32 = Color32::from_rgb(0x2A, 0x2A, 0x2E);
/// Slightly lifted surface (used only for hover/pressed states, never a gradient).
pub const SURFACE: Color32 = Color32::from_rgb(0x12, 0x12, 0x14);
/// Hovered control surface: one step lighter than [`SURFACE`], still flat.
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x1C, 0x1C, 0x20);
/// Pressed control surface.
pub const SURFACE_RAISED: Color32 = Color32::from_rgb(0x26, 0x26, 0x2B);
/// Lighter tint of the accent, used for the accent button's hover state.
pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(0xFD, 0xE0, 0x56);
/// Darker tint of the accent, used for the accent button's pressed state.
pub const ACCENT_DARK: Color32 = Color32::from_rgb(0xD4, 0xAA, 0x0B);
/// Dim error surface behind destructive hover states.
pub const ERROR_DIM: Color32 = Color32::from_rgb(0x2A, 0x14, 0x14);
/// Error text color (kept dim so the accent stays the only highlight).
pub const ERROR: Color32 = Color32::from_rgb(0xF8, 0x71, 0x71);

/// Height of the top bar in points.
pub const TOP_BAR_HEIGHT: f32 = 44.0;
/// Height of the transport bar in points.
pub const TRANSPORT_HEIGHT: f32 = 96.0;
/// Width of the layers panel in points.
pub const LAYERS_PANEL_WIDTH: f32 = 292.0;

/// A 1px solid border stroke.
pub fn border_stroke() -> Stroke {
    Stroke::new(1.0, BORDER)
}

/// A 1px accent stroke.
pub fn accent_stroke() -> Stroke {
    Stroke::new(1.0, ACCENT)
}

/// Add a custom heading text style used for the large typographic actions on
/// the home screen.
pub const DISPLAY_STYLE: &str = "display";

/// Build the flat, dark visuals used by the whole application.
pub fn visuals() -> Visuals {
    let mut v = Visuals::dark();

    v.dark_mode = true;
    v.override_text_color = Some(TEXT);
    v.weak_text_color = Some(TEXT_DIM);
    v.weak_text_alpha = 1.0;
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = ACCENT;
    v.error_fg_color = ERROR;

    // Panels and windows: black, flat, 1px border.
    v.panel_fill = BG;
    v.window_fill = BG;
    v.window_stroke = border_stroke();
    v.window_corner_radius = CornerRadius::ZERO;
    v.window_shadow = Default::default();
    v.popup_shadow = Default::default();
    v.menu_corner_radius = CornerRadius::ZERO;
    v.faint_bg_color = SURFACE;
    v.extreme_bg_color = BG;
    v.text_edit_bg_color = Some(BG);
    v.code_bg_color = SURFACE;
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(0xFA, 0xCC, 0x15, 0x40);
    v.selection.stroke = accent_stroke();
    v.text_cursor.stroke = accent_stroke();

    // Widgets: square, black, hairline borders. Accent for hover/open states.
    let base = egui::style::WidgetVisuals {
        bg_fill: BG,
        weak_bg_fill: BG,
        bg_stroke: border_stroke(),
        corner_radius: CornerRadius::ZERO,
        fg_stroke: Stroke::new(1.0, TEXT),
        expansion: 0.0,
    };
    v.widgets.noninteractive = egui::style::WidgetVisuals {
        fg_stroke: Stroke::new(1.0, TEXT_DIM),
        ..base
    };
    v.widgets.inactive = base;
    v.widgets.hovered = egui::style::WidgetVisuals {
        bg_fill: SURFACE,
        weak_bg_fill: SURFACE,
        bg_stroke: accent_stroke(),
        fg_stroke: Stroke::new(1.0, ACCENT),
        ..base
    };
    v.widgets.active = egui::style::WidgetVisuals {
        bg_fill: Color32::from_rgb(0x1A, 0x1A, 0x1D),
        weak_bg_fill: Color32::from_rgb(0x1A, 0x1A, 0x1D),
        bg_stroke: accent_stroke(),
        fg_stroke: Stroke::new(1.0, ACCENT),
        ..base
    };
    v.widgets.open = egui::style::WidgetVisuals {
        bg_fill: SURFACE,
        weak_bg_fill: SURFACE,
        bg_stroke: accent_stroke(),
        fg_stroke: Stroke::new(1.0, ACCENT),
        ..base
    };

    v
}

/// Apply the global style. Called once at startup and again whenever the
/// accent/monospace metrics need to be refreshed.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|style| {
        style.visuals = visuals();
        style.animation_time = 0.0;
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.spacing.interact_size = Vec2::new(32.0, 26.0);
        style.spacing.slider_width = 140.0;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.window_margin = egui::Margin::same(0);

        style.text_styles.insert(
            TextStyle::Body,
            FontId::new(14.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(13.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(20.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        );
        style.text_styles.insert(
            TextStyle::Name(DISPLAY_STYLE.into()),
            FontId::new(44.0, FontFamily::Proportional),
        );
    });
    // Both themes are identical: the app does not follow the system theme.
    ctx.set_visuals_of(Theme::Light, visuals());
}

/// A small helper for accent-colored text.
pub fn accent_text(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).color(ACCENT)
}

/// A small helper for dimmed text.
pub fn dim_text(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).color(TEXT_DIM)
}
