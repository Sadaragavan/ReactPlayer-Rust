//! The `eframe` application: one struct holds the session, the engine, the store
//! and every piece of transient UI state; the screens are functions over it.
//!
//! Responsibilities that live here rather than in a screen:
//! * opening and saving sessions, and keeping the engine in step with them,
//! * the keyboard dispatcher, because a shortcut means different things on
//!   different screens,
//! * drag-and-drop from anywhere in the window,
//! * the modal dialogs (new session, confirmations, the cheat sheet).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, RichText};

use crate::engine::demo;
use crate::engine::runtime::Runtime;
use crate::engine::video_frame::{CropPx, VideoFrame};
use crate::gpu::capture;
use crate::gpu::renderer::Compositor;
use crate::model::geom::{Handle, NormRect, Point};
use crate::model::presets::{self, LayoutType, Preset, PresetSlot};
use crate::model::session::{now_ms, CanvasAspect, FitMode, Layer, MediaSource, Role, Session};
use crate::model::settings::Settings;
use crate::model::store::{SessionSummary, Store};
use crate::ui::screens;
use crate::ui::screens::player::to_norm;
use crate::ui::shortcuts::{self, Action};
use crate::ui::theme;
use crate::ui::toast::Toasts;

/// Which screen is up. The alignment studio is its own screen: it has its own
/// transport and its own compare options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Home,
    Sessions,
    Presets,
    Studio,
    Settings,
    Player,
}

/// How the studio compares the two captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareMode {
    Onion,
    Wipe,
}

/// A captured alignment frame: the exact timestamp plus the frames that were on
/// screen around it.
#[derive(Clone)]
pub struct Capture {
    pub pts_us: i64,
    pub frames: Vec<Arc<VideoFrame>>,
}

impl Capture {
    pub fn newest(&self) -> Option<Arc<VideoFrame>> {
        self.frames.last().cloned()
    }
}

/// Extensions the file dialogs offer. FFmpeg demuxes more, but these are what a
/// phone or a camera writes.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "mov", "webm", "avi", "m4v", "ts"];

/// The alignment studio's state.
pub struct Studio {
    pub a: Option<String>,
    pub b: Option<String>,
    pub mode: CompareMode,
    pub onion_mix: f32,
    pub wipe_percent: f32,
    pub capture_a: Option<Capture>,
    pub capture_b: Option<Capture>,
    pub note: Option<String>,
}

impl Default for Studio {
    fn default() -> Self {
        Self {
            a: None,
            b: None,
            mode: CompareMode::Wipe,
            onion_mix: 0.5,
            wipe_percent: 0.5,
            capture_a: None,
            capture_b: None,
            note: None,
        }
    }
}

impl Studio {
    pub fn delta_us(&self) -> Option<i64> {
        match (&self.capture_a, &self.capture_b) {
            (Some(a), Some(b)) => Some(b.pts_us - a.pts_us),
            _ => None,
        }
    }

    pub fn reset(&mut self) {
        self.capture_a = None;
        self.capture_b = None;
        self.note = None;
    }
}

/// The New Session dialog's fields.
pub struct NewSessionForm {
    pub open: bool,
    pub name: String,
    pub layout: LayoutType,
    pub aspect: CanvasAspect,
    pub files: Vec<PathBuf>,
    pub error: Option<String>,
}

impl Default for NewSessionForm {
    fn default() -> Self {
        Self {
            open: false,
            name: String::new(),
            layout: LayoutType::SideBySide,
            aspect: CanvasAspect::Ratio16x9,
            files: Vec::new(),
            error: None,
        }
    }
}

/// Confirmations, all destructive actions go through one of these.
#[derive(Default)]
pub struct Confirm {
    pub clear_data: bool,
    pub delete_session: Option<String>,
    pub import_overwrite: Option<PathBuf>,
}

/// A drag in progress on the canvas.
pub struct DragState {
    pub layer: String,
    pub handle: Option<Handle>,
    pub start_rect: NormRect,
    /// Where the drag started, in canvas-relative units.
    pub start: Point,
    pub moved: bool,
}

/// Transient canvas state: the drag, the snap guides and the scrub gesture.
#[derive(Default)]
pub struct CanvasState {
    pub drag: Option<DragState>,
    pub guides_x: Vec<f32>,
    pub guides_y: Vec<f32>,
    pub scrubbing: bool,
    pub context_layer: Option<String>,
    pub lock_layout: bool,
    /// Where the canvas was drawn last frame, in screen points. A dropped file
    /// uses it to land under the cursor instead of in the middle.
    pub last_rect: Option<egui::Rect>,
}

/// The application.
pub struct App {
    pub store: Store,
    pub settings: Settings,
    /// Built-in presets followed by the user's own.
    pub presets: Vec<Preset>,
    pub screen: Screen,
    pub session: Option<Session>,
    pub runtime: Option<Runtime>,
    pub toasts: Toasts,
    pub sessions: Vec<SessionSummary>,
    pub selection: Option<String>,
    pub layers_panel: bool,
    pub theater: bool,
    pub canvas: CanvasState,
    pub studio: Studio,
    pub new_session: NewSessionForm,
    pub confirm: Confirm,
    pub sheet: bool,
    /// Name being edited in the sessions list.
    pub renaming: Option<(String, String)>,
    /// The buffer behind "save this arrangement as a preset".
    pub preset_name: String,
    pub wallpaper: Option<egui::TextureHandle>,
    /// Remembers that a probe result was written, so the session is only
    /// touched once per source.
    pub probed: Vec<String>,
    pub quit_saved: bool,
    /// Whether `ffmpeg` was found on `PATH` when the app started. Only the demo
    /// needs it, and the home screen says so before it is clicked.
    pub ffmpeg_ready: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            Compositor::install(render_state);
        }
        let store = match Store::open() {
            Ok(store) => store,
            Err(message) => {
                // Without a config directory the app still runs; it just cannot
                // remember anything, and says so once.
                eprintln!("reaction-player: {message}");
                Store::at(std::env::temp_dir().join("reaction-player/config"),
                          std::env::temp_dir().join("reaction-player/data"))
            }
        };
        let settings = store.load_settings();
        let mut presets = presets::builtin_presets();
        presets.extend(store.load_custom_presets());
        let mut app = Self {
            sessions: store.list_sessions(),
            store,
            settings,
            presets,
            screen: Screen::Home,
            session: None,
            runtime: None,
            toasts: Toasts::new(),
            selection: None,
            layers_panel: true,
            theater: false,
            canvas: CanvasState::default(),
            studio: Studio::default(),
            new_session: NewSessionForm::default(),
            confirm: Confirm::default(),
            sheet: false,
            renaming: None,
            preset_name: String::new(),
            wallpaper: None,
            probed: Vec::new(),
            quit_saved: false,
            ffmpeg_ready: crate::engine::demo::ffmpeg_on_path(),
        };
        app.reload_wallpaper(&cc.egui_ctx);
        // Continue where the last run left off, on the home screen.
        if let Some(id) = app.settings.last_session.clone()
            && let Ok(session) = app.store.load_session(&id) {
            app.open_session(session, &cc.egui_ctx);
        }
        app
    }

    // ---- navigation ----------------------------------------------------

    pub fn go(&mut self, screen: Screen) {
        if screen != Screen::Player && self.screen == Screen::Player {
            self.save_current();
            if let Some(runtime) = self.runtime.as_mut()
                && let Some(session) = self.session.as_ref() {
                runtime.stop(session);
            }
        }
        self.screen = screen;
        // Both screens print facts about what is on disk ("continue where you
        // left off", thumbnails), so they re-read the directory on entry.
        if matches!(screen, Screen::Sessions | Screen::Home) {
            self.refresh_sessions();
        }
    }

    pub fn refresh_sessions(&mut self) {
        self.sessions = self.store.list_sessions();
    }

    /// Show the new session dialog with a set of files already chosen.
    pub fn new_session_with_files(&mut self, files: Vec<PathBuf>) {
        self.new_session = NewSessionForm {
            open: true,
            name: default_session_name(),
            layout: self.settings.default_layout,
            aspect: self.settings.default_aspect,
            files,
            error: None,
        };
    }

    /// Build and open a session from the New Session dialog's fields.
    pub fn start_session(&mut self, ctx: &egui::Context) {
        let form = &mut self.new_session;
        if form.files.is_empty() {
            form.error = Some("Add at least one video file.".to_owned());
            return;
        }
        let name = if form.name.trim().is_empty() {
            default_session_name()
        } else {
            form.name.trim().to_owned()
        };
        let mut session = Session::new(name);
        session.canvas_aspect = form.aspect;
        let rects = presets::layout_rects(form.layout, form.files.len());
        let z = presets::layout_z(form.layout, form.files.len());
        for (index, file) in form.files.iter().enumerate() {
            let role = if index == 0 { Role::Source } else { Role::Reaction };
            let mut layer = Layer::new(
                role.default_name(index),
                role,
                MediaSource::file(file.clone()),
                rects.get(index).copied().unwrap_or(NormRect::FULL),
                z.get(index).copied().unwrap_or(index as i32),
            );
            layer.fit = FitMode::Contain;
            session.layers.push(layer);
        }
        session.normalize_z();
        session.master_audio = session.layers.first().map(|layer| layer.id.clone());
        self.new_session.open = false;
        self.new_session.files.clear();
        self.open_session(session, ctx);
        self.go(Screen::Player);
    }

    /// Make `session` the open one and bring the engine up for it.
    pub fn open_session(&mut self, mut session: Session, _ctx: &egui::Context) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Some(previous) = self.session.as_ref() {
            runtime.stop(previous);
        }
        session.normalize_z();
        if session.master_audio.is_none() {
            session.master_audio = session.default_master().map(|layer| layer.id.clone());
        }
        self.selection = session.paint_order().last().map(|index| session.layers[*index].id.clone());
        self.canvas = CanvasState::default();
        self.studio = Studio::default();
        self.probed.clear();
        self.runtime = Some(Runtime::new(&self.settings));
        self.session = Some(session.clone());
        // Probe immediately so durations and frame rates are real before the
        // first paint.
        if let (Some(session), Some(runtime)) = (self.session.as_mut(), self.runtime.as_mut()) {
            let probed = runtime.sync_session(session, &self.settings);
            let _ = probed;
        }
        self.settings.last_session = Some(session.id.clone());
        self.save_settings();
        self.theater = false;
        self.layers_panel = true;
    }

    pub fn close_session(&mut self) {
        self.save_current();
        if let (Some(runtime), Some(session)) = (self.runtime.as_mut(), self.session.as_ref()) {
            runtime.stop(session);
        }
        self.runtime = None;
        self.session = None;
        self.selection = None;
        self.screen = Screen::Home;
        self.settings.last_session = None;
        self.save_settings();
    }

    /// Save the open session, its thumbnail and the preferences.
    pub fn save_current(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let mut session = session.clone();
        session.touch();
        if let Some(thumb) = self.thumbnail_rgba() {
            match self
                .store
                .save_thumbnail(&session.id, thumb.width, thumb.height, &thumb.data)
            {
                Ok(path) => session.thumbnail = path,
                Err(message) => self.toasts.error(format!("Could not write the thumbnail: {message}")),
            }
        }
        match self.store.save_session(&mut session) {
            Ok(()) => {
                if let Some(open) = self.session.as_mut() {
                    open.updated_at = session.updated_at;
                    open.thumbnail = session.thumbnail.clone();
                }
                self.refresh_sessions();
            }
            Err(message) => self.toasts.error(format!("Could not save: {message}")),
        }
        self.save_settings();
    }

    pub fn save_settings(&mut self) {
        if let Some(session) = self.session.as_ref() {
            self.settings.last_session = Some(session.id.clone());
        }
        if let Err(message) = self.store.save_settings(&self.settings) {
            self.toasts.error(format!("Could not save preferences: {message}"));
        }
    }

    /// The top layer's current picture, downscaled, for the session card.
    fn thumbnail_rgba(&self) -> Option<crate::engine::video_frame::RgbaBuffer> {
        let session = self.session.as_ref()?;
        let runtime = self.runtime.as_ref()?;
        let top = session
            .paint_order()
            .last()
            .map(|index| &session.layers[*index])?;
        let frame = runtime.frame(&top.id)?;
        let rgba = frame.to_rgba_cropped(None);
        Some(capture::scaled_rgba(&rgba, 480))
    }

    // ---- transport -----------------------------------------------------

    pub fn toggle_play(&mut self) {
        let (Some(session), Some(runtime)) = (self.session.as_ref(), self.runtime.as_mut()) else {
            return;
        };
        runtime.toggle(session, &self.settings);
    }

    pub fn seek_display(&mut self, display_us: i64) {
        let (Some(session), Some(runtime)) = (self.session.as_ref(), self.runtime.as_mut()) else {
            return;
        };
        runtime.seek_display(session, display_us);
    }

    pub fn step_frames(&mut self, steps: i64) {
        let (Some(session), Some(runtime)) = (self.session.as_ref(), self.runtime.as_mut()) else {
            return;
        };
        runtime.step(session, steps);
    }

    pub fn selected_layer(&self) -> Option<&Layer> {
        let session = self.session.as_ref()?;
        let id = self.selection.as_deref()?;
        session.layer(id)
    }

    pub fn select(&mut self, id: &str) {
        self.selection = Some(id.to_owned());
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.set_focused(Some(id.to_owned()));
        }
    }

    /// Copy the selected layer's cropped frame to the clipboard.
    pub fn copy_frame(&mut self) {
        let Some(layer) = self.selected_layer() else {
            self.toasts.error("Select a layer first.");
            return;
        };
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let Some(frame) = runtime.frame(&layer.id) else {
            self.toasts.error("Nothing decoded yet for this layer.");
            return;
        };
        let crop = layer.crop;
        let (cx, cy, cw, ch) = crop.to_px(frame.width, frame.height);
        let cropped = CropPx {
            x: cx,
            y: cy,
            w: cw,
            h: ch,
        };
        match capture::copy_frame_to_clipboard(&frame, Some(cropped.clamped(frame.width, frame.height))) {
            Ok((width, height)) => self
                .toasts
                .success(format!("Copied a {width}x{height} frame to the clipboard.")),
            Err(message) => self.toasts.error(message),
        }
    }

    /// Add dropped files to the open session as new layers.
    pub fn add_layers(&mut self, files: Vec<PathBuf>, drop: Option<Point>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let reactions = session
            .layers
            .iter()
            .filter(|layer| layer.role != Role::Source)
            .count();
        for (offset, file) in files.iter().enumerate() {
            let index = reactions + offset;
            let role = Role::Reaction;
            let mut rect = NormRect::new(0.62, 0.62, 0.34, 0.34);
            if let Some(point) = drop {
                // Drop where the cursor is, keeping the inset on the canvas.
                rect.x = (point.x - rect.w / 2.0).clamp(0.0, 1.0 - rect.w);
                rect.y = (point.y - rect.h / 2.0).clamp(0.0, 1.0 - rect.h);
            }
            let z = index as i32 + 1;
            let mut layer = Layer::new(
                Role::Reaction.default_name(index + 1),
                role,
                MediaSource::file(file.clone()),
                rect,
                z,
            );
            layer.fit = FitMode::Contain;
            session.layers.push(layer);
        }
        session.normalize_z();
        let added = files.len();
        self.sync_engine();
        let noun = if added == 1 { "layer" } else { "layers" };
        self.toasts.success(format!("Added {added} {noun}."));
    }

    /// Ask for video files and add them to the open session as reactions.
    pub fn add_layers_dialog(&mut self) {
        if self.session.is_none() {
            return;
        }
        let Some(files) = rfd::FileDialog::new()
            .add_filter("Video", VIDEO_EXTENSIONS)
            .pick_files()
        else {
            return;
        };
        if files.is_empty() {
            return;
        }
        self.add_layers(files, None);
    }

    /// Open the demo: generate the clips if needed, then the session.
    pub fn open_demo(&mut self, ctx: &egui::Context) {
        let dir = self.store.data_dir().to_path_buf();
        match demo::ensure(&dir) {
            Ok(clips) => {
                let session = demo::build_session(&clips);
                self.open_session(session, ctx);
                self.go(Screen::Player);
                self.toasts
                    .info("Demo ready: the reaction flashes 1500 ms after the source.");
            }
            Err(message) => self.toasts.error(message),
        }
    }

    /// Re-run `sync_session`, e.g. after layers were added or removed.
    pub fn sync_engine(&mut self) {
        if let (Some(session), Some(runtime)) = (self.session.as_mut(), self.runtime.as_mut()) {
            runtime.sync_session(session, &self.settings);
        }
    }

    /// Point the selected layer at a file the user picks, keeping its offset,
    /// crop and rectangle. The decoder is thrown away and the file re-probed, so
    /// a session whose media moved is repairable without rebuilding it.
    pub fn relink_layer(&mut self, _ctx: &egui::Context) {
        let Some(id) = self.selection.clone() else {
            self.toasts.error("Select the layer to relink first.");
            return;
        };
        let start_dir = self
            .session
            .as_ref()
            .and_then(|session| session.layer(&id))
            .and_then(|layer| layer.source.path())
            .and_then(|path| path.parent())
            .map(std::path::Path::to_path_buf);
        let mut dialog = rfd::FileDialog::new().add_filter("Video", VIDEO_EXTENSIONS);
        if let Some(dir) = start_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.pick_file() else {
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let relinked = self
            .session
            .as_mut()
            .map(|session| session.relink(&id, MediaSource::file(path)))
            .unwrap_or(false);
        if !relinked {
            return;
        }
        // Forget what was probed, stop the old worker, and let the tick that
        // follows open the new file.
        self.probed.retain(|seen| seen != &id);
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.forget_layer(&id);
        }
        self.sync_engine();
        if let Some(session) = self.session.as_mut() {
            session.touch();
        }
        self.save_current();
        self.toasts.success(format!("Relinked to {name}."));
    }

    // ---- presets -------------------------------------------------------

    pub fn reload_presets(&mut self) {
        let mut presets = presets::builtin_presets();
        presets.extend(self.store.load_custom_presets());
        self.presets = presets;
    }

    pub fn save_preset(&mut self, name: &str) {
        let Some(session) = self.session.as_ref() else {
            self.toasts.error("Open a session before saving a layout.");
            return;
        };
        let preset = Preset::from_session(name, session);
        let mut custom: Vec<Preset> = self
            .store
            .load_custom_presets()
            .into_iter()
            .filter(|existing| existing.name != preset.name)
            .collect();
        custom.push(preset);
        match self.store.save_custom_presets(&custom) {
            Ok(()) => {
                self.reload_presets();
                self.toasts.success(format!("Saved the layout as \"{name}\"."));
            }
            Err(message) => self.toasts.error(message),
        }
    }

    pub fn delete_preset(&mut self, id: &str) {
        let custom: Vec<Preset> = self
            .store
            .load_custom_presets()
            .into_iter()
            .filter(|preset| preset.id != id)
            .collect();
        match self.store.save_custom_presets(&custom) {
            Ok(()) => {
                self.reload_presets();
                self.toasts.info("Preset deleted.");
            }
            Err(message) => self.toasts.error(message),
        }
    }

    pub fn apply_preset(&mut self, id: &str) {
        let Some(preset) = self.presets.iter().find(|preset| preset.id == id).cloned() else {
            return;
        };
        let Some(session) = self.session.as_mut() else {
            self.toasts.error("Open a session first.");
            return;
        };
        presets::apply_preset(session, &preset);
        self.save_current();
        self.toasts
            .success(format!("Applied \"{}\".", preset.name));
    }

    // ---- alignment studio ----------------------------------------------

    /// Open the studio with two sensible layers: the source is the reference.
    pub fn open_studio(&mut self, ctx: &egui::Context) {
        let Some(layers) = self.session.as_ref().map(|session| session.layers.len()) else {
            self.toasts.error("Open a session to align.");
            return;
        };
        if layers < 2 {
            self.toasts
                .error("The studio compares two layers; add another video first.");
            return;
        }
        let _ = ctx;
        let session = self.session.as_ref().expect("checked above");
        let a = session
            .layers
            .iter()
            .find(|layer| layer.role == Role::Source)
            .map(|layer| layer.id.clone())
            .or_else(|| session.layers.first().map(|layer| layer.id.clone()));
        let b = session
            .layers
            .iter()
            .find(|layer| Some(&layer.id) != a.as_ref())
            .map(|layer| layer.id.clone());
        self.studio = Studio {
            a,
            b,
            ..Studio::default()
        };
        let ids: Vec<String> = [self.studio.a.clone(), self.studio.b.clone()]
            .into_iter()
            .flatten()
            .collect();
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.set_focused(self.studio.a.clone());
            if let Some(session) = self.session.as_ref() {
                for id in ids {
                    let position = runtime.presented_us(&id);
                    runtime.seek_unlinked(session, &id, position);
                }
            }
        }
        self.go(Screen::Studio);
    }

    /// `C` in the studio: remember where both sides are, with their frames.
    pub fn studio_capture(&mut self) {
        let (Some(a), Some(b)) = (self.studio.a.clone(), self.studio.b.clone()) else {
            self.toasts.error("Choose both sides before capturing.");
            return;
        };
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let capture_a = Capture {
            pts_us: runtime.presented_us(&a),
            frames: runtime.history(&a),
        };
        let capture_b = Capture {
            pts_us: runtime.presented_us(&b),
            frames: runtime.history(&b),
        };
        if capture_a.frames.is_empty() || capture_b.frames.is_empty() {
            self.toasts.error("Both sides need a decoded frame first.");
            return;
        }
        self.studio.capture_a = Some(capture_a);
        self.studio.capture_b = Some(capture_b);
        if let Some(delta) = self.studio.delta_us() {
            self.studio.note = Some(format!(
                "B lands {} the flash by {} ms.",
                if delta >= 0 { "after" } else { "before" },
                delta.abs() / 1_000
            ));
            self.toasts
                .info(format!("Captured: offset_B = {:.3} s.", delta as f64 / 1e6));
        }
    }

    /// Step one side of the studio comparison by whole frames.
    pub fn studio_step(&mut self, layer_id: &str, steps: i64) {
        let (Some(session), Some(runtime)) = (self.session.as_ref(), self.runtime.as_mut()) else {
            return;
        };
        let frame_us = session
            .layer(layer_id)
            .map(|layer| layer.frame_us())
            .unwrap_or(33_333);
        let current = runtime.presented_us(layer_id);
        let target = current + steps.saturating_mul(frame_us);
        runtime.seek_unlinked(session, layer_id, target.max(0));
    }

    /// Write the measured delta into the session as B's offset.
    pub fn studio_apply(&mut self) {
        let (Some(delta), Some(b), Some(a)) = (
            self.studio.delta_us(),
            self.studio.b.clone(),
            self.studio.a.clone(),
        ) else {
            return;
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let a_offset = session.layer(&a).map(|layer| layer.offset_us).unwrap_or(0);
        if let Some(layer) = session.layer_mut(&b) {
            layer.offset_us = a_offset + delta;
        }
        session.touch();
        if let (Some(runtime), Some(session)) = (self.runtime.as_mut(), self.session.as_ref()) {
            runtime.apply_offsets(session);
        }
        self.studio.note = Some(format!("Applied {:.3} s to the other layer.", delta as f64 / 1e6));
        self.toasts.success("Alignment applied.");
    }

    // ---- keyboard ------------------------------------------------------

    pub fn apply_action(&mut self, action: Action, ctx: &egui::Context) {
        // Dialogs and the cheat sheet take Escape first.
        if action == Action::Escape {
            if self.new_session.open {
                self.new_session.open = false;
                return;
            }
            if self.confirm.clear_data
                || self.confirm.delete_session.is_some()
                || self.confirm.import_overwrite.is_some()
            {
                self.confirm = Confirm::default();
                return;
            }
            if self.sheet {
                self.sheet = false;
                return;
            }
            if self.theater {
                self.theater = false;
                return;
            }
            if self.screen == Screen::Player || self.screen == Screen::Studio {
                self.go(Screen::Home);
                return;
            }
            return;
        }
        if action == Action::Help {
            self.sheet = !self.sheet;
            return;
        }
        // Everything else only makes sense in the player or the studio.
        if !matches!(self.screen, Screen::Player | Screen::Studio) {
            return;
        }
        let in_studio = self.screen == Screen::Studio;
        match action {
            Action::TogglePlay => {
                if in_studio {
                    // The studio runs both sides from the same key.
                    let playing = self.runtime.as_ref().map(|r| r.is_playing()).unwrap_or(false);
                    if playing {
                        if let (Some(session), Some(runtime)) =
                            (self.session.as_ref(), self.runtime.as_mut())
                        {
                            runtime.pause(session);
                        }
                    } else if let (Some(session), Some(runtime)) =
                        (self.session.as_ref(), self.runtime.as_mut())
                    {
                        runtime.play(session, &self.settings);
                    }
                } else {
                    self.toggle_play();
                }
            }
            Action::ToggleLink => {
                if let Some(session) = self.session.as_mut() {
                    session.is_linked = !session.is_linked;
                    let linked = session.is_linked;
                    session.touch();
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.set_focused(self.selection.clone());
                    }
                    self.toasts.info(if linked {
                        "Timelines linked."
                    } else {
                        "Timelines unlinked: each track keeps its own position."
                    });
                }
            }
            Action::ToggleTheater => self.theater = !self.theater,
            Action::ToggleFullscreen => {
                let fullscreen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }
            Action::LayerUp | Action::LayerDown | Action::LayerFront | Action::LayerBack => {
                let Some(id) = self.selection.clone() else {
                    return;
                };
                let Some(session) = self.session.as_mut() else {
                    return;
                };
                match action {
                    Action::LayerUp => session.bring_forward(&id),
                    Action::LayerDown => session.send_backward(&id),
                    Action::LayerFront => session.bring_to_front(&id),
                    Action::LayerBack => session.send_to_back(&id),
                    _ => {}
                }
                session.touch();
            }
            Action::StepBack => self.step_side(in_studio, -1),
            Action::StepForward => self.step_side(in_studio, 1),
            Action::Back | Action::Forward | Action::BackBig | Action::ForwardBig => {
                if in_studio {
                    let step = match action {
                        Action::Back | Action::Forward => 0.01,
                        _ => 0.10,
                    };
                    let sign = match action {
                        Action::Back | Action::BackBig => -1.0,
                        _ => 1.0,
                    };
                    self.studio.wipe_percent =
                        (self.studio.wipe_percent + sign * step).clamp(0.0, 1.0);
                } else {
                    let playing = self.runtime.as_ref().map(|r| r.is_playing()).unwrap_or(false);
                    let step_us = self.settings.seek_step_us();
                    let union = self.session.as_ref().map(|s| s.union()).unwrap_or_default();
                    let sign: i64 = match action {
                        Action::Back | Action::BackBig => -1,
                        _ => 1,
                    };
                    match (action, playing) {
                        (Action::Back | Action::Forward, false) => self.step_frames(sign),
                        (Action::Back | Action::Forward, true) => {
                            let position = self
                                .runtime
                                .as_ref()
                                .map(|r| r.position_us())
                                .unwrap_or(0);
                            self.seek_display(position - union.start_us + sign * step_us);
                        }
                        (Action::BackBig | Action::ForwardBig, false) => {
                            let position = self
                                .runtime
                                .as_ref()
                                .map(|r| r.position_us())
                                .unwrap_or(0);
                            self.seek_display(position - union.start_us + sign * step_us);
                        }
                        _ => {
                            let position = self
                                .runtime
                                .as_ref()
                                .map(|r| r.position_us())
                                .unwrap_or(0);
                            let tenth = (union.duration_us() / 10).max(1);
                            self.seek_display(position - union.start_us + sign * tenth);
                        }
                    }
                }
            }
            Action::Capture => {
                if in_studio {
                    self.studio_capture();
                } else {
                    self.copy_frame();
                }
            }
            Action::MuteMaster => {
                if let Some(session) = self.session.as_mut() {
                    let master = session
                        .master()
                        .map(|layer| layer.id.clone())
                        .or_else(|| session.layers.first().map(|layer| layer.id.clone()));
                    if let Some(id) = master
                        && let Some(layer) = session.layer_mut(&id) {
                        layer.muted = !layer.muted;
                        let muted = layer.muted;
                        self.toasts.info(if muted {
                            "Master audio muted."
                        } else {
                            "Master audio on."
                        });
                    }
                }
            }
            Action::ToggleLayers => self.layers_panel = !self.layers_panel,
            Action::Escape | Action::Help => {}
        }
    }

    fn step_side(&mut self, in_studio: bool, steps: i64) {
        if !in_studio {
            self.step_frames(steps);
            return;
        }
        // In the studio only the B side moves: A is the reference.
        let Some(b) = self.studio.b.clone() else {
            return;
        };
        let (Some(session), Some(runtime)) = (self.session.as_ref(), self.runtime.as_mut()) else {
            return;
        };
        runtime.step_unlinked(session, &b, steps);
    }

    // ---- per-frame work ------------------------------------------------

    /// Keep the engine in step with the session and pull probe results back.
    fn pump_engine(&mut self) {
        let settings = self.settings.clone();
        let (Some(session), Some(runtime)) = (self.session.as_mut(), self.runtime.as_mut()) else {
            return;
        };
        let probed = runtime.sync_session(session, &settings);
        for (id, info) in probed {
            if self.probed.iter().any(|seen| seen == &id) {
                continue;
            }
            if let Some(layer) = session.layer_mut(&id) {
                layer.duration_us = info.duration_us;
                layer.width = info.width;
                layer.height = info.height;
                layer.fps_num = info.fps_num;
                layer.fps_den = info.fps_den;
                layer.has_audio = info.has_audio;
                layer.audio_channels = info.audio_channels;
                layer.audio_rate = info.audio_rate;
                if layer.offset_us != 0 && info.duration_us > 0 {
                    // Keep an offset that already exists, but make sure a fresh
                    // layer lands at the start of its footage.
                }
                session.touch();
                self.probed.push(id.clone());
                if let Some(layer) = session.layer(&id)
                    && layer.role == Role::Source {
                        let master = session.master_audio.clone();
                        if master.is_none() {
                            session.master_audio = Some(layer.id.clone());
                        }
                    }
            }
        }
        let _tick = runtime.tick(session, &settings);
        runtime.set_focused(self.selection.clone());
    }

    fn handle_dropped(&mut self, ctx: &egui::Context) {
        let (dropped, hovering) = ctx.input(|input| {
            let dropped: Vec<PathBuf> = input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect();
            (dropped, !input.raw.hovered_files.is_empty())
        });
        if hovering {
            ctx.request_repaint();
        }
        if dropped.is_empty() {
            return;
        }
        let videos: Vec<PathBuf> = dropped
            .into_iter()
            .filter(|path| {
                path.extension()
                    .map(|ext| {
                        let ext = ext.to_string_lossy().to_lowercase();
                        matches!(
                            ext.as_str(),
                            "mp4" | "mkv" | "mov" | "webm" | "avi" | "m4v" | "ts" | "m2ts"
                        )
                    })
                    .unwrap_or(false)
            })
            .collect();
        if videos.is_empty() {
            self.toasts.error("That was not a video file.");
            return;
        }
        if self.session.is_some() && matches!(self.screen, Screen::Player | Screen::Studio) {
            // Land under the cursor when the drop was over the canvas; a drop
            // over the side panels has no canvas position, so the layer gets the
            // default inset instead.
            let point = ctx
                .input(|input| input.pointer.latest_pos())
                .and_then(|pos| self.canvas.last_rect.map(|rect| to_norm(rect, pos)))
                .filter(|point| (0.0..=1.0).contains(&point.x) && (0.0..=1.0).contains(&point.y));
            self.add_layers(videos, point);
        } else {
            self.new_session_with_files(videos);
        }
    }

    /// Reload the wallpaper texture from settings.
    pub fn reload_wallpaper(&mut self, ctx: &egui::Context) {
        self.wallpaper = None;
        let Some(path) = self.settings.wallpaper.clone() else {
            return;
        };
        match image::open(&path) {
            Ok(image) => {
                let rgba = image.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                self.wallpaper = Some(ctx.load_texture(
                    "wallpaper",
                    color,
                    egui::TextureOptions::LINEAR,
                ));
            }
            Err(error) => {
                self.toasts
                    .error(format!("Could not read the wallpaper: {error}"));
                self.settings.wallpaper = None;
            }
        }
    }

    /// Ask for a wallpaper image.
    pub fn pick_wallpaper(&mut self, ctx: &egui::Context) {
        let picked = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
            .pick_file();
        if let Some(path) = picked {
            self.settings.wallpaper = Some(path);
            self.reload_wallpaper(ctx);
            self.save_settings();
            self.toasts.success("Wallpaper set.");
        }
    }

    /// Forget every session, preset, preference and generated clip.
    pub fn clear_data(&mut self) {
        match self.store.clear_all() {
            Ok(()) => {
                self.session = None;
                self.runtime = None;
                self.settings = Settings::default();
                self.wallpaper = None;
                self.reload_presets();
                self.refresh_sessions();
                self.toasts.success("Local data cleared.");
            }
            Err(message) => self.toasts.error(message),
        }
    }

    /// Export the open session as a portable `.rplay.json`.
    pub fn export_session(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let suggested = format!("{}.rplay.json", slug(&session.name));
        let path = rfd::FileDialog::new()
            .set_file_name(suggested)
            .add_filter("Reaction Player session", &["json"])
            .save_file();
        let Some(path) = path else {
            return;
        };
        match self.store.export_session(session, &path) {
            Ok(()) => self.toasts.success("Session exported."),
            Err(message) => self.toasts.error(message),
        }
    }

    /// Import a `.rplay.json`, opening it.
    pub fn import_session(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Reaction Player session", &["json"])
            .pick_file()
        else {
            return;
        };
        match self.store.import_session(&path) {
            Ok(session) => {
                self.open_session(session, ctx);
                self.go(Screen::Player);
                self.toasts.success("Session imported.");
            }
            Err(message) => self.toasts.error(message),
        }
    }

    // ---- dialogs -------------------------------------------------------

    fn show_dialogs(&mut self, ctx: &egui::Context) {
        self.show_new_session(ctx);
        self.show_confirm(ctx);
        self.show_sheet(ctx);
    }

    fn show_new_session(&mut self, ctx: &egui::Context) {
        if !self.new_session.open {
            return;
        }
        let mut open = self.new_session.open;
        let mut start = false;
        let mut cancel = false;
        let mut pick = false;
        egui::Window::new("New session")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(theme::float_frame())
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.label(RichText::new("Name").color(theme::MUTED).size(12.0));
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_session.name)
                        .hint_text("Session name")
                        .margin(egui::Vec2::new(8.0, 6.0)),
                );
                ui.add_space(10.0);
                ui.label(RichText::new("Layout").color(theme::MUTED).size(12.0));
                for layout in LayoutType::CHOOSABLE {
                    let on = self.new_session.layout == layout;
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(layout.label())
                                    .color(if on { theme::TEXT } else { theme::MUTED })
                                    .size(12.0),
                            )
                            .fill(if on { theme::FILL_ACTIVE } else { theme::BG })
                            .stroke(if on {
                                egui::Stroke::new(1.0, theme::ACCENT)
                            } else {
                                theme::hairline(theme::LINE)
                            })
                            .corner_radius(theme::RADIUS),
                        )
                        .on_hover_text(layout.description())
                        .clicked()
                    {
                        self.new_session.layout = layout;
                    }
                }
                ui.add_space(10.0);
                ui.label(RichText::new("Canvas").color(theme::MUTED).size(12.0));
                ui.horizontal_wrapped(|ui| {
                    for aspect in CanvasAspect::ALL {
                        let on = self.new_session.aspect == aspect;
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(aspect.label())
                                        .color(if on { theme::TEXT } else { theme::MUTED })
                                        .size(12.0),
                                )
                                .fill(if on { theme::FILL_ACTIVE } else { theme::BG })
                                .stroke(if on {
                                    egui::Stroke::new(1.0, theme::ACCENT)
                                } else {
                                    theme::hairline(theme::LINE)
                                })
                                .corner_radius(theme::RADIUS),
                            )
                            .clicked()
                        {
                            self.new_session.aspect = aspect;
                        }
                    }
                });
                ui.add_space(10.0);
                ui.label(RichText::new("Media").color(theme::MUTED).size(12.0));
                if self.new_session.files.is_empty() {
                    ui.label(
                        RichText::new("Drop video files here, or add them below.")
                            .color(theme::MUTED)
                            .size(12.0),
                    );
                }
                let mut remove = None;
                for (index, file) in self.new_session.files.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let role = if index == 0 { "Source" } else { "Reaction" };
                        ui.label(
                            RichText::new(format!("{role}: {}", file.display()))
                                .color(theme::TEXT)
                                .size(12.0),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                    });
                }
                if let Some(index) = remove {
                    self.new_session.files.remove(index);
                }
                if let Some(error) = &self.new_session.error {
                    ui.label(RichText::new(error).color(theme::DANGER).size(12.0));
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Add files…").clicked() {
                        pick = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Start").color(theme::BG).size(13.0),
                                )
                                .fill(theme::ACCENT)
                                .stroke(egui::Stroke::new(1.0, theme::ACCENT))
                                .corner_radius(theme::RADIUS)
                                .min_size(egui::Vec2::new(92.0, 30.0)),
                            )
                            .clicked()
                        {
                            start = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            });
        self.new_session.open = open && !cancel && !start;
        if pick
            && let Some(files) = rfd::FileDialog::new()
                .add_filter(
                    "Video",
                    crate::app::VIDEO_EXTENSIONS,
                )
                .pick_files()
        {
            self.new_session.files.extend(files);
            self.new_session.error = None;
        }
        if start {
            self.start_session(ctx);
        }
    }

    fn show_confirm(&mut self, ctx: &egui::Context) {
        if self.confirm.clear_data {
            let mut open = true;
            egui::Window::new("Clear local data?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .frame(theme::float_frame())
                .show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.label(
                        RichText::new(
                            "Every session, preset, preference and generated demo clip will be \
                             deleted. Your video files are not touched.",
                        )
                        .color(theme::TEXT)
                        .size(12.5),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Delete everything")
                                        .color(theme::DANGER)
                                        .size(13.0),
                                )
                                .fill(theme::BG)
                                .stroke(egui::Stroke::new(1.0, theme::DANGER))
                                .corner_radius(theme::RADIUS),
                            )
                            .clicked()
                        {
                            self.confirm.clear_data = false;
                            self.clear_data();
                            open = false;
                        }
                        if ui.button("Keep my data").clicked() {
                            self.confirm.clear_data = false;
                            open = false;
                        }
                    });
                });
            let _ = open;
        }
        if let Some(id) = self.confirm.delete_session.clone() {
            let name = self
                .sessions
                .iter()
                .find(|summary| summary.id == id)
                .map(|summary| summary.name.clone())
                .unwrap_or_else(|| "this session".to_owned());
            egui::Window::new("Delete session?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .frame(theme::float_frame())
                .show(ctx, |ui| {
                    ui.set_width(400.0);
                    ui.label(
                        RichText::new(format!("\"{name}\" will be removed. This cannot be undone."))
                            .color(theme::TEXT)
                            .size(12.5),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("Delete").color(theme::DANGER).size(13.0),
                                )
                                .fill(theme::BG)
                                .stroke(egui::Stroke::new(1.0, theme::DANGER))
                                .corner_radius(theme::RADIUS),
                            )
                            .clicked()
                        {
                            if let Err(message) = self.store.delete_session(&id) {
                                self.toasts.error(message);
                            } else {
                                if self.session.as_ref().map(|s| s.id.as_str()) == Some(id.as_str()) {
                                    self.session = None;
                                    self.runtime = None;
                                }
                                self.refresh_sessions();
                                self.toasts.info("Session deleted.");
                            }
                            self.confirm.delete_session = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm.delete_session = None;
                        }
                    });
                });
        }
    }

    fn show_sheet(&mut self, ctx: &egui::Context) {
        if !self.sheet {
            return;
        }
        let mut open = true;
        egui::Window::new("Keyboard")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(theme::float_frame())
            .show(ctx, |ui| {
                ui.set_width(460.0);
                for action in shortcuts::ALL {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [110.0, 18.0],
                            egui::Label::new(
                                RichText::new(action.keys()).color(theme::ACCENT).size(12.0),
                            ),
                        );
                        ui.label(
                            RichText::new(action.description())
                                .color(theme::MUTED)
                                .size(12.0),
                        );
                    });
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Shortcuts pause while a text field has focus.")
                        .color(theme::MUTED)
                        .size(11.0),
                );
            });
        self.sheet = open;
    }

    fn show_drop_overlay(&self, ctx: &egui::Context) {
        let hovering = ctx.input(|input| !input.raw.hovered_files.is_empty());
        if !hovering {
            return;
        }
        let rect = ctx.content_rect().shrink(24.0);
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("drop-overlay"),
        ));
        painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(180));
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, theme::ACCENT),
            egui::StrokeKind::Inside,
        );
        let in_session = self.session.is_some()
            && matches!(self.screen, Screen::Player | Screen::Studio);
        let (title, colour) = if !self.ffmpeg_ready {
            ("FFmpeg is not on the PATH", theme::DANGER)
        } else if in_session {
            ("Drop to add a layer", theme::ACCENT)
        } else {
            ("Drop to start a session", theme::ACCENT)
        };
        let mut at = rect.center();
        if !self.ffmpeg_ready {
            at.y -= 12.0;
        }
        painter.text(
            at,
            egui::Align2::CENTER_CENTER,
            title,
            egui::FontId::proportional(16.0),
            colour,
        );
        if !self.ffmpeg_ready {
            painter.text(
                at + egui::vec2(0.0, 22.0),
                egui::Align2::CENTER_CENTER,
                "Install FFmpeg so that \"ffmpeg -version\" runs, then drop the file again",
                egui::FontId::proportional(12.0),
                theme::MUTED,
            );
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = now_ms();
        self.toasts.tick(now);
        self.handle_dropped(&ctx);
        for action in shortcuts::pressed(&ctx) {
            self.apply_action(action, &ctx);
        }
        self.pump_engine();

        match self.screen {
            Screen::Home => screens::home::show(self, ui),
            Screen::Sessions => screens::sessions::show(self, ui),
            Screen::Presets => screens::presets::show(self, ui),
            Screen::Studio => screens::studio::show(self, ui),
            Screen::Settings => screens::settings::show(self, ui),
            Screen::Player => screens::player::show(self, ui),
        }

        self.show_dialogs(&ctx);
        self.show_drop_overlay(&ctx);
        self.toasts.show(&ctx);

        // Keep drawing while frames are moving or files are still opening.
        let playing = self.runtime.as_ref().map(|r| r.is_playing()).unwrap_or(false);
        let probing = self
            .session
            .as_ref()
            .map(|session| {
                let runtime = self.runtime.as_ref();
                session.layers.iter().any(|layer| {
                    runtime
                        .map(|runtime| runtime.is_probing(&layer.id))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if playing {
            ctx.request_repaint_after(Duration::from_millis(8));
        } else if probing {
            ctx.request_repaint_after(Duration::from_millis(120));
        }

        // Save once when the window is closing.
        if ctx.input(|input| input.viewport().close_requested()) && !self.quit_saved {
            self.quit_saved = true;
            self.save_current();
        }
    }
}

/// A name for a session nobody named.
fn default_session_name() -> String {
    let stamp = now_ms() / 1_000;
    format!("Session {stamp}")
}

/// A file-name-safe version of a session name.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
        } else if (character.is_whitespace() || character == '-' || character == '_')
            && !out.ends_with('-') {
                out.push('-');
            }
    }
    let trimmed = out.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        "session".to_owned()
    } else {
        trimmed
    }
}

/// What a preset slot contributes, exposed for the presets screen's preview.
pub fn slot_summary(slot: &PresetSlot) -> String {
    format!(
        "{:.0}% x {:.0}% at {:.0}%, {:.0}%",
        slot.rect.w * 100.0,
        slot.rect.h * 100.0,
        slot.rect.x * 100.0,
        slot.opacity * 100.0
    )
}

/// Open the window and run until it closes.
pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Reaction Player")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([1024.0, 640.0])
            .with_app_id("reaction-player")
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Wgpu,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    eframe::run_native(
        "Reaction Player",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_file_name_safe() {
        assert_eq!(slug("Take 2: the flash"), "take-2-the-flash");
        assert_eq!(slug("!!!"), "session");
        assert_eq!(slug("a_b-c"), "a-b-c");
    }

    #[test]
    fn studio_delta_uses_b_minus_a() {
        let mut studio = Studio::default();
        assert_eq!(studio.delta_us(), None);
        studio.capture_a = Some(Capture {
            pts_us: 2_000_000,
            frames: Vec::new(),
        });
        studio.capture_b = Some(Capture {
            pts_us: 3_500_000,
            frames: Vec::new(),
        });
        assert_eq!(studio.delta_us(), Some(1_500_000));
        studio.reset();
        assert_eq!(studio.delta_us(), None);
    }

    #[test]
    fn slot_summary_reads_like_a_layout() {
        let slot = PresetSlot::new(NormRect::new(0.5, 0.25, 0.5, 0.5), 0);
        assert_eq!(slot_summary(&slot), "50% x 50% at 50%, 100%");
    }
}
