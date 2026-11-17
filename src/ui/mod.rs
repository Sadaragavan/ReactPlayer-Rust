//! Everything the user sees: the theme, shared widgets, screens, and the
//! keyboard dispatcher.

pub mod screens;
pub mod shortcuts;
pub mod theme;
pub mod toast;
pub mod widgets;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_constants_are_the_documented_values() {
        assert_eq!(theme::BG.to_array(), [0x00, 0x00, 0x00, 0xFF]);
        assert_eq!(theme::TEXT.to_array(), [0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(theme::TEXT_DIM.to_array(), [0xA1, 0xA1, 0xAA, 0xFF]);
        assert_eq!(theme::ACCENT.to_array(), [0xFA, 0xCC, 0x15, 0xFF]);
    }

    #[test]
    fn panel_metrics_are_sane() {
        assert!(theme::TOP_BAR_HEIGHT >= 40.0);
        assert!(theme::TRANSPORT_HEIGHT >= 80.0);
        assert!(theme::LAYERS_PANEL_WIDTH >= 240.0);
    }

    #[test]
    fn widgets_are_square() {
        assert_eq!(widgets::RADIUS.nw, 0);
    }
}
