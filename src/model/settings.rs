//! Preferences, stored as JSON in the platform config directory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::presets::LayoutType;
use super::session::CanvasAspect;

/// Bumped when the preference shape changes.
pub const SETTINGS_VERSION: u32 = 1;

/// How hard to try to decode on the GPU.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HwAccelPref {
    /// Try hardware, fall back to software silently.
    #[default]
    Auto,
    /// Same device order as `Auto`, but a hardware failure is surfaced in the
    /// layer badge instead of being treated as normal.
    HardwarePreferred,
    /// Never open a hardware device.
    SoftwareOnly,
}

impl HwAccelPref {
    pub const ALL: [Self; 3] = [Self::Auto, Self::HardwarePreferred, Self::SoftwareOnly];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::HardwarePreferred => "Hardware preferred",
            Self::SoftwareOnly => "Software only",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Auto => "Hardware decode when the driver works, software otherwise.",
            Self::HardwarePreferred => {
                "VAAPI / Vulkan / VDPAU on Linux, D3D11VA then DXVA2 on Windows, VideoToolbox on macOS."
            }
            Self::SoftwareOnly => "Always decode on the CPU. Most compatible, highest CPU cost.",
        }
    }

    pub fn allows_hardware(self) -> bool {
        !matches!(self, Self::SoftwareOnly)
    }
}

fn default_seek_step() -> i64 {
    5_000
}

fn default_snap() -> f32 {
    8.0
}

fn default_volume() -> f32 {
    1.0
}

fn default_layout() -> LayoutType {
    LayoutType::SideBySide
}

fn default_aspect() -> CanvasAspect {
    CanvasAspect::Ratio16x9
}

fn default_true() -> bool {
    true
}

/// Everything the Settings screen edits.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Settings {
    pub version: u32,
    #[serde(default)]
    pub hw_accel: HwAccelPref,
    /// Default seek step for `Left`/`Right`, in milliseconds.
    #[serde(default = "default_seek_step")]
    pub seek_step_ms: i64,
    /// Lend the master audio layer's sound to the topmost active layer when the
    /// master has no footage.
    #[serde(default = "default_true")]
    pub keep_sound_going: bool,
    /// Snapping distance, in physical pixels.
    #[serde(default = "default_snap")]
    pub snap_sensitivity_px: f32,
    /// Absolute path to the Home wallpaper; `None` is pure black.
    #[serde(default)]
    pub wallpaper: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub show_layers_panel: bool,
    #[serde(default)]
    pub theater: bool,
    #[serde(default = "default_volume")]
    pub master_volume: f32,
    /// Id of the session `Continue` opens.
    #[serde(default)]
    pub last_session: Option<String>,
    #[serde(default = "default_layout")]
    pub default_layout: LayoutType,
    #[serde(default = "default_aspect")]
    pub default_aspect: CanvasAspect,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            hw_accel: HwAccelPref::Auto,
            seek_step_ms: default_seek_step(),
            keep_sound_going: true,
            snap_sensitivity_px: default_snap(),
            wallpaper: None,
            show_layers_panel: true,
            theater: false,
            master_volume: 1.0,
            last_session: None,
            default_layout: default_layout(),
            default_aspect: default_aspect(),
        }
    }
}

impl Settings {
    /// Clamp anything a hand-edited file could get wrong. Called on load and
    /// before every save.
    pub fn sanitized(mut self) -> Self {
        self.version = SETTINGS_VERSION;
        self.seek_step_ms = self.seek_step_ms.clamp(100, 600_000);
        self.snap_sensitivity_px = self.snap_sensitivity_px.clamp(0.0, 48.0);
        self.master_volume = self.master_volume.clamp(0.0, 1.0);
        if self.last_session.as_deref() == Some("") {
            self.last_session = None;
        }
        if let Some(path) = &self.wallpaper
            && !path.exists() {
                self.wallpaper = None;
            }
        self
    }

    /// The seek step as microseconds.
    pub fn seek_step_us(&self) -> i64 {
        self.seek_step_ms * 1_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec() {
        let s = Settings::default();
        assert_eq!(s.seek_step_ms, 5_000);
        assert_eq!(s.seek_step_us(), 5_000_000);
        assert_eq!(s.snap_sensitivity_px, 8.0);
        assert!(s.keep_sound_going);
        assert!(s.show_layers_panel);
        assert!(!s.theater);
        assert_eq!(s.hw_accel, HwAccelPref::Auto);
        assert_eq!(s.wallpaper, None);
        assert_eq!(s.version, SETTINGS_VERSION);
    }

    #[test]
    fn sanitize_clamps_hand_edited_values() {
        let raw = Settings {
            seek_step_ms: 10,
            snap_sensitivity_px: 1_000.0,
            master_volume: 9.0,
            wallpaper: Some(PathBuf::from("/definitely/not/here.png")),
            ..Settings::default()
        };
        let s = raw.sanitized();
        assert_eq!(s.seek_step_ms, 100);
        assert_eq!(s.snap_sensitivity_px, 48.0);
        assert_eq!(s.master_volume, 1.0);
        assert_eq!(s.wallpaper, None);
    }

    #[test]
    fn json_round_trip_keeps_the_version() {
        let s = Settings {
            hw_accel: HwAccelPref::SoftwareOnly,
            keep_sound_going: false,
            seek_step_ms: 2_500,
            last_session: Some("session-1".into()),
            ..Settings::default()
        };
        let json = serde_json::to_string_pretty(&s).unwrap();
        assert!(json.contains("\"version\""));
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn missing_fields_get_defaults() {
        let back: Settings = serde_json::from_str("{\"version\":1}").unwrap();
        assert_eq!(back.seek_step_ms, 5_000);
        assert_eq!(back.hw_accel, HwAccelPref::Auto);
        assert_eq!(back.default_layout, LayoutType::SideBySide);
    }

    #[test]
    fn hw_accel_pref_gates_hardware() {
        assert!(HwAccelPref::Auto.allows_hardware());
        assert!(HwAccelPref::HardwarePreferred.allows_hardware());
        assert!(!HwAccelPref::SoftwareOnly.allows_hardware());
    }
}
