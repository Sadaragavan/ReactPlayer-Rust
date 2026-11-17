//! Application preferences, persisted as JSON in the OS config directory.

use serde::{Deserialize, Serialize};

use super::session::{HardwareAccel, SCHEMA_VERSION};

/// Default seek step: 5 seconds.
pub const DEFAULT_SEEK_STEP_MS: u64 = 5_000;
/// Default snap sensitivity in pixels.
pub const DEFAULT_SNAP_PX: f32 = 8.0;

/// User-facing preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preferences {
    /// Schema version, always written.
    pub version: u32,
    /// Hardware decode preference.
    pub hardware: HardwareAccel,
    /// Seek step for arrow keys, in milliseconds.
    pub seek_step_ms: u64,
    /// "Keep the sound going" master-audio lending.
    pub keep_sound_going: bool,
    /// Snap sensitivity in pixels of the canvas.
    pub snap_px: f32,
    /// Wallpaper image path; `None` means pure black.
    pub wallpaper: Option<String>,
    /// Session id of the last opened session (for the Continue button).
    pub last_session: Option<String>,
    /// Window width, remembered between runs.
    pub window_size: Option<(f32, f32)>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            hardware: HardwareAccel::Auto,
            seek_step_ms: DEFAULT_SEEK_STEP_MS,
            keep_sound_going: true,
            snap_px: DEFAULT_SNAP_PX,
            wallpaper: None,
            last_session: None,
            window_size: None,
        }
    }
}

impl Preferences {
    /// Clamp values into their supported ranges after loading.
    pub fn sanitized(mut self) -> Self {
        self.version = SCHEMA_VERSION;
        self.seek_step_ms = self.seek_step_ms.clamp(100, 600_000);
        self.snap_px = if self.snap_px.is_finite() {
            self.snap_px.clamp(0.0, 64.0)
        } else {
            DEFAULT_SNAP_PX
        };
        if let Some((w, h)) = self.window_size
            && (!w.is_finite() || !h.is_finite() || w < 320.0 || h < 240.0)
        {
            self.window_size = None;
        }
        self
    }

    /// The seek step in microseconds.
    pub fn seek_step_us(&self) -> i64 {
        (self.seek_step_ms as i64) * 1_000
    }
}

/// The full settings document as stored on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// The preferences themselves, stored under `preferences` for forward
    /// compatibility of future top-level keys.
    pub preferences: Preferences,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            preferences: Preferences::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let p = Preferences::default();
        assert_eq!(p.seek_step_ms, 5_000);
        assert!(p.keep_sound_going);
        assert_eq!(p.hardware, HardwareAccel::Auto);
        assert_eq!(p.snap_px, 8.0);
        assert!(p.wallpaper.is_none());
    }

    #[test]
    fn settings_round_trip() {
        let mut s = Settings::default();
        s.preferences.hardware = HardwareAccel::SoftwareOnly;
        s.preferences.seek_step_ms = 1_250;
        s.preferences.last_session = Some("abc".into());
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
        assert!(json.contains("\"version\""));
    }

    #[test]
    fn sanitize_clamps_bad_values() {
        let p = Preferences {
            seek_step_ms: 0,
            snap_px: f32::NAN,
            window_size: Some((0.0, 0.0)),
            ..Preferences::default()
        }
        .sanitized();
        assert_eq!(p.seek_step_ms, 100);
        assert_eq!(p.snap_px, DEFAULT_SNAP_PX);
        assert!(p.window_size.is_none());
    }

    #[test]
    fn missing_fields_use_defaults() {
        // A v1 file written by an older build with fewer fields must load.
        let json = r#"{"preferences": {"version": 1, "hardware": "Auto",
            "seek_step_ms": 5000, "keep_sound_going": true, "snap_px": 8.0,
            "wallpaper": null, "last_session": null}}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(s.preferences.window_size.is_none());
    }

    #[test]
    fn seek_step_conversion() {
        let p = Preferences::default();
        assert_eq!(p.seek_step_us(), 5_000_000);
    }
}
