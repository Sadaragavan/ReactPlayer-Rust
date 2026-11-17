//! One-line messages for the bottom of the window: what the clipboard did, why
//! a file would not open, that a preset was applied. Errors are the only thing
//! allowed to use a colour other than white, grey or the accent.

use eframe::egui::{self, Align2, RichText, Vec2};

use super::theme::{self, ACCENT, DANGER, MUTED, TEXT};

/// How long a message stays up.
pub const LIFETIME_MS: i64 = 4_500;

/// What kind of message this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Error,
}

impl Level {
    fn colour(self) -> eframe::egui::Color32 {
        match self {
            Self::Info => MUTED,
            Self::Success => ACCENT,
            Self::Error => DANGER,
        }
    }
}

/// A single message.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub text: String,
    pub level: Level,
    pub born_ms: i64,
}

/// The stack of messages, newest last.
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Level::Info, text);
    }

    pub fn success(&mut self, text: impl Into<String>) {
        self.push(Level::Success, text);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push(Level::Error, text);
    }

    fn push(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        // Repeating the same message should not stack up.
        self.items.retain(|toast| toast.text != text);
        self.items.push(Toast {
            text,
            level,
            born_ms: crate::model::session::now_ms(),
        });
        while self.items.len() > 3 {
            self.items.remove(0);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn last(&self) -> Option<&Toast> {
        self.items.last()
    }

    /// Drop expired messages. Called once per frame.
    pub fn tick(&mut self, now_ms: i64) {
        self.items
            .retain(|toast| now_ms - toast.born_ms < LIFETIME_MS);
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Draw the stack near the bottom of the window.
    pub fn show(&self, ctx: &egui::Context) {
        if self.items.is_empty() {
            return;
        }
        let screen = ctx.screen_rect();
        let anchor = egui::pos2(screen.center().x, screen.bottom() - 84.0);
        egui::Area::new(egui::Id::new("toasts"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -48.0))
            .fixed_pos(anchor)
            .interactable(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    for toast in &self.items {
                        egui::Frame::new()
                            .fill(theme::BG)
                            .stroke(theme::hairline(theme::LINE_STRONG))
                            .corner_radius(theme::RADIUS)
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&toast.text)
                                        .color(toast.level.colour())
                                        .size(12.5),
                                );
                            });
                    }
                });
            });
        // Toasts are the only thing that asks for a repaint while idle.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

/// Text colour for a level, exposed for tests.
pub fn colour_of(level: Level) -> eframe::egui::Color32 {
    match level {
        Level::Info => MUTED,
        Level::Success => ACCENT,
        Level::Error => DANGER,
    }
}

/// The default text colour, so a status line matches a toast.
pub fn body_colour() -> eframe::egui::Color32 {
    TEXT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_stack_but_do_not_repeat() {
        let mut toasts = Toasts::new();
        toasts.info("saved");
        toasts.info("saved");
        assert_eq!(toasts.len(), 1);
        toasts.error("nope");
        assert_eq!(toasts.len(), 2);
        assert_eq!(toasts.last().map(|t| t.level), Some(Level::Error));
    }

    #[test]
    fn older_messages_expire() {
        let mut toasts = Toasts::new();
        toasts.success("copied");
        let born = toasts.last().expect("one toast").born_ms;
        toasts.tick(born + LIFETIME_MS - 1);
        assert!(!toasts.is_empty());
        toasts.tick(born + LIFETIME_MS);
        assert!(toasts.is_empty());
    }

    #[test]
    fn at_most_three_stay_up() {
        let mut toasts = Toasts::new();
        for index in 0..6 {
            toasts.info(format!("message {index}"));
        }
        assert_eq!(toasts.len(), 3);
        assert_eq!(toasts.last().map(String::as_str), Some("message 5"));
    }

    #[test]
    fn levels_have_distinct_colours() {
        assert_eq!(colour_of(Level::Error), DANGER);
        assert_eq!(colour_of(Level::Success), ACCENT);
        assert_eq!(colour_of(Level::Info), MUTED);
        assert_eq!(body_colour(), TEXT);
    }
}
