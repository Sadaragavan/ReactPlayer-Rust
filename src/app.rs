//! The application: state, screen routing, and the per-frame driver.
//!
//! `App` owns the persisted [`Store`], the current [`Session`], the engine
//! [`Runtime`], and every transient bit of UI state (selection, drag, toasts).
//! Screens are plain functions over `&mut App` so they stay readable and can be
//! reordered without touching engine code.

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, Rect};
use eframe::egui_wgpu;
use eframe::Frame;

use crate::engine::runtime::{LayerState, Runtime};
use crate::engine::track::{spawn_track, MediaInfo, TrackError, TrackHandle, TrackSpawn};
use crate::engine::video_frame::PlanePool;
use crate::gpu::capture::Capture;
use crate::gpu::compositor::{self, PaintList};
use crate::model::layout::CanvasAspect;
use crate::model::presets::PresetLibrary;
use crate::model::session::{LayerConfig, Session};
use crate::model::settings::{Preferences, Settings};
use crate::model::store::{generate_id, AppPaths, Store};
use crate::model::timeline::union_bounds;
use crate::model::UnionBounds;
use crate::ui::screens;
use crate::ui::shortcuts::{self, Action, KeyPress, Mods, TransportState};
use crate::ui::toast::{Toast, Toasts};

/// Everything remembered between runs.
#[derive(Debug, Clone)]
pub struct PersistedState {
    /// User preferences.
    pub preferences: Preferences,
    /// Preset library (built-ins are regenerated; user presets live here).
    pub presets: PresetLibrary,
}

/// A transient message shown under the top bar until dismissed.
#[derive(Debug, Clone)]
pub struct Banner {
    /// The message.
    pub message: String,
    /// `true` for errors, `false` for informational notes.
    pub is_error: bool,
}

/// Which layers the offset steppers act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetTarget {
    /// One specific layer.
    Layer(u64),
    /// Every layer at once.
    All,
}

impl OffsetTarget {
    /// The span applied by a stepper click of `micros` size.
    pub fn spans(&self) -> bool {
        matches!(self, OffsetTarget::All)
    }
}

/// A layer drag in progress on the canvas.
#[derive(Debug, Clone, Copy)]
pub struct DragState {
    /// The layer.
    pub layer_id: u64,
    /// The handle that started the drag; `None` moves the whole rect.
    pub handle: Option<crate::model::Handle>,
    /// Start position in normalized canvas coordinates.
    pub start_pos: (f32, f32),
    /// The rect at the start of the gesture.
    pub start_rect: crate::model::NormalizedRect,
    /// True once the pointer moved past the click threshold.
    pub moved: bool,
}

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Session picker / landing page.
    Home,
    /// The multi-layer editor.
    Editor,
}

/// The application.
pub struct App {
    // -------------------------------------------------------------- storage
    /// File store.
    pub store: Store,
    /// Preferences + presets.
    pub persisted: PersistedState,
    /// The session being edited and played.
    pub session: Session,
    /// Session cards on the home screen.
    pub sessions: Vec<crate::model::SessionSummary>,

    // --------------------------------------------------------------- engine
    /// Playback runtime (clock, audio, layers).
    pub runtime: Runtime,
    /// Probe results from workers.
    pub probe_rx: Receiver<(u64, Result<MediaInfo, TrackError>)>,
    /// Sender handed to workers.
    pub probe_tx: Sender<(u64, Result<MediaInfo, TrackError>)>,
    /// Shared plane pool.
    pub pool: Arc<PlanePool>,

    // ------------------------------------------------------------------- ui
    /// Active screen.
    pub screen: Screen,
    /// Selected layer id.
    pub selected: Option<u64>,
    /// Canvas rectangle in points, recomputed each frame.
    pub canvas_rect: Rect,
    /// Active drag gesture.
    pub drag: Option<DragState>,
    /// Snap guidelines to draw this frame: `(vertical, normalized_position)`.
    pub snap_guides: Vec<(bool, f32)>,
    /// Toasts.
    pub toasts: Toasts,
    /// Banner under the top bar.
    pub banner: Option<Banner>,
    /// Which layers the offset steppers act on.
    pub offset_target: OffsetTarget,
    /// Theater mode.
    pub theater: bool,
    /// Layers panel visibility.
    pub show_layers: bool,
    /// New Session modal state.
    pub new_session: Option<screens::new_session::NewSessionForm>,
    /// Alignment Studio state.
    pub studio: screens::alignment::StudioState,
    /// Settings screen draft.
    pub settings_draft: Option<Preferences>,
    /// Settings screen visibility.
    pub show_settings: bool,
    /// Cheat sheet visibility.
    pub show_cheats: bool,
    /// The paint list produced this frame.
    pub paint_list: PaintList,
    /// Union bounds cache.
    pub union: UnionBounds,
    /// Global layout lock.
    pub lock_layout: bool,
    /// The layer whose context menu is open.
    pub context_layer: Option<u64>,
    /// True while the pointer is over the canvas.
    pub canvas_hovered: bool,
    /// The egui context, cloned once so action handlers can talk to it.
    pub egui_ctx: egui::Context,
    /// eframe's wgpu render state, kept so compositor caches can be trimmed.
    pub render_state: Option<egui_wgpu::RenderState>,
    /// Layer ids whose GPU texture sets are currently cached.
    gpu_cache_keys: RefCell<Vec<u64>>,
}

impl App {
    /// Build from eframe's creation callback.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        // Install the compositor once, into eframe's wgpu render state.
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            compositor::install(render_state);
        }
        let render_state = cc.wgpu_render_state.clone();

        let (store, banner) = match Store::discover() {
            Ok(store) => (store, None),
            Err(e) => {
                // The UI must still come up: fall back to a temp directory and
                // say so loudly instead of panicking.
                let fallback = std::env::temp_dir().join("reaction-player");
                let store = Store::open(AppPaths::new(fallback.clone(), fallback))
                    .expect("a temp store is always creatable");
                (
                    store,
                    Some(Banner {
                        message: format!(
                            "Could not open the data directory ({e}). Using a temporary one: files will not be saved."
                        ),
                        is_error: true,
                    }),
                )
            }
        };

        let settings = store.load_settings();
        let presets = store.load_presets();
        let sessions = store.list_sessions();
        let persisted = PersistedState {
            preferences: settings.preferences,
            presets,
        };

        let mut session = persisted
            .preferences
            .last_session
            .as_ref()
            .and_then(|id| store.load_session(id).ok())
            .unwrap_or_else(|| Session::new(generate_id(), "Untitled", CanvasAspect::Widescreen));
        session.normalize_z();

        let (probe_tx, probe_rx) = std::sync::mpsc::channel();
        let mut runtime = Runtime::with_audio();
        runtime.apply_preferences(&persisted.preferences);

        let union = union_bounds(&session);
        Self {
            store,
            persisted,
            session,
            sessions,
            runtime,
            probe_rx,
            probe_tx,
            pool: PlanePool::new(),
            screen: Screen::Home,
            selected: None,
            canvas_rect: Rect::NOTHING,
            drag: None,
            snap_guides: Vec::new(),
            toasts: Toasts::new(),
            banner,
            offset_target: OffsetTarget::All,
            theater: false,
            show_layers: true,
            new_session: None,
            studio: screens::alignment::StudioState::default(),
            settings_draft: None,
            show_settings: false,
            show_cheats: false,
            paint_list: PaintList::default(),
            union,
            lock_layout: false,
            context_layer: None,
            canvas_hovered: false,
            egui_ctx: cc.egui_ctx.clone(),
            render_state,
            gpu_cache_keys: RefCell::new(Vec::new()),
        }
    }

    // ------------------------------------------------------------- session
    /// Recompute the union timeline.
    pub fn rebuild_union(&mut self) {
        self.union = union_bounds(&self.session);
    }

    /// Load a session, replacing the current one and re-binding the engine.
    pub fn open_session(&mut self, id: &str) {
        match self.store.load_session(id) {
            Ok(mut session) => {
                session.normalize_z();
                self.session = session;
                self.selected = None;
                self.screen = Screen::Editor;
                self.rebuild_union();
                self.persisted.preferences.last_session = Some(id.to_string());
                self.save_preferences();
                self.bind_runtime();
            }
            Err(e) => self.error(format!("Could not open session {id}: {e}")),
        }
    }

    /// Save the current session and refresh the home screen list.
    pub fn save_current_session(&mut self) {
        self.session.sanitize();
        if let Err(e) = self.store.save_session(&self.session) {
            self.error(format!("Could not save the session: {e}"));
            return;
        }
        self.sessions = self.store.list_sessions();
        if self.persisted.preferences.last_session.as_deref() != Some(self.session.id.as_str()) {
            self.persisted.preferences.last_session = Some(self.session.id.clone());
            self.save_preferences();
        }
    }

    /// Persist preferences.
    pub fn save_preferences(&mut self) {
        let settings = Settings {
            preferences: self.persisted.preferences.clone(),
        };
        if let Err(e) = self.store.save_settings(&settings) {
            self.error(format!("Could not save preferences: {e}"));
        }
    }

    /// Recreate the runtime and spawn a worker per layer.
    pub fn bind_runtime(&mut self) {
        self.runtime.shutdown();
        self.runtime = Runtime::with_audio();
        self.runtime.apply_preferences(&self.persisted.preferences);
        self.runtime.linked = self.session.linked;
        self.runtime.master_audio = self.session.master_audio_layer;
        self.runtime.master_muted = false;
        self.runtime.master_volume = 1.0;
        // A fresh session starts at the union start so a short offset layer
        // does not begin mid-sentence.
        self.runtime.seek_timeline(self.union.start_us);
        let layers = self.session.layers.clone();
        for layer in &layers {
            self.spawn_layer(layer);
        }
    }

    /// Spawn one layer's decode worker.
    pub fn spawn_layer(&mut self, layer: &LayerConfig) {
        if layer.source.trim().is_empty() {
            return;
        }
        let path = std::path::PathBuf::from(&layer.source);
        if !path.exists() && !is_url(&layer.source) {
            self.error(format!(
                "{}: media not found at {}. Use Relink to point at the file.",
                layer.label,
                path.display()
            ));
            return;
        }
        let spec = TrackSpawn {
            id: layer.id,
            path,
            hardware: self.persisted.preferences.hardware,
            output_sample_rate: self.runtime.audio.sample_rate,
            seek_epoch: self.runtime.epoch().clone(),
            pool: Arc::clone(&self.pool),
        };
        let handle: TrackHandle = spawn_track(spec, self.probe_tx.clone());
        let mut state = LayerState::new(handle);
        state.offset_us = layer.offset_us;
        self.runtime.insert_layer(state);
    }

    /// Point a layer at a media file and start decoding it.
    ///
    /// Replaces whatever the layer was playing: the old worker is stopped
    /// first, so a relink cannot leave two decoders writing into the same
    /// layer id. The offset is kept -- relinking is what you do when a file
    /// moved, and the timing decision was already made.
    pub fn load_layer_media(&mut self, id: u64, source: String) {
        let Some(layer) = self.session.layer_mut(id) else {
            self.error("That layer no longer exists");
            return;
        };
        let label = layer.label.clone();
        layer.source = source;
        let updated = layer.clone();
        self.runtime.remove_layer(id);
        self.spawn_layer(&updated);
        self.save_current_session();
        self.notice(format!("{label}: media loaded"));
    }

    /// Report an error in the banner and as a toast.
    pub fn error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.toasts.push(Toast::error(message.clone()));
        self.banner = Some(Banner {
            message,
            is_error: true,
        });
    }

    /// Report informational text as a toast.
    pub fn notice(&mut self, message: impl Into<String>) {
        self.toasts.push(Toast::info(message));
    }

    // -------------------------------------------------------------- actions
    /// Execute one keyboard action.
    pub fn apply_action(&mut self, action: Action) {
        match action {
            Action::TogglePlay => self.toggle_play(),
            Action::ToggleLink => {
                self.session.linked = !self.session.linked;
                self.runtime.linked = self.session.linked;
                if self.session.linked {
                    self.notice("Transports linked");
                } else {
                    self.notice("Transports unlinked");
                }
                self.save_current_session();
            }
            Action::ToggleTheater => self.theater = !self.theater,
            Action::ToggleFullscreen => {
                let is_full = self
                    .egui_ctx
                    .input(|i| i.viewport().fullscreen.unwrap_or(false));
                self.egui_ctx
                    .send_viewport_cmd(egui::ViewportCommand::Fullscreen(!is_full));
            }
            Action::ToggleLayersPanel => self.show_layers = !self.show_layers,
            Action::StepMicros(delta) => self.runtime.step_timeline(delta),
            Action::StepFrame { forward } => self.runtime.step_one_frame_all(forward),
            Action::NudgeOffset(delta) => self.nudge_offset(delta),
            Action::BringForward => self.reorder_selected(|s, id| s.bring_forward(id)),
            Action::SendBackward => self.reorder_selected(|s, id| s.send_backward(id)),
            Action::BringToFront => self.reorder_selected(|s, id| s.bring_to_front(id)),
            Action::SendToBack => self.reorder_selected(|s, id| s.send_to_back(id)),
            Action::NudgeWipe(pct) => {
                self.studio.wipe =
                    crate::gpu::capture::nudge_wipe(self.studio.wipe, pct);
            }
            Action::Capture => self.capture_frame(),
            Action::ToggleMasterMute => {
                self.runtime.master_muted = !self.runtime.master_muted;
                if self.runtime.master_muted {
                    self.notice("Master audio muted");
                } else {
                    self.notice("Master audio unmuted");
                }
            }
            Action::CycleCompareMode => self.studio.cycle_mode(),
            Action::Escape => self.escape(),
            Action::ToggleCheatSheet => self.show_cheats = !self.show_cheats,
        }
    }

    fn reorder_selected(&mut self, f: impl FnOnce(&mut Session, u64) -> bool) {
        let Some(id) = self.selected else {
            return;
        };
        if f(&mut self.session, id) {
            self.session.normalize_z();
            self.save_current_session();
        }
    }

    fn toggle_play(&mut self) {
        let playing = !self.runtime.is_playing();
        if playing {
            let t = self.runtime.clock.position_us();
            if self.union.is_empty() {
                self.notice("Nothing to play: every layer is empty");
                return;
            }
            if t < self.union.start_us || t >= self.union.end_us {
                self.runtime.seek_timeline(self.union.start_us);
            }
        }
        self.runtime.set_playing(playing);
    }

    fn nudge_offset(&mut self, delta_us: i64) {
        match self.offset_target {
            OffsetTarget::Layer(id) => {
                if let Some(layer) = self.session.layer_mut(id) {
                    layer.offset_us = layer.offset_us.saturating_add(delta_us);
                    self.runtime.set_offset(id, layer.offset_us);
                }
                self.rebuild_union();
                self.save_current_session();
            }
            OffsetTarget::All => {
                for layer in &mut self.session.layers {
                    layer.offset_us = layer.offset_us.saturating_add(delta_us);
                }
                let pairs: Vec<(u64, i64)> = self
                    .session
                    .layers
                    .iter()
                    .map(|l| (l.id, l.offset_us))
                    .collect();
                for (id, off) in pairs {
                    self.runtime.set_offset(id, off);
                }
                self.rebuild_union();
                self.save_current_session();
            }
        }
    }

    fn escape(&mut self) {
        // Cascade, topmost first; the first thing that closes ends it.
        if self.show_cheats {
            self.show_cheats = false;
        } else if self.new_session.is_some() {
            self.new_session = None;
        } else if self.show_settings {
            self.show_settings = false;
            self.settings_draft = None;
        } else if self.context_layer.take().is_some() {
            // closed the context menu
        } else if self.drag.take().is_some() {
            // cancelled the drag
        } else if self.selected.take().is_some() {
            // deselected
        } else if self.screen == Screen::Editor {
            self.runtime.set_playing(false);
            self.save_current_session();
            self.screen = Screen::Home;
        }
    }

    /// Capture the selected layer's current frame for alignment.
    pub fn capture_frame(&mut self) {
        let Some(id) = self
            .selected
            .or_else(|| self.session.layers.first().map(|l| l.id))
        else {
            self.error("Nothing to capture: no layer is loaded");
            return;
        };
        let capture = self
            .runtime
            .layers
            .get(&id)
            .and_then(|s| s.presented.as_ref())
            .map(|p| Capture::new(p.frame.pts_us, Arc::new(p.frame.clone()), p.color));
        let Some(capture) = capture else {
            self.error("No decoded frame yet: play for a moment and try again");
            return;
        };
        let pts_ms = capture.pts_ms();
        self.studio.push_for(id, capture);
        let label = self
            .session
            .layer(id)
            .map(|l| l.label.clone())
            .unwrap_or_else(|| format!("layer {id}"));
        self.notice(format!("Captured {label} at {pts_ms} ms"));
    }

    /// Drain worker probe results into the runtime.
    pub fn poll_probes(&mut self) {
        while let Ok((id, result)) = self.probe_rx.try_recv() {
            match result {
                Ok(info) => {
                    if let Some(state) = self.runtime.layers.get_mut(&id) {
                        state.info = Some(info);
                    }
                }
                Err(e) => {
                    self.error(format!("Layer {id} failed to open: {e}"));
                }
            }
        }
    }

    /// Handle keyboard + text events for this frame.
    pub fn handle_keys(&mut self, ctx: &egui::Context) {
        // While a text field is focused everything belongs to it, including
        // Space and `?`.
        if ctx.text_edit_focused() {
            return;
        }
        let state = if self.runtime.is_playing() {
            TransportState::Playing
        } else {
            TransportState::Paused
        };
        let seek_step_us = self.persisted.preferences.seek_step_us();
        let mut actions: Vec<Action> = Vec::new();
        let events = ctx.input(|i| i.events.clone());
        for event in &events {
            match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    repeat,
                    ..
                } => {
                    let mods = Mods {
                        shift: modifiers.shift,
                        ctrl: modifiers.ctrl,
                        alt: modifiers.alt,
                        command: modifiers.command,
                    };
                    if let Some(action) = shortcuts::map_key(
                        KeyPress {
                            key: *key,
                            mods,
                            repeat: *repeat,
                        },
                        state,
                        seek_step_us,
                    ) {
                        actions.push(action);
                    }
                }
                egui::Event::Text(text) => {
                    if let Some(action) = shortcuts::map_text(text) {
                        actions.push(action);
                    }
                }
                _ => {}
            }
        }
        for action in actions {
            self.apply_action(action);
        }
        let _: Option<KeyPress> = None;
    }
}

/// True for FFmpeg-supported URL schemes.
pub fn is_url(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    const SCHEMES: [&str; 7] = [
        "http://", "https://", "rtmp://", "rtsp://", "udp://", "tcp://", "file://",
    ];
    SCHEMES.iter().any(|s| lower.starts_with(s))
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut Frame) {
        let ctx = ui.ctx().clone();
        self.egui_ctx = ctx.clone();

        self.poll_probes();
        self.runtime.tick();
        self.handle_keys(&ctx);

        // Panels are drawn back-to-front so the central canvas sees the space
        // that is really left over.
        screens::top_bar::show(self, ui);
        screens::banner::show(self, ui);

        if self.screen == Screen::Editor {
            screens::transport::show(self, ui);
            if self.show_layers && !self.theater {
                screens::layers::show(self, ui);
            }
            screens::canvas::show(self, ui);
        } else {
            screens::home::show(self, ui);
        }

        screens::modals::show_all(self, &ctx);
        screens::toasts::show(self, ui);

        self.retire_gpu_caches();
    }

    /// Drop GPU texture sets for layers that no longer exist.
    ///
    /// The compositor caches up to three plane textures per layer, so deleting
    /// a clip would otherwise leak its set until the app exits. Runs every
    /// frame and costs one `retain` over a handful of entries. The render
    /// state is absent under a non-wgpu backend, in which case there is no
    /// cache to trim.
    fn retire_gpu_caches(&self) {
        let Some(render_state) = self.render_state.as_ref() else {
            return;
        };
        let live: Vec<u64> = self.session.layers.iter().map(|l| l.id).collect();
        // Only take the renderer lock when the layer set actually changed:
        // writing to it every frame would contend with the paint thread for
        // nothing.
        if *self.gpu_cache_keys.borrow() == live {
            return;
        }
        if let Ok(mut renderer) = render_state.renderer.write() {
            crate::gpu::compositor::retire_layers(&mut renderer.callback_resources, &live);
        }
        *self.gpu_cache_keys.borrow_mut() = live;
    }
}
