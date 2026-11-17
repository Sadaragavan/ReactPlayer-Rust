//! Transient toasts shown at the bottom of the window.
//!
//! Toasts are plain data so they can be created from any thread; the app
//! drains nothing -- each toast exists until its lifetime expires or the user
//! dismisses it.

use std::time::{Duration, Instant};

/// Severity of a toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Confirmation of an action.
    Info,
    /// A recoverable problem.
    Error,
}

/// How long each kind lives.
pub fn lifetime(kind: ToastKind) -> Duration {
    match kind {
        ToastKind::Info => Duration::from_millis(2_600),
        ToastKind::Error => Duration::from_millis(6_000),
    }
}

/// One toast.
#[derive(Debug, Clone)]
pub struct Toast {
    /// The text.
    pub message: String,
    /// Severity.
    pub kind: ToastKind,
    /// When it appeared.
    pub at: Instant,
}

impl Toast {
    /// An informational toast.
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ToastKind::Info,
            at: Instant::now(),
        }
    }

    /// An error toast.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ToastKind::Error,
            at: Instant::now(),
        }
    }

    /// True when the toast has outlived its kind's lifetime.
    pub fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.at) >= lifetime(self.kind)
    }

    /// Remaining fraction of the toast's life, `0..1`.
    pub fn remaining(&self, now: Instant) -> f32 {
        let total = lifetime(self.kind).as_secs_f32();
        let used = now.duration_since(self.at).as_secs_f32();
        (1.0 - used / total).clamp(0.0, 1.0)
    }
}

/// The toast queue.
#[derive(Debug, Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

/// The oldest toasts are dropped once there are more than this many.
pub const MAX_TOASTS: usize = 4;

impl Toasts {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a toast, dropping the oldest when full.
    pub fn push(&mut self, toast: Toast) {
        self.items.push(toast);
        while self.items.len() > MAX_TOASTS {
            self.items.remove(0);
        }
    }

    /// Remove expired toasts and return the live ones, oldest first.
    pub fn live(&mut self) -> &[Toast] {
        let now = Instant::now();
        self.items.retain(|t| !t.expired(now));
        &self.items
    }

    /// Number of live toasts.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when no toast is queued.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Dismiss the toast at `index`.
    pub fn dismiss(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_is_capped() {
        let mut t = Toasts::new();
        for i in 0..10 {
            t.push(Toast::info(format!("toast {i}")));
        }
        assert_eq!(t.len(), MAX_TOASTS);
        assert_eq!(t.live()[0].message, "toast 6");
    }

    #[test]
    fn errors_live_longer_than_info() {
        assert!(lifetime(ToastKind::Error) > lifetime(ToastKind::Info));
    }

    #[test]
    fn expiry_respects_the_clock() {
        let toast = Toast {
            message: "x".into(),
            kind: ToastKind::Info,
            at: Instant::now() - Duration::from_secs(60),
        };
        assert!(toast.expired(Instant::now()));
        assert_eq!(toast.remaining(Instant::now()), 0.0);
    }

    #[test]
    fn fresh_toast_is_full() {
        let toast = Toast::info("x");
        assert!(!toast.expired(Instant::now()));
        assert!(toast.remaining(Instant::now()) > 0.9);
    }

    #[test]
    fn dismiss_and_clear() {
        let mut t = Toasts::new();
        t.push(Toast::info("a"));
        t.push(Toast::info("b"));
        t.dismiss(0);
        assert_eq!(t.len(), 1);
        assert_eq!(t.live()[0].message, "b");
        t.clear();
        assert!(t.is_empty());
    }
}
