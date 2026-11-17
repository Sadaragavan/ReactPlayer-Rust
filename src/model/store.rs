//! Persistence: sessions, presets and preferences as versioned JSON in the
//! platform's config and data directories (on Arch: `~/.config/reaction-player`
//! and `~/.local/share/reaction-player`).
//!
//! Every file carries a `version` field. A file written by a newer build is
//! refused with a clear error instead of being half-understood; a file from an
//! older build loads with defaults for the fields it does not have.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::presets::{PRESET_VERSION, Preset};
use super::session::{SESSION_VERSION, Session, new_id, now_ms};
use super::settings::{SETTINGS_VERSION, Settings};

/// Largest thumbnail edge, in pixels.
pub const THUMBNAIL_MAX: u32 = 480;

/// Where everything lives on disk.
pub struct Store {
    config: PathBuf,
    data: PathBuf,
}

/// A row in the Load Session list.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub name: String,
    pub updated_at: i64,
    /// Union timeline length in microseconds.
    pub duration_us: i64,
    pub layer_count: usize,
    /// Layers whose local file is gone.
    pub missing_media: usize,
    pub thumbnail: Option<PathBuf>,
}

#[derive(Serialize, serde::Deserialize)]
struct PresetFile {
    version: u32,
    #[serde(default)]
    presets: Vec<Preset>,
}

impl Store {
    /// The real locations for this user, from the XDG variables on Linux.
    pub fn open() -> Result<Self, String> {
        let dirs = ProjectDirs::from("", "", "reaction-player")
            .ok_or_else(|| "could not determine the user's config directory".to_owned())?;
        Ok(Self::at(dirs.config_dir().to_path_buf(), dirs.data_dir().to_path_buf()))
    }

    /// An explicit pair of roots, used by tests and by `--data-dir`.
    pub fn at(config: impl Into<PathBuf>, data: impl Into<PathBuf>) -> Self {
        Self {
            config: config.into(),
            data: data.into(),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.data.join("sessions")
    }

    pub fn thumbnails_dir(&self) -> PathBuf {
        self.data.join("thumbnails")
    }

    /// Where the demo clips are generated and read from.
    pub fn demo_dir(&self) -> PathBuf {
        self.data.join("demo")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.config.join("preferences.json")
    }

    pub fn presets_path(&self) -> PathBuf {
        self.config.join("presets.json")
    }

    fn session_path(&self, id: &str) -> PathBuf {
        self.sessions_dir().join(format!("{id}.json"))
    }

    // ---- preferences ---------------------------------------------------

    /// Preferences, or the defaults when the file is missing or unreadable.
    pub fn load_settings(&self) -> Settings {
        let path = self.settings_path();
        match read_json::<Settings>(&path) {
            Ok(settings) => {
                if settings.version > SETTINGS_VERSION {
                    return Settings::default();
                }
                settings.sanitized()
            }
            Err(_) => Settings::default(),
        }
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), String> {
        let settings = settings.clone().sanitized();
        write_json(&self.settings_path(), &settings)
    }

    // ---- presets -------------------------------------------------------

    /// Only the user's own presets; the built-ins are compiled in.
    pub fn load_custom_presets(&self) -> Vec<Preset> {
        match read_json::<PresetFile>(&self.presets_path()) {
            Ok(file) if file.version <= PRESET_VERSION => {
                let mut presets: Vec<Preset> =
                    file.presets.into_iter().filter(|p| !p.built_in).collect();
                presets.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                presets
            }
            _ => Vec::new(),
        }
    }

    pub fn save_custom_presets(&self, presets: &[Preset]) -> Result<(), String> {
        let file = PresetFile {
            version: PRESET_VERSION,
            presets: presets.iter().filter(|p| !p.built_in).cloned().collect(),
        };
        write_json(&self.presets_path(), &file)
    }

    // ---- sessions ------------------------------------------------------

    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let dir = self.sessions_dir();
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(session) = self.load_session_file(&path) else {
                continue;
            };
            out.push(SessionSummary {
                thumbnail: session
                    .thumbnail
                    .as_deref()
                    .map(|name| self.data.join(name))
                    .filter(|p| p.exists()),
                id: session.id.clone(),
                name: session.name.clone(),
                updated_at: session.updated_at,
                duration_us: session.union().duration_us(),
                layer_count: session.layers.len(),
                missing_media: session.missing_media().len(),
            });
        }
        // Newest first; ids break ties so two sessions saved inside the same
        // millisecond still come back in a stable order.
        out.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        out
    }

    pub fn load_session(&self, id: &str) -> Result<Session, String> {
        self.load_session_file(&self.session_path(id))
    }

    fn load_session_file(&self, path: &Path) -> Result<Session, String> {
        let session: Session = read_json(path)?;
        if session.version > SESSION_VERSION {
            return Err(format!(
                "{} was written by a newer version of Reaction Player (format {}, this build reads {})",
                session.name, session.version, SESSION_VERSION
            ));
        }
        Ok(session)
    }

    /// Save, creating directories as needed and stamping `updated_at`.
    pub fn save_session(&self, session: &mut Session) -> Result<(), String> {
        session.touch();
        session.version = SESSION_VERSION;
        write_json(&self.session_path(&session.id), session)
    }

    pub fn delete_session(&self, id: &str) -> Result<(), String> {
        let path = self.session_path(id);
        if path.exists() {
            fs::remove_file(&path).map_err(|e| format!("could not delete {}: {e}", path.display()))?;
        }
        let thumb = self.thumbnails_dir().join(format!("{id}.png"));
        if thumb.exists() {
            let _ = fs::remove_file(thumb);
        }
        Ok(())
    }

    /// Write a standalone `.rplay.json` that another machine can import.
    pub fn export_session(&self, session: &Session, path: &Path) -> Result<(), String> {
        write_json(path, session)
    }

    /// Read a `.rplay.json`. Ids are kept; collisions are the caller's problem
    /// (imports land as new files named after the imported id).
    pub fn import_session(&self, path: &Path) -> Result<Session, String> {
        let mut session: Session = read_json(path)?;
        if session.version > SESSION_VERSION {
            return Err(format!(
                "that layout file uses format {}, this build reads {}",
                session.version, SESSION_VERSION
            ));
        }
        if session.id.is_empty() {
            session.id = new_id("session");
        }
        session.version = SESSION_VERSION;
        Ok(session)
    }

    // ---- thumbnails ----------------------------------------------------

    /// Store a PNG thumbnail for `id` and return the path to remember in the
    /// session.
    pub fn save_thumbnail(
        &self,
        id: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<Option<String>, String> {
        if width == 0 || height == 0 || rgba.len() < (width as usize * height as usize * 4) {
            return Err("thumbnail has no pixels".to_owned());
        }
        let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())
            .ok_or_else(|| "thumbnail buffer has the wrong size".to_owned())?;
        let scale = (THUMBNAIL_MAX as f32 / width.max(height) as f32).min(1.0);
        let thumb = if scale < 1.0 {
            image::imageops::resize(
                &image,
                ((width as f32 * scale) as u32).max(1),
                ((height as f32 * scale) as u32).max(1),
                image::imageops::FilterType::Triangle,
            )
        } else {
            image
        };
        let dir = self.thumbnails_dir();
        fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
        let path = dir.join(format!("{id}.png"));
        thumb
            .save_with_format(&path, image::ImageFormat::Png)
            .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        Ok(Some(format!("thumbnails/{id}.png")))
    }

    /// Absolute path of a remembered thumbnail name.
    pub fn thumbnail_path(&self, name: &str) -> PathBuf {
        self.data.join(name)
    }

    // ---- lifecycle -----------------------------------------------------

    /// Remove sessions, presets, preferences and generated demo media. The
    /// caller is expected to have asked first.
    pub fn clear_all(&self) -> Result<(), String> {
        remove_dir_contents(&self.sessions_dir())?;
        remove_dir_contents(&self.thumbnails_dir())?;
        remove_dir_contents(&self.demo_dir())?;
        for path in [self.settings_path(), self.presets_path()] {
            if path.exists() {
                fs::remove_file(&path)
                    .map_err(|e| format!("could not delete {}: {e}", path.display()))?;
            }
        }
        Ok(())
    }

    /// Every file the app knows about, for the "clear data" confirmation.
    pub fn usage(&self) -> (usize, u64) {
        let mut files = 0;
        let mut bytes = 0;
        for dir in [
            self.sessions_dir(),
            self.thumbnails_dir(),
            self.demo_dir(),
            self.config.clone(),
        ] {
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if let Ok(meta) = entry.metadata() {
                        files += 1;
                        bytes += meta.len();
                    }
                }
            }
        }
        (files, bytes)
    }
}

/// Milliseconds-based "now" for the summary list.
pub fn timestamp_now() -> i64 {
    now_ms()
}

fn remove_dir_contents(dir: &Path) -> Result<(), String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        result.map_err(|e| format!("could not delete {}: {e}", path.display()))?;
    }
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("could not parse {}: {e}", path.display()))
}

/// Write pretty JSON through a temporary file so a crash cannot leave a
/// truncated document behind.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("could not serialize for {}: {e}", path.display()))?;
    text.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text.as_bytes()).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(format!("could not replace {}: {e}", path.display()))
        }
    }
}

/// True when the store has never written anything (first run).
pub fn is_first_run(store: &Store) -> bool {
    !store.settings_path().exists() && store.list_sessions().is_empty()
}

/// `io::Error` to message, used by the demo generator.
pub fn io_message(context: &str, error: io::Error) -> String {
    format!("{context}: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::geom::{NormRect, Point};
    use crate::model::session::{Layer, MediaSource, Role};

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("reaction-player-store-{tag}-{}", new_id("t")))
    }

    fn store(tag: &str) -> (Store, PathBuf) {
        let root = temp_root(tag);
        (
            Store::at(root.join("config"), root.join("data")),
            root,
        )
    }

    fn session(name: &str) -> Session {
        let mut s = Session::new(name);
        let mut a = Layer::new(
            "Source",
            Role::Source,
            MediaSource::file("/tmp/source.mp4"),
            NormRect::FULL,
            0,
        );
        a.duration_us = 10_000_000;
        a.has_audio = true;
        let mut b = Layer::new(
            "Reaction 1",
            Role::Reaction,
            MediaSource::file("/tmp/reaction.mp4"),
            NormRect::new(0.5, 0.0, 0.5, 1.0),
            1,
        );
        b.duration_us = 12_000_000;
        b.offset_us = 1_500_000;
        s.layers.push(a);
        s.layers.push(b);
        s.master_audio = Some(s.layers[0].id.clone());
        s
    }

    #[test]
    fn settings_round_trip_through_disk() {
        let (store, root) = store("settings");
        let mut settings = Settings::default();
        settings.hw_accel = crate::model::settings::HwAccelPref::SoftwareOnly;
        settings.seek_step_ms = 2_000;
        settings.snap_sensitivity_px = 12.0;
        store.save_settings(&settings).unwrap();
        assert!(store.settings_path().exists());
        assert!(!is_first_run(&store));
        let back = store.load_settings();
        assert_eq!(back.hw_accel, settings.hw_accel);
        assert_eq!(back.seek_step_ms, 2_000);
        assert_eq!(back.snap_sensitivity_px, 12.0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_preferences_are_defaults_not_errors() {
        let (store, root) = store("missing");
        assert_eq!(store.load_settings(), Settings::default());
        assert!(store.list_sessions().is_empty());
        assert!(store.load_custom_presets().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn session_round_trip_keeps_every_field() {
        let (store, root) = store("session");
        let mut original = session("My Session");
        store.save_session(&mut original).unwrap();
        let loaded = store.load_session(&original.id).unwrap();
        assert_eq!(loaded, original);
        assert_eq!(loaded.layers[1].offset_us, 1_500_000);
        assert_eq!(loaded.layers[1].z_index, 1);
        assert_eq!(loaded.union().duration_us(), 12_000_000);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn session_list_is_sorted_newest_first() {
        let (store, root) = store("list");
        let mut a = session("older");
        a.updated_at = 1_000;
        store.save_session(&mut a).unwrap();
        // Saving stamps `updated_at`, and the list orders by it, so the second
        // save has to land in a later millisecond to be "newer".
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut b = session("newer");
        store.save_session(&mut b).unwrap();
        let list = store.list_sessions();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "newer");
        assert_eq!(list[0].layer_count, 2);
        assert_eq!(list[1].duration_us, 12_000_000);
        // Neither /tmp/source.mp4 nor /tmp/reaction.mp4 exists here.
        assert_eq!(list[0].missing_media, 2);
        store.delete_session(&b.id).unwrap();
        assert_eq!(store.list_sessions().len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn export_and_import_round_trip() {
        let (store, root) = store("export");
        let original = session("Travelling");
        let export = root.join("Travelling.rplay.json");
        store.export_session(&original, &export).unwrap();
        let back = store.import_session(&export).unwrap();
        assert_eq!(back.name, "Travelling");
        assert_eq!(back.layers.len(), 2);
        assert_eq!(back.master_audio, original.master_audio);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn newer_files_are_refused_with_a_clear_error() {
        let (store, root) = store("version");
        let mut s = session("future");
        s.version = SESSION_VERSION + 1;
        let path = store.sessions_dir().join(format!("{}.json", s.id));
        write_json(&path, &s).unwrap();
        let err = store.load_session(&s.id).unwrap_err();
        assert!(err.contains("newer version"), "{err}");

        let mut file = PresetFile {
            version: PRESET_VERSION + 1,
            presets: Vec::new(),
        };
        write_json(&store.presets_path(), &file).unwrap();
        assert!(store.load_custom_presets().is_empty());
        file.version = PRESET_VERSION;
        write_json(&store.presets_path(), &file).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn custom_presets_are_persisted_by_name() {
        let (store, root) = store("presets");
        let s = session("p");
        let mut b = Preset::from_session("Beta", &s);
        let a = Preset::from_session("Alpha", &s);
        b.created_at = 0;
        store.save_custom_presets(&[b, a]).unwrap();
        let loaded = store.load_custom_presets();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "Alpha");
        assert_eq!(loaded[1].name, "Beta");
        assert_eq!(loaded[0].version, PRESET_VERSION);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn thumbnails_are_written_as_png() {
        let (store, root) = store("thumb");
        let (w, h) = (8u32, 4u32);
        let rgba = vec![255u8; (w * h * 4) as usize];
        let name = store.save_thumbnail("sess-1", w, h, &rgba).unwrap().unwrap();
        assert_eq!(name, "thumbnails/sess-1.png");
        let path = store.thumbnail_path(&name);
        assert!(path.exists());
        let decoded = image::open(&path).unwrap();
        assert_eq!(decoded.width(), 8);
        assert!(store.save_thumbnail("empty", 0, 0, &[]).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clear_all_removes_data_but_keeps_the_root() {
        let (store, root) = store("clear");
        let mut s = session("gone");
        store.save_session(&mut s).unwrap();
        store.save_settings(&Settings::default()).unwrap();
        store.save_custom_presets(&[Preset::from_session("x", &s)]).unwrap();
        assert!(store.usage().0 > 0);
        store.clear_all().unwrap();
        assert!(store.list_sessions().is_empty());
        assert!(store.load_custom_presets().is_empty());
        assert!(!store.settings_path().exists());
        assert!(root.join("config").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn importing_a_file_without_an_id_mints_one() {
        let (store, root) = store("noid");
        let mut s = session("anon");
        s.id = String::new();
        let path = root.join("anon.rplay.json");
        write_json(&path, &s).unwrap();
        let back = store.import_session(&path).unwrap();
        assert!(!back.id.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn session_survives_a_layout_round_trip() {
        let (store, root) = store("layout");
        let mut s = session("layout");
        s.layers[1].rect = NormRect::new(0.1, 0.2, 0.3, 0.4);
        s.layers[1].crop = crate::model::geom::Crop {
            left: 0.05,
            bottom: 0.1,
            ..crate::model::geom::Crop::ZERO
        };
        store.save_session(&mut s).unwrap();
        let back = store.load_session(&s.id).unwrap();
        assert!(back.layers[1].rect.contains(Point { x: 0.2, y: 0.3 }));
        assert!((back.layers[1].crop.uv()[0] - 0.05).abs() < 1e-6);
        let _ = fs::remove_dir_all(root);
    }
}
