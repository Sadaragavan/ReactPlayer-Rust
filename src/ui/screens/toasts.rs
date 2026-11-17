//! Toast rendering: one stack in the bottom-right corner, above everything.

use eframe::egui::{self, Align2, Pos2, RichText, Stroke, Ui, Vec2};

use crate::app::App;
use crate::ui::theme;
use crate::ui::toast::ToastKind;

/// Draw the live toasts.
pub fn show(app: &mut App, ui: &mut Ui) {
    let window = ui.ctx().content_rect();
    let mut dismiss: Option<usize> = None;
    let mut y = window.bottom() - 22.0;
    let toasts: Vec<(usize, String, ToastKind, f32)> = app
        .toasts
        .live()
        .iter()
        .enumerate()
        .map(|(i, t)| (i, t.message.clone(), t.kind, t.remaining(std::time::Instant::now())))
        .collect();

    // Newest at the bottom, growing upwards.
    for (i, message, kind, remaining) in toasts.into_iter().rev() {
        let galley = ui.painter().layout(
            message.clone(),
            egui::FontId::proportional(12.0),
            theme::TEXT,
            320.0,
        );
        let size = Vec2::new(galley.size().x + 24.0, galley.size().y + 16.0);
        let rect = egui::Rect::from_min_size(
            Pos2::new(window.right() - size.x - 18.0, y - size.y),
            size,
        );
        let painter = ui
            .ctx()
            .layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique(("toast", i))));
        painter.rect_filled(rect, 0.0, theme::SURFACE);
        let border = if kind == ToastKind::Error {
            theme::ERROR
        } else {
            theme::BORDER
        };
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, border),
            egui::StrokeKind::Inside,
        );
        painter.galley(
            Pos2::new(rect.left() + 12.0, rect.top() + 8.0),
            galley,
            theme::TEXT,
        );
        // Lifetime bar along the bottom edge.
        let bar = egui::Rect::from_min_size(
            Pos2::new(rect.left(), rect.bottom() - 2.0),
            Vec2::new(rect.width() * remaining, 2.0),
        );
        painter.rect_filled(
            bar,
            0.0,
            if kind == ToastKind::Error {
                theme::ERROR
            } else {
                theme::ACCENT
            },
        );

        // A click anywhere on the toast dismisses it.
        let resp = ui.interact(rect, egui::Id::unique(("toast-hit", i)), egui::Sense::click());
        if resp.clicked() {
            dismiss = Some(i);
        }
        y = rect.top() - 8.0;
    }

    if let Some(i) = dismiss {
        app.toasts.dismiss(i);
    }

    // Keep animating the lifetime bars while toasts are alive.
    if !app.toasts.is_empty() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
    }
}

/// Toast corner, exposed so tests and other overlays can avoid it.
pub const CORNER: Align2 = Align2::RIGHT_BOTTOM;

/// Convenience for text-only toasts used by tests.
pub fn label_for(kind: ToastKind) -> RichText {
    match kind {
        ToastKind::Info => RichText::new("info").color(theme::TEXT),
        ToastKind::Error => RichText::new("error").color(theme::ERROR),
    }
}
