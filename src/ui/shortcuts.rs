//! The single, tested keyboard-shortcut dispatcher.
//!
//! Every keyboard action in the app flows through [`Action`]: the app feeds raw
//! keys and text events in, gets zero or more actions out, and executes them.
//! Keeping the mapping in one pure function is what makes the cheat sheet, the
//! tests, and the actual behaviour impossible to drift apart.

use eframe::egui::Key;

/// A keyboard-modifier state, decoupled from egui so tests are trivial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    /// Shift held.
    pub shift: bool,
    /// Control held.
    pub ctrl: bool,
    /// Alt held.
    pub alt: bool,
    /// Command (macOS) / control (elsewhere).
    pub command: bool,
}

impl Mods {
    /// No modifiers.
    pub const NONE: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: false,
        command: false,
    };
    /// Shift only.
    pub const SHIFT: Mods = Mods {
        shift: true,
        ..Mods::NONE
    };

    /// True when any modifier that changes shortcut meaning is down.
    pub fn any(&self) -> bool {
        self.shift || self.ctrl || self.alt || self.command
    }
}

/// What a keyboard event means to the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Play / pause all layers.
    TogglePlay,
    /// Link / unlink the transports.
    ToggleLink,
    /// Toggle theater mode.
    ToggleTheater,
    /// Toggle fullscreen.
    ToggleFullscreen,
    /// Toggle the layers panel.
    ToggleLayersPanel,
    /// Step the timeline by a signed number of microseconds.
    StepMicros(i64),
    /// Step a single frame from the master layer; `forward = false` steps back.
    StepFrame {
        /// Forward when true.
        forward: bool,
    },
    /// Nudge the selected layer's offset by a signed number of microseconds.
    NudgeOffset(i64),
    /// Bring the selected layer forward in z.
    BringForward,
    /// Send the selected layer backward in z.
    SendBackward,
    /// Bring the selected layer to the front.
    BringToFront,
    /// Send the selected layer to the back.
    SendToBack,
    /// Move the wipe divider by a signed percentage.
    NudgeWipe(i32),
    /// Capture a frame in the Alignment Studio.
    Capture,
    /// Toggle mute on the designated master-audio layer.
    ToggleMasterMute,
    /// Cycle the Alignment Studio's compare mode.
    CycleCompareMode,
    /// Escape: dismiss/close the topmost thing (handled by the app cascade).
    Escape,
    /// Show the cheat sheet.
    ToggleCheatSheet,
}

/// How the timeline is currently behaving, which changes what Left/Right mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    /// Playing: arrows seek by the configured step.
    Playing,
    /// Paused: arrows step one frame.
    Paused,
}

/// Framing of the immediate keyboard state.
#[derive(Debug, Clone, Copy)]
pub struct KeyPress {
    /// The logical key.
    pub key: Key,
    /// Modifiers at the time of the press.
    pub mods: Mods,
    /// True when this is an auto-repeat (held down).
    pub repeat: bool,
}

/// Which layer the offset steppers act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetNudgeSpan {
    /// 1 ms.
    Fine,
    /// 10 ms.
    Medium,
    /// 100 ms.
    Coarse,
}

impl OffsetNudgeSpan {
    /// The span in microseconds.
    pub fn micros(self) -> i64 {
        match self {
            OffsetNudgeSpan::Fine => 1_000,
            OffsetNudgeSpan::Medium => 10_000,
            OffsetNudgeSpan::Coarse => 100_000,
        }
    }

    /// The span used when the stepper is clicked without a modifier.
    pub const DEFAULT: OffsetNudgeSpan = OffsetNudgeSpan::Fine;
}

/// Translate one key press into an [`Action`].
///
/// Returns `None` for keys that mean nothing (or that must be left to the
/// text field / IME when one is focused the caller has already checked).
pub fn map_key(press: KeyPress, state: TransportState, seek_step_us: i64) -> Option<Action> {
    // Alt-modified keys belong to the OS/other layers; command/ctrl combos are
    // text-editing commands. Both are deliberately ignored here.
    if press.mods.alt || press.mods.ctrl || press.mods.command {
        return None;
    }
    let shift = press.mods.shift;
    match press.key {
        Key::Space => Some(Action::TogglePlay),
        Key::L => Some(Action::ToggleLink),
        Key::T => Some(Action::ToggleTheater),
        Key::F => Some(Action::ToggleFullscreen),
        Key::M => Some(Action::ToggleMasterMute),
        Key::C => Some(Action::Capture),
        Key::P => Some(Action::CycleCompareMode),
        Key::Escape => Some(Action::Escape),
        Key::OpenBracket => Some(if shift {
            Action::SendToBack
        } else {
            Action::SendBackward
        }),
        Key::CloseBracket => Some(if shift {
            Action::BringToFront
        } else {
            Action::BringForward
        }),
        Key::Comma => Some(Action::NudgeOffset(-OffsetNudgeSpan::DEFAULT.micros())),
        Key::Period => Some(Action::NudgeOffset(OffsetNudgeSpan::DEFAULT.micros())),
        Key::ArrowLeft | Key::ArrowRight => {
            let forward = press.key == Key::ArrowRight;
            match state {
                TransportState::Playing => {
                    let step = if shift { seek_step_us * 10 } else { seek_step_us };
                    Some(Action::StepMicros(if forward { step } else { -step }))
                }
                TransportState::Paused => Some(Action::StepFrame { forward }),
            }
        }
        _ => None,
    }
}

/// Translate text input into an action.
///
/// `?` never arrives as a key on layouts where it needs Shift, so it is
/// detected from the text event (the character itself). Text events also fire
/// for ordinary typing, which is why the caller must only consult this when no
/// text field has focus.
pub fn map_text(text: &str) -> Option<Action> {
    if text == "?" {
        Some(Action::ToggleCheatSheet)
    } else {
        None
    }
}

/// The wipe step used by the Alignment Studio's `[`/`]`-less nudge controls.
pub const WIPE_STEP_PCT: i32 = 1;
/// The wipe step with Shift held.
pub const WIPE_STEP_PCT_LARGE: i32 = 10;

/// One line of the cheat sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheatSheetRow {
    /// The key or key combination, as displayed.
    pub keys: &'static str,
    /// What it does.
    pub what: &'static str,
}

/// The cheat sheet, in the order it is displayed.
pub const CHEAT_SHEET: &[CheatSheetRow] = &[
    CheatSheetRow { keys: "Space", what: "Play / pause" },
    CheatSheetRow { keys: "L", what: "Link transports" },
    CheatSheetRow { keys: "T", what: "Theater mode" },
    CheatSheetRow { keys: "F", what: "Fullscreen" },
    CheatSheetRow { keys: "?", what: "This sheet" },
    CheatSheetRow { keys: "Left / Right", what: "Seek while playing, step a frame while paused (Shift = 10x)" },
    CheatSheetRow { keys: ", / .", what: "Nudge the selected offset by 1 ms" },
    CheatSheetRow { keys: "[ / ]", what: "Move the selected layer down / up one z step" },
    CheatSheetRow { keys: "Shift+[ / Shift+]", what: "Send to back / bring to front" },
    CheatSheetRow { keys: "C", what: "Capture a frame for alignment" },
    CheatSheetRow { keys: "P", what: "Cycle Onion / Wipe" },
    CheatSheetRow { keys: "M", what: "Mute / unmute master audio" },
    CheatSheetRow { keys: "Esc", what: "Close the topmost window" },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key) -> KeyPress {
        KeyPress {
            key,
            mods: Mods::NONE,
            repeat: false,
        }
    }

    fn press_shift(key: Key) -> KeyPress {
        KeyPress {
            key,
            mods: Mods::SHIFT,
            repeat: false,
        }
    }

    #[test]
    fn space_toggles_play() {
        assert_eq!(
            map_key(press(Key::Space), TransportState::Paused, 5_000_000),
            Some(Action::TogglePlay)
        );
    }

    #[test]
    fn letter_shortcuts_map() {
        let cases = [
            (Key::L, Action::ToggleLink),
            (Key::T, Action::ToggleTheater),
            (Key::F, Action::ToggleFullscreen),
            (Key::M, Action::ToggleMasterMute),
            (Key::C, Action::Capture),
            (Key::P, Action::CycleCompareMode),
            (Key::Escape, Action::Escape),
        ];
        for (key, action) in cases {
            assert_eq!(
                map_key(press(key), TransportState::Paused, 5_000_000),
                Some(action),
                "{key:?}"
            );
        }
    }

    #[test]
    fn arrows_seek_while_playing_and_step_frames_while_paused() {
        assert_eq!(
            map_key(press(Key::ArrowRight), TransportState::Playing, 5_000_000),
            Some(Action::StepMicros(5_000_000))
        );
        assert_eq!(
            map_key(press(Key::ArrowLeft), TransportState::Playing, 5_000_000),
            Some(Action::StepMicros(-5_000_000))
        );
        assert_eq!(
            map_key(press_shift(Key::ArrowRight), TransportState::Playing, 5_000_000),
            Some(Action::StepMicros(50_000_000)),
            "Shift multiplies the seek step by ten"
        );
        assert_eq!(
            map_key(press(Key::ArrowRight), TransportState::Paused, 5_000_000),
            Some(Action::StepFrame { forward: true })
        );
        assert_eq!(
            map_key(press(Key::ArrowLeft), TransportState::Paused, 5_000_000),
            Some(Action::StepFrame { forward: false })
        );
    }

    #[test]
    fn offset_nudges_are_one_millisecond() {
        assert_eq!(
            map_key(press(Key::Comma), TransportState::Paused, 5_000_000),
            Some(Action::NudgeOffset(-1_000))
        );
        assert_eq!(
            map_key(press(Key::Period), TransportState::Paused, 5_000_000),
            Some(Action::NudgeOffset(1_000))
        );
        assert_eq!(
            map_key(press_shift(Key::Period), TransportState::Paused, 5_000_000),
            Some(Action::NudgeOffset(1_000)),
            "Shift does not change the offset nudge in this build"
        );
    }

    #[test]
    fn bracket_shortcuts_reorder_z() {
        assert_eq!(
            map_key(press(Key::OpenBracket), TransportState::Paused, 5_000_000),
            Some(Action::SendBackward)
        );
        assert_eq!(
            map_key(press(Key::CloseBracket), TransportState::Paused, 5_000_000),
            Some(Action::BringForward)
        );
        assert_eq!(
            map_key(press_shift(Key::OpenBracket), TransportState::Paused, 5_000_000),
            Some(Action::SendToBack)
        );
        assert_eq!(
            map_key(press_shift(Key::CloseBracket), TransportState::Paused, 5_000_000),
            Some(Action::BringToFront)
        );
    }

    #[test]
    fn modified_letters_are_left_alone() {
        // Cmd/Ctrl combos must reach the text fields (copy, paste, ...).
        let mut p = press(Key::C);
        p.mods.command = true;
        assert_eq!(map_key(p, TransportState::Paused, 5_000_000), None);
        let mut p = press(Key::Space);
        p.mods.alt = true;
        assert_eq!(map_key(p, TransportState::Paused, 5_000_000), None);
    }

    #[test]
    fn unassigned_keys_do_nothing() {
        for key in [Key::A, Key::B, Key::Z, Key::Enter, Key::Tab, Key::ArrowUp] {
            assert_eq!(map_key(press(key), TransportState::Paused, 5_000_000), None);
        }
    }

    #[test]
    fn question_mark_comes_from_text_events() {
        assert_eq!(map_text("?"), Some(Action::ToggleCheatSheet));
        assert_eq!(map_text("a"), None);
        assert_eq!(map_text(""), None);
    }

    #[test]
    fn every_cheat_sheet_row_is_documented_and_unique() {
        let mut keys: Vec<&str> = CHEAT_SHEET.iter().map(|r| r.keys).collect();
        let before = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), before, "duplicate cheat-sheet rows");
        assert!(CHEAT_SHEET.iter().any(|r| r.keys == "Space"));
        assert!(CHEAT_SHEET.iter().any(|r| r.what.contains("Play")));
    }

    #[test]
    fn offset_spans_are_ordered() {
        assert!(OffsetNudgeSpan::Fine.micros() < OffsetNudgeSpan::Medium.micros());
        assert!(OffsetNudgeSpan::Medium.micros() < OffsetNudgeSpan::Coarse.micros());
        assert_eq!(OffsetNudgeSpan::DEFAULT.micros(), 1_000);
    }

    #[test]
    fn mods_default_has_nothing_down() {
        assert!(!Mods::NONE.any());
        assert!(Mods::SHIFT.any());
    }

    #[test]
    fn repeat_key_still_maps() {
        let mut p = press(Key::ArrowRight);
        p.repeat = true;
        assert_eq!(
            map_key(p, TransportState::Playing, 5_000_000),
            Some(Action::StepMicros(5_000_000))
        );
    }
}
