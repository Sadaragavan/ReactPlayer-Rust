//! The keyboard, in one place.
//!
//! `action_for` is a pure mapping from a key and its modifiers to an
//! [`Action`]; the app decides what an action means given what is on screen.
//! Keeping the mapping pure is what makes it testable without a window.

use eframe::egui::{Event, Key, Modifiers};

/// What a key press asks for, before the app decides how to carry it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Space: play or pause.
    TogglePlay,
    /// `L`: link or unlink the timelines.
    ToggleLink,
    /// `T`: theater mode.
    ToggleTheater,
    /// `F`: fullscreen.
    ToggleFullscreen,
    /// `]`: one layer up.
    LayerUp,
    /// `[`: one layer down.
    LayerDown,
    /// Shift+`]`: bring to front.
    LayerFront,
    /// Shift+`[`: send to back.
    LayerBack,
    /// Left/right arrow: a frame when paused, a seek step while playing, one
    /// percent of the wipe while comparing.
    Back,
    Forward,
    /// Shift plus an arrow: the whole seek step when paused, ten percent of the
    /// wipe while comparing, a tenth of the timeline while playing.
    BackBig,
    ForwardBig,
    /// `,` and `.`: step whole frames.
    StepBack,
    StepForward,
    /// `C`: capture, in the studio or on the canvas.
    Capture,
    /// `M`: mute the master audio layer.
    MuteMaster,
    /// `P`: show or hide the layers panel.
    ToggleLayers,
    /// Escape: back out of one thing at a time.
    Escape,
    /// `?`: the cheat sheet.
    Help,
}

impl Action {
    /// The keys this action answers to, for the cheat sheet.
    pub fn keys(self) -> &'static str {
        match self {
            Self::TogglePlay => "Space",
            Self::ToggleLink => "L",
            Self::ToggleTheater => "T",
            Self::ToggleFullscreen => "F",
            Self::LayerUp => "]",
            Self::LayerDown => "[",
            Self::LayerFront => "Shift+]",
            Self::LayerBack => "Shift+[",
            Self::Back | Self::Forward => "Left / Right",
            Self::BackBig | Self::ForwardBig => "Shift+Left / Shift+Right",
            Self::StepBack => ",",
            Self::StepForward => ".",
            Self::Capture => "C",
            Self::MuteMaster => "M",
            Self::ToggleLayers => "P",
            Self::Escape => "Esc",
            Self::Help => "?",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::TogglePlay => "Play or pause. In the studio, run both captures.",
            Self::ToggleLink => "Link or unlink the track timelines.",
            Self::ToggleTheater => "Theater mode: canvas only.",
            Self::ToggleFullscreen => "Full screen.",
            Self::LayerUp => "Move the selected layer up one step.",
            Self::LayerDown => "Move the selected layer down one step.",
            Self::LayerFront => "Bring the selected layer to the front.",
            Self::LayerBack => "Send the selected layer to the back.",
            Self::Back | Self::Forward => {
                "One frame while paused, the seek step while playing, 1% of the wipe in the studio."
            }
            Self::BackBig | Self::ForwardBig => {
                "The seek step while paused, 10% of the wipe in the studio."
            }
            Self::StepBack => "Step one frame back.",
            Self::StepForward => "Step one frame forward.",
            Self::Capture => "Capture the current frames for alignment.",
            Self::MuteMaster => "Mute or unmute the master audio layer.",
            Self::ToggleLayers => "Show or hide the layers panel.",
            Self::Escape => "Close the topmost thing: menu, panel, theater, full screen.",
            Self::Help => "This list.",
        }
    }
}

/// Every binding, in cheat-sheet order.
pub const ALL: [Action; 19] = [
    Action::TogglePlay,
    Action::Back,
    Action::Forward,
    Action::StepBack,
    Action::StepForward,
    Action::BackBig,
    Action::ForwardBig,
    Action::ToggleLink,
    Action::LayerUp,
    Action::LayerDown,
    Action::LayerFront,
    Action::LayerBack,
    Action::ToggleTheater,
    Action::ToggleFullscreen,
    Action::Capture,
    Action::MuteMaster,
    Action::ToggleLayers,
    Action::Escape,
    Action::Help,
];

/// The mapping from a key press to an action. Nothing here needs the app state,
/// which is the point: the meaning of [`Action::Back`] is decided later.
pub fn action_for(key: Key, modifiers: Modifiers) -> Option<Action> {
    // Ctrl/Cmd and Alt are left to egui: they belong to text editing and menus.
    if modifiers.command || modifiers.alt {
        return None;
    }
    let shift = modifiers.shift;
    Some(match key {
        Key::Space => Action::TogglePlay,
        Key::L => Action::ToggleLink,
        Key::T => Action::ToggleTheater,
        Key::F => Action::ToggleFullscreen,
        Key::CloseBracket => {
            if shift {
                Action::LayerFront
            } else {
                Action::LayerUp
            }
        }
        Key::OpenBracket => {
            if shift {
                Action::LayerBack
            } else {
                Action::LayerDown
            }
        }
        Key::ArrowLeft => {
            if shift {
                Action::BackBig
            } else {
                Action::Back
            }
        }
        Key::ArrowRight => {
            if shift {
                Action::ForwardBig
            } else {
                Action::Forward
            }
        }
        Key::Comma => Action::StepBack,
        Key::Period => Action::StepForward,
        Key::C => Action::Capture,
        Key::M => Action::MuteMaster,
        Key::P => Action::ToggleLayers,
        Key::Escape => Action::Escape,
        _ => return None,
    })
}

/// Keys pressed this frame, in press order. Panels and menus consume input
/// before this is called, and the app skips it while a text field has focus.
pub fn pressed(ctx: &eframe::egui::Context) -> Vec<Action> {
    if ctx.egui_wants_keyboard_input() {
        return Vec::new();
    }
    ctx.input(|input| {
        let mut actions = Vec::new();
        // `?` arrives as text, not as a key, because it is Shift+/ on most
        // layouts and egui reports the character that was typed.
        if help_typed(&input.events) {
            actions.push(Action::Help);
        }
        for event in &input.events {
            if let Event::Key {
                key,
                pressed: true,
                modifiers,
                repeat: false,
                ..
            } = event
                && let Some(action) = action_for(*key, *modifiers) {
                    actions.push(action);
                }
        }
        actions
    })
}

/// True when the events contain a typed `?`.
pub fn help_typed(events: &[Event]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, Event::Text(text) if text == "?"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Modifiers {
        Modifiers::NONE
    }

    fn shift() -> Modifiers {
        Modifiers::SHIFT
    }

    #[test]
    fn every_documented_binding_maps() {
        assert_eq!(action_for(Key::Space, plain()), Some(Action::TogglePlay));
        assert_eq!(action_for(Key::L, plain()), Some(Action::ToggleLink));
        assert_eq!(action_for(Key::T, plain()), Some(Action::ToggleTheater));
        assert_eq!(
            action_for(Key::F, plain()),
            Some(Action::ToggleFullscreen)
        );
        assert_eq!(action_for(Key::OpenBracket, plain()), Some(Action::LayerDown));
        assert_eq!(action_for(Key::CloseBracket, plain()), Some(Action::LayerUp));
        assert_eq!(
            action_for(Key::OpenBracket, shift()),
            Some(Action::LayerBack)
        );
        assert_eq!(
            action_for(Key::CloseBracket, shift()),
            Some(Action::LayerFront)
        );
        assert_eq!(action_for(Key::ArrowLeft, plain()), Some(Action::Back));
        assert_eq!(action_for(Key::ArrowRight, plain()), Some(Action::Forward));
        assert_eq!(action_for(Key::ArrowLeft, shift()), Some(Action::BackBig));
        assert_eq!(
            action_for(Key::ArrowRight, shift()),
            Some(Action::ForwardBig)
        );
        assert_eq!(action_for(Key::Comma, plain()), Some(Action::StepBack));
        assert_eq!(action_for(Key::Period, plain()), Some(Action::StepForward));
        assert_eq!(action_for(Key::C, plain()), Some(Action::Capture));
        assert_eq!(action_for(Key::M, plain()), Some(Action::MuteMaster));
        assert_eq!(action_for(Key::P, plain()), Some(Action::ToggleLayers));
        assert_eq!(action_for(Key::Escape, plain()), Some(Action::Escape));
    }

    #[test]
    fn unbound_keys_do_nothing() {
        assert_eq!(action_for(Key::Q, plain()), None);
        assert_eq!(action_for(Key::Num1, plain()), None);
        assert_eq!(action_for(Key::Enter, plain()), None);
    }

    #[test]
    fn command_and_alt_are_left_to_egui() {
        let command = Modifiers {
            command: true,
            ..Modifiers::NONE
        };
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(action_for(Key::Space, command), None);
        assert_eq!(action_for(Key::C, command), None);
        assert_eq!(action_for(Key::P, alt), None);
    }

    #[test]
    fn question_mark_arrives_as_text() {
        assert!(help_typed(&[Event::Text("?".to_owned())]));
        assert!(!help_typed(&[Event::Text("x".to_owned())]));
        assert!(!help_typed(&[]));
    }

    #[test]
    fn every_action_has_keys_and_a_description() {
        for action in ALL {
            assert!(!action.keys().is_empty());
            assert!(!action.description().is_empty());
        }
        // The cheat sheet lists each action once.
        for (index, action) in ALL.iter().enumerate() {
            assert!(!ALL[..index].contains(action), "duplicate {action:?}");
        }
    }
}
