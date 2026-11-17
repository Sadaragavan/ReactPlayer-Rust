//! On-disk persistence.
//!
//! Layout (Arch Linux, per the spec):
//!
//! * config: `~/.config/reaction-player`
//! * data:   `~/.local/share/reaction-player`
//!
//! Sessions are `data/sessions/<id>.json`, presets live in
//! `config/presets.json`, preferences in `config/preferences.json`, session
//! thumbnails in `data/thumbnails/<id>.png` and demo clips in `data/demo/`.
//!
//! All documents are wrapped in a small envelope carrying a `version` field,
//! and unknown fields are ignored on load so newer files degrade gracefully.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{AppError, AppResult};

use super::presets::{Preset, PresetLibrary};
use super::session::{Session, SessionSummary, SCHEMA_VERSION};
use super::settings::Settings;

/// A versioned JSON envelope.
#[derive(Debug, Serialize)]
struct Envelope<'a, T> {
    version: u32,
    #[serde(flatten)]
    body: &'a T,
}

/// Locations used by the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Directory for user-editable JSON (preferences, presets).
    pub config_dir: PathBuf,
    /// Directory for media, thumbnails and sessions.
    pub data_dir: PathBuf,
}

impl AppPaths {
    /// Build from `directories::ProjectDirs`, honouring the documented layout.
    pub fn discover() -> AppResult<Self> {
        let dirs = directories::ProjectDirs::from("dev", "reaction", "reaction-player")
            .ok_or_else(|| AppError::new("Could not determine the OS data directories"))?;
        // On Linux ProjectDirs::from yields exactly
        // `~/.config/reaction-player` and `~/.local/share/reaction-player`.
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
        })
    }

    /// Explicit paths (used by tests and by `--data-dir` overrides).
    pub fn new(config_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
            data_dir: data_dir.into(),
        }
    }

    /// `config/preferences.json`
    pub fn preferences_file(&self) -> PathBuf {
        self.config_dir.join("preferences.json")
    }

    /// `config/presets.json`
    pub fn presets_file(&self) -> PathBuf {
        self.config_dir.join("presets.json")
    }

    /// `data/sessions`
    pub fn sessions_dir(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    /// `data/sessions/<id>.json`
    pub fn session_file(&self, id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{}.json", sanitize_id(id)))
    }

    /// `data/thumbnails`
    pub fn thumbnails_dir(&self) -> PathBuf {
        self.data_dir.join("thumbnails")
    }

    /// `data/thumbnails/<id>.png`
    pub fn thumbnail_file(&self, id: &str) -> PathBuf {
        self.thumbnails_dir().join(format!("{}.png", sanitize_id(id)))
    }

    /// `data/demo`
    pub fn demo_dir(&self) -> PathBuf {
        self.data_dir.join("demo")
    }

    /// Create all directories (idempotent).
    pub fn ensure(&self) -> AppResult<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.sessions_dir(),
            &self.thumbnails_dir(),
            &self.demo_dir(),
        ] {
            fs::create_dir_all(dir).map_err(|e| {
                AppError::new(format!("Could not create {}: {e}", dir.display()))
            })?;
        }
        Ok(())
    }

    /// Delete every file the app owns. Used by "Clear local app data".
    pub fn clear(&self) -> AppResult<()> {
        for dir in [
            &self.config_dir,
            &self.sessions_dir(),
            &self.thumbnails_dir(),
            &self.demo_dir(),
        ] {
            if dir.exists() {
                fs::remove_dir_all(dir)
                    .map_err(|e| AppError::new(format!("Could not clear {}: {e}", dir.display())))?;
            }
        }
        Ok(())
    }
}

/// Keep ids alphanumeric so they are safe as file names on every OS.
pub fn sanitize_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    if cleaned.is_empty() {
        "session".to_string()
    } else {
        cleaned
    }
}

/// Generate a new session id from the current time.
pub fn generate_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("s-{:x}-{:03}", now.as_millis(), now.subsec_millis())
}

/// File-backed store for all application state.
#[derive(Debug, Clone)]
pub struct Store {
    /// Filesystem locations.
    pub paths: AppPaths,
}

impl Store {
    /// Create a store, ensuring every directory exists.
    pub fn open(paths: AppPaths) -> AppResult<Self> {
        paths.ensure()?;
        Ok(Self { paths })
    }

    /// Open with paths discovered from the OS.
    pub fn discover() -> AppResult<Self> {
        Self::open(AppPaths::discover()?)
    }

    // ---------------------------------------------------------------- JSON

    /// Write a versioned JSON document atomically (write to `.tmp`, rename).
    ///
    /// The `version` key is injected by the envelope. Types that already carry
    /// their own `version` field (`Session`) must use [`Self::write_json_plain`]
    /// instead, otherwise the flattened envelope would duplicate the key and
    /// fail to deserialize.
    pub fn write_json<T: Serialize>(&self, path: &Path, value: &T) -> AppResult<()> {
        let envelope = Envelope {
            version: SCHEMA_VERSION,
            body: value,
        };
        self.write_json_plain(path, &envelope)
    }

    /// Write a JSON document exactly as given (no extra `version` key).
    ///
    /// Used for [`Session`], which serializes its own `version`.
    pub fn write_json_plain<T: Serialize>(&self, path: &Path, value: &T) -> AppResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(value)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Read a JSON document, ignoring unknown fields and repairing nothing.
    pub fn read_json<T: DeserializeOwned>(&self, path: &Path) -> AppResult<T> {
        let bytes = fs::read(path)?;
        let value = serde_json::from_slice(&bytes)?;
        Ok(value)
    }

    // ------------------------------------------------------------ settings

    /// Load preferences, falling back to defaults when absent or corrupt.
    pub fn load_settings(&self) -> Settings {
        match self.read_json::<Settings>(&self.paths.preferences_file()) {
            Ok(s) => Settings {
                preferences: s.preferences.sanitized(),
            },
            Err(_) => Settings::default(),
        }
    }

    /// Persist preferences.
    pub fn save_settings(&self, settings: &Settings) -> AppResult<()> {
        self.write_json(&self.paths.preferences_file(), settings)
    }

    // -------------------------------------------------------------- presets

    /// Load the preset library (built-ins + user presets).
    pub fn load_presets(&self) -> PresetLibrary {
        #[derive(serde::Deserialize)]
        struct Doc {
            #[serde(default)]
            presets: Vec<Preset>,
        }
        match self.read_json::<Doc>(&self.paths.presets_file()) {
            Ok(doc) => PresetLibrary::with_user(doc.presets),
            Err(_) => PresetLibrary::default(),
        }
    }

    /// Persist only the user presets.
    pub fn save_presets(&self, library: &PresetLibrary) -> AppResult<()> {
        #[derive(serde::Serialize)]
        struct Doc {
            version: u32,
            presets: Vec<Preset>,
        }
        // The version is also written by `write_json`'s envelope; a duplicated
        // key would be a serde error, so write this document directly.
        let doc = Doc {
            version: SCHEMA_VERSION,
            presets: library.user_presets(),
        };
        if let Some(parent) = self.paths.presets_file().parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(&doc)?;
        let path = self.paths.presets_file();
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    // ------------------------------------------------------------- sessions

    /// Save a session. Updates `modified_unix`.
    pub fn save_session(&self, session: &Session) -> AppResult<()> {
        let mut s = session.clone();
        s.modified_unix = super::session::now_unix();
        // `Session` carries its own `version` field; writing it through the
        // envelope would emit `version` twice.
        self.write_json_plain(&self.paths.session_file(&s.id), &s)
    }

    /// Load a session by id.
    pub fn load_session(&self, id: &str) -> AppResult<Session> {
        let mut s = self.read_json::<Session>(&self.paths.session_file(id))?;
        s.sanitize();
        Ok(s)
    }

    /// Delete a session and its thumbnail.
    pub fn delete_session(&self, id: &str) -> AppResult<()> {
        let path = self.paths.session_file(id);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        let thumb = self.paths.thumbnail_file(id);
        if thumb.exists() {
            let _ = fs::remove_file(thumb);
        }
        Ok(())
    }

    /// List sessions, newest first. Corrupt files are skipped.
    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let mut out = Vec::new();
        let dir = self.paths.sessions_dir();
        let Ok(entries) = fs::read_dir(&dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match self.read_json::<Session>(&path) {
                Ok(s) => out.push(s.summary()),
                Err(_) => continue,
            }
        }
        out.sort_by(|a, b| b.modified_unix.cmp(&a.modified_unix));
        out
    }

    /// Duplicate a session under a new id and name.
    pub fn duplicate_session(&self, id: &str) -> AppResult<Session> {
        let mut s = self.load_session(id)?;
        s.id = generate_id();
        s.name = format!("{} copy", s.name);
        s.created_unix = super::session::now_unix();
        s.thumbnail = None;
        self.save_session(&s)?;
        Ok(s)
    }

    /// Export a session as a standalone `.rplay.json` file.
    pub fn export_session(&self, session: &Session, dest: &Path) -> AppResult<()> {
        // Session already contains its "version" field.
        let json = serde_json::to_vec_pretty(session)?;
        fs::write(dest, json)?;
        Ok(())
    }

    /// Import a `.rplay.json` file, assigning a fresh id so it never
    /// overwrites an existing session.
    pub fn import_session(&self, src: &Path) -> AppResult<Session> {
        let bytes = fs::read(src)?;
        let mut s: Session = serde_json::from_slice(&bytes)?;
        s.id = generate_id();
        s.thumbnail = None;
        s.modified_unix = super::session::now_unix();
        s.sanitize();
        self.save_session(&s)?;
        Ok(s)
    }

    /// Save a thumbnail PNG for a session and record it in the session file.
    pub fn save_thumbnail(&self, session_id: &str, png_path: &Path) -> AppResult<String> {
        let dest = self.paths.thumbnail_file(session_id);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(png_path, &dest)?;
        Ok(format!("{}.png", sanitize_id(session_id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::layout::CanvasAspect;
    use crate::model::session::{HardwareAccel, LayerConfig, LayerRole};
    use crate::model::geom::NormalizedRect;

    fn temp_store(name: &str) -> Store {
        let base = std::env::temp_dir().join(format!(
            "reaction-player-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        Store::open(AppPaths::new(base.join("config"), base.join("data"))).unwrap()
    }

    fn sample() -> Session {
        let mut s = Session::new("sess-a", "My Session", CanvasAspect::Widescreen);
        s.layers.push(LayerConfig::new(
            1,
            "Source",
            LayerRole::Source,
            "/tmp/a.mp4",
            NormalizedRect::full(),
            0,
        ));
        s.sanitize();
        s
    }

    #[test]
    fn paths_follow_xdg_layout() {
        let p = AppPaths::new("/home/u/.config/reaction-player", "/home/u/.local/share/reaction-player");
        assert_eq!(
            p.preferences_file(),
            PathBuf::from("/home/u/.config/reaction-player/preferences.json")
        );
        assert_eq!(
            p.demo_dir(),
            PathBuf::from("/home/u/.local/share/reaction-player/demo")
        );
    }

    #[test]
    fn session_round_trip_on_disk() {
        let store = temp_store("session");
        let s = sample();
        store.save_session(&s).unwrap();
        let back = store.load_session("sess-a").unwrap();
        assert_eq!(back.name, "My Session");
        assert_eq!(back.layers.len(), 1);
        let list = store.list_sessions();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "sess-a");
    }

    #[test]
    fn delete_session_removes_file() {
        let store = temp_store("delete");
        store.save_session(&sample()).unwrap();
        assert!(store.paths.session_file("sess-a").exists());
        store.delete_session("sess-a").unwrap();
        assert!(!store.paths.session_file("sess-a").exists());
        assert!(store.list_sessions().is_empty());
    }

    #[test]
    fn settings_and_presets_round_trip() {
        let store = temp_store("settings");
        let mut settings = Settings::default();
        settings.preferences.hardware = HardwareAccel::SoftwareOnly;
        settings.preferences.seek_step_ms = 2_000;
        store.save_settings(&settings).unwrap();
        let back = store.load_settings();
        assert_eq!(back, settings);

        let mut lib = store.load_presets();
        lib.upsert(Preset::new(
            "custom",
            "Custom",
            vec![crate::model::PresetRole {
                role: LayerRole::Source,
                rect: NormalizedRect::full(),
                crop: Default::default(),
                opacity: 1.0,
                fit: Default::default(),
                z_index: 0,
            }],
        ));
        store.save_presets(&lib).unwrap();
        let back = store.load_presets();
        assert!(back.get("custom").is_some());
        assert!(back.get("builtin-split").is_some());
    }

    #[test]
    fn export_import_assigns_new_id() {
        let store = temp_store("export");
        let s = sample();
        let dest = std::env::temp_dir().join(format!("rplay-export-{}.rplay.json", std::process::id()));
        store.export_session(&s, &dest).unwrap();
        let imported = store.import_session(&dest).unwrap();
        assert_ne!(imported.id, s.id);
        assert_eq!(imported.name, s.name);
        let _ = fs::remove_file(dest);
    }

    #[test]
    fn corrupt_session_files_are_skipped() {
        let store = temp_store("corrupt");
        store.save_session(&sample()).unwrap();
        fs::write(store.paths.sessions_dir().join("broken.json"), b"{not json").unwrap();
        let list = store.list_sessions();
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn clear_removes_everything() {
        let store = temp_store("clear");
        store.save_session(&sample()).unwrap();
        store.save_settings(&Settings::default()).unwrap();
        store.paths.clear().unwrap();
        assert!(!store.paths.sessions_dir().exists());
        assert!(store.list_sessions().is_empty());
    }

    #[test]
    fn ids_are_sanitized() {
        assert_eq!(sanitize_id("a/b c"), "a-b-c");
        assert_eq!(sanitize_id(""), "session");
        assert!(generate_id().starts_with("s-"));
    }
}
