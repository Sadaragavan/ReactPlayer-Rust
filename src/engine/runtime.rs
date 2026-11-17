//! The multi-layer runtime: the object the UI talks to.
//!
//! The runtime owns one [`TrackHandle`] per layer plus the [`MasterClock`] and
//! [`AudioEngine`], and implements every *policy* decision that can be made
//! without touching a device:
//!
//! * which media frame to present (following the master clock, holding the
//!   first frame before a layer's span and the last frame after it),
//! * drift classification and the resulting rate nudge or seek,
//! * "keep the sound going" master-audio lending,
//! * the union timeline and the frame/offset steppers.
//!
//! The policy lives here (free of FFmpeg types) so it is directly unit-tested;
//! the device-facing parts are thin.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::model::session::Session;
use crate::model::timeline::{
    clamp_track_time, layer_active, layer_source_time_us, union_bounds, TrackSpan, UnionBounds,
};
use crate::model::{CropRect, Preferences};

use super::audio::AudioEngine;
use super::hwaccel::DecodeBackend;
use super::sync_clock::{self, DriftAction, DriftController, MasterClock};
use super::track::{MediaInfo, PresentedFrame, TrackError, TrackHandle};
use super::video_frame::RgbaImage;

/// How long a seek may stay outstanding before the runtime stops suppressing
/// drift handling (network streams can be slow).
pub const SEEK_PENDING_TIMEOUT_MS: u128 = 250;

/// What the renderer needs for one layer in a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerPresentation {
    /// Layer id.
    pub layer_id: u64,
    /// Media timestamp currently on screen, or `None` while loading.
    pub pts_us: Option<i64>,
    /// True when the layer is holding its first frame (before its span).
    pub holding_first: bool,
    /// True when the layer is holding its last frame (after its span).
    pub holding_last: bool,
    /// True when the presented frame changed this tick, i.e. the GPU should
    /// re-upload the textures (they only change when the pts changes).
    pub pts_changed: bool,
}

/// Live state of one layer.
pub struct LayerState {
    /// Layer id.
    pub id: u64,
    /// The decode worker.
    pub track: TrackHandle,
    /// Probe info once the worker has opened the file.
    pub info: Option<MediaInfo>,
    /// Presentation currently on screen.
    pub presented: Option<PresentedFrame>,
    /// Drift controller with hysteresis.
    pub drift: DriftController,
    /// The pts handed to the renderer last frame.
    pub last_presented_pts: Option<i64>,
    /// Set while a seek is outstanding; drift actions are suppressed until it
    /// lands or [`SEEK_PENDING_TIMEOUT_MS`] elapses.
    pub seek_pending: bool,
    /// When the last seek was requested.
    pub last_seek_at: Option<Instant>,
    /// Frames presented since the last statistics reset.
    pub frames_presented: u64,
    /// Last error reported for this layer.
    pub error: Option<TrackError>,
    /// Current effective offset, mirrored from the session by the UI.
    pub offset_us: i64,
}

impl LayerState {
    /// Wrap a track handle in a layer state with zero offset.
    pub fn new(track: TrackHandle) -> Self {
        let id = track.id();
        Self {
            id,
            track,
            info: None,
            presented: None,
            drift: DriftController::default(),
            last_presented_pts: None,
            seek_pending: false,
            last_seek_at: None,
            frames_presented: 0,
            error: None,
            offset_us: 0,
        }
    }

    /// True once the layer has been probed and has a frame to show.
    pub fn is_ready(&self) -> bool {
        self.info.is_some() && self.presented.is_some()
    }

    /// Media duration, or zero while probing.
    pub fn duration_us(&self) -> i64 {
        self.info.as_ref().map(|i| i.duration_us).unwrap_or(0)
    }

    /// Decode backend badge.
    pub fn backend(&self) -> DecodeBackend {
        self.info
            .as_ref()
            .map(|i| i.backend)
            .unwrap_or_else(|| self.track.backend())
    }
}

/// Which layer's audio is being heard this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioRouting {
    /// The layer whose audio is audible, if any.
    pub active_audio_layer: Option<u64>,
    /// True when audio was lent away from the master layer because it has no
    /// footage at the current instant.
    pub lent: bool,
}

/// The runtime.
pub struct Runtime {
    /// Master timeline clock (audio driven, or `Instant` driven).
    pub clock: Arc<MasterClock>,
    /// Audio output and the mixer.
    pub audio: AudioEngine,
    /// Per-layer state, keyed by layer id.
    pub layers: HashMap<u64, LayerState>,
    /// Whether layers follow the master timeline together.
    pub linked: bool,
    /// Master audio layer id.
    pub master_audio: Option<u64>,
    /// "Keep the sound going".
    pub keep_sound_going: bool,
    /// Master mute (key `M`).
    pub master_muted: bool,
    /// Master output volume in `0..=1`.
    pub master_volume: f32,
    /// Last routing decision.
    pub routing: AudioRouting,
    /// Shared seek epoch, bumped on every seek.
    pub seek_epoch: Arc<std::sync::atomic::AtomicU64>,
}

impl Runtime {
    /// Runtime with a real audio device (silently falls back to `Instant`
    /// when no device exists).
    pub fn with_audio() -> Self {
        Self::build(AudioEngine::with_device(Arc::new(MasterClock::new())))
    }

    /// Runtime without touching any audio device. Used by tests and by
    /// `--no-audio`.
    pub fn new_silent() -> Self {
        Self::build(AudioEngine::silent())
    }

    fn build(audio: AudioEngine) -> Self {
        let clock = Arc::clone(&audio.clock);
        clock.set_audio_driven(audio.device_available());
        Self {
            clock,
            audio,
            layers: HashMap::new(),
            linked: true,
            master_audio: None,
            keep_sound_going: true,
            master_muted: false,
            master_volume: 1.0,
            routing: AudioRouting::default(),
            seek_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// The epoch counter other components tag data with.
    pub fn epoch(&self) -> &Arc<std::sync::atomic::AtomicU64> {
        &self.seek_epoch
    }

    /// Insert or replace a layer state.
    pub fn insert_layer(&mut self, state: LayerState) {
        let id = state.id;
        // Connect the track's ring buffer to the mixer before it can produce
        // anything. Without this the decode worker writes PcmChunks into a ring
        // nobody drains and the app is silent; the callback-side mixer only
        // knows about sources registered here.
        if let Some(ring) = state.track.audio() {
            let consumer = ring.attach_consumer();
            self.audio.add_source(id, consumer);
        }
        self.layers.insert(id, state);
        if self.master_audio.is_none() {
            self.master_audio = Some(id);
        }
    }

    /// Remove a layer and its audio source.
    pub fn remove_layer(&mut self, id: u64) -> Option<LayerState> {
        self.audio.remove_source(id);
        let removed = self.layers.remove(&id);
        if self.master_audio == Some(id) {
            self.master_audio = self.layers.keys().copied().min();
        }
        removed
    }

    /// Mirror the session's master/link state into the runtime.
    pub fn bind_session(&mut self, session: &Session) {
        self.master_audio = session.master_audio_layer.or(self.master_audio);
        self.linked = session.linked;
        for layer in &session.layers {
            self.set_offset(layer.id, layer.offset_us);
            self.audio.set_track_volume(layer.id, layer.volume);
        }
    }

    /// Set a layer's effective offset (`target = master + offset`).
    pub fn set_offset(&mut self, layer_id: u64, offset_us: i64) {
        if let Some(state) = self.layers.get_mut(&layer_id) {
            state.offset_us = offset_us;
        }
    }

    /// Effective offset of a layer.
    pub fn offset(&self, layer_id: u64) -> i64 {
        self.layers
            .get(&layer_id)
            .map(|s| s.offset_us)
            .unwrap_or(0)
    }

    /// Union bounds over every probed layer.
    pub fn union(&self) -> UnionBounds {
        union_bounds(&self.spans())
    }

    /// One span per probed layer.
    pub fn spans(&self) -> Vec<TrackSpan> {
        self.layers
            .values()
            .filter(|l| l.info.is_some())
            .map(|l| TrackSpan::new(l.duration_us(), l.offset_us))
            .collect()
    }

    /// Span of a single layer, if probed.
    pub fn span(&self, layer_id: u64) -> Option<TrackSpan> {
        let s = self.layers.get(&layer_id)?;
        s.info.as_ref()?;
        Some(TrackSpan::new(s.duration_us(), s.offset_us))
    }

    /// Media time this layer must present at timeline instant `t_us`,
    /// clamped to its own duration (first/last frame hold).
    pub fn media_time_for(&self, layer_id: u64, t_us: i64) -> Option<i64> {
        let state = self.layers.get(&layer_id)?;
        state.info.as_ref()?;
        let offset = state.offset_us;
        let source = layer_source_time_us(t_us, offset);
        Some(clamp_track_time(source, state.duration_us()))
    }

    /// True when the layer has its own footage at `t_us`.
    pub fn is_active(&self, layer_id: u64, t_us: i64) -> bool {
        let Some(state) = self.layers.get(&layer_id) else {
            return false;
        };
        if state.info.is_none() {
            return false;
        }
        layer_active(t_us, state.offset_us, state.duration_us())
    }

    /// Presentation info for a layer. Does not mutate except bookkeeping.
    pub fn presentation_for(&self, layer_id: u64, t_us: i64) -> Option<LayerPresentation> {
        let state = self.layers.get(&layer_id)?;
        let presented_pts = state.presented.as_ref().map(|p| p.frame.pts_us);
        let Some(_) = state.info.as_ref() else {
            return Some(LayerPresentation {
                layer_id,
                pts_us: None,
                holding_first: false,
                holding_last: false,
                pts_changed: false,
            });
        };
        let wanted = clamp_track_time(t_us + state.offset_us, state.duration_us());
        Some(LayerPresentation {
            layer_id,
            pts_us: presented_pts,
            holding_first: t_us + state.offset_us < 0,
            holding_last: t_us + state.offset_us > state.duration_us(),
            pts_changed: presented_pts != Some(wanted),
        })
    }

    /// Classify drift for one layer and apply the resulting action.
    pub fn handle_drift(&mut self, layer_id: u64, actual_pts_us: i64, t_us: i64) -> DriftAction {
        let Some(state) = self.layers.get_mut(&layer_id) else {
            return DriftAction::Ignore;
        };
        if state.seek_pending {
            return DriftAction::Ignore;
        }
        let offset = state.offset_us;
        let drift = sync_clock::drift_us(actual_pts_us, t_us, offset);
        let action = state.drift.update(drift);
        match action {
            DriftAction::Seek => {
                let target = sync_clock::target_pts_us(t_us, offset);
                // Copy out before borrowing self mutably again.
                self.request_seek(layer_id, target);
            }
            DriftAction::Nudge(rate) => {
                state.track.set_rate(rate);
                self.audio.set_track_rate(layer_id, rate);
            }
            DriftAction::Ignore => {
                state.track.set_rate(1.0);
                self.audio.set_track_rate(layer_id, 1.0);
            }
        }
        action
    }

    /// Issue a seek on one layer and bump the shared epoch.
    pub fn request_seek(&mut self, layer_id: u64, target_us: i64) {
        self.seek_epoch
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Some(state) = self.layers.get_mut(&layer_id) {
            state.seek_pending = true;
            state.last_seek_at = Some(Instant::now());
            state.drift.reset();
            state.track.seek(target_us.max(0));
        }
        self.audio.set_track_rate(layer_id, 1.0);
    }

    /// Mark a layer's seek as landed.
    pub fn clear_seek_pending(&mut self, layer_id: u64) {
        if let Some(state) = self.layers.get_mut(&layer_id) {
            state.seek_pending = false;
        }
    }

    /// Seek the whole timeline so that instant `t_us` is shown.
    pub fn seek_timeline(&mut self, t_us: i64) {
        self.clock.set_position_us(t_us);
        let ids: Vec<u64> = self.layers.keys().copied().collect();
        for id in ids {
            if let Some(target) = self.media_time_for(id, t_us) {
                self.request_seek(id, target);
            }
        }
    }

    /// Step the master timeline by `delta_us`.
    pub fn step_timeline(&mut self, delta_us: i64) {
        let t = self.clock.position_us() + delta_us;
        self.seek_timeline(t);
    }

    /// Step one layer independently (Unlinked mode).
    pub fn step_layer(&mut self, layer_id: u64, delta_us: i64) {
        let current = self
            .layers
            .get(&layer_id)
            .and_then(|s| s.presented.as_ref().map(|p| p.frame.pts_us))
            .or_else(|| self.media_time_for(layer_id, self.clock.position_us()))
            .unwrap_or(0);
        self.request_seek(layer_id, (current + delta_us).max(0));
    }

    /// The frame duration of a layer in microseconds.
    pub fn frame_duration_us(&self, layer_id: u64) -> i64 {
        self.layers
            .get(&layer_id)
            .and_then(|s| s.info.as_ref())
            .map(|i| i.frame_duration_us())
            .unwrap_or(33_333)
    }

    /// Step exactly one frame on every linked layer, keeping the master clock
    /// consistent with the driving layer.
    pub fn step_one_frame_all(&mut self, forward: bool) {
        let t = self.clock.position_us();
        let ids: Vec<u64> = self.layers.keys().copied().collect();
        if ids.is_empty() {
            return;
        }
        // Use the primary (lowest id) layer's frame duration as the timeline
        // step, since layers can have different frame rates.
        let primary = *ids.iter().min().unwrap_or(&ids[0]);
        let step = self.frame_duration_us(primary);
        for id in &ids {
            let Some(current) = self.media_time_for(*id, t) else {
                continue;
            };
            let target = if forward {
                current + self.frame_duration_us(*id)
            } else {
                (current - self.frame_duration_us(*id)).max(0)
            };
            self.request_seek(*id, target);
        }
        let new_t = if forward {
            (t + step).max(0)
        } else {
            (t - step).max(0)
        };
        self.clock.set_position_us(new_t);
    }

    /// Play/pause every layer.
    pub fn set_playing(&mut self, playing: bool) {
        self.clock.set_playing(playing);
        for state in self.layers.values() {
            state.track.set_playing(playing);
        }
    }

    /// True while the transport is running.
    pub fn is_playing(&self) -> bool {
        self.clock.is_playing()
    }

    /// Decide which layer's audio is heard and push the decision to the mixer.
    ///
    /// The master layer wins while it has footage at `t_us`. When "keep the
    /// sound going" is on and the master is outside its span, audio is lent to
    /// the highest-priority active layer (highest `z_index`). Lending only
    /// changes the mixer's mute state; the session's mute flags are untouched,
    /// so everything reverts automatically.
    pub fn route_audio(&mut self, t_us: i64, session: &Session) -> AudioRouting {
        let master = self.master_audio.or(session.master_audio_layer);
        let master_active = master.is_some_and(|id| self.is_active(id, t_us));

        let chosen = if master_active {
            master
        } else if self.keep_sound_going {
            session
                .layers
                .iter()
                .filter(|l| l.visible && self.is_active(l.id, t_us))
                .max_by_key(|l| l.z_index)
                .map(|l| l.id)
        } else {
            None
        };
        let lent = chosen.is_some() && chosen != master;

        for layer in &session.layers {
            let audible = Some(layer.id) == chosen;
            self.audio
                .set_track_muted(layer.id, !audible || self.master_muted);
            self.audio.set_track_volume(layer.id, layer.volume);
        }
        self.audio.set_master_volume(self.master_volume);
        self.routing = AudioRouting {
            active_audio_layer: chosen,
            lent,
        };
        self.routing
    }

    /// One UI tick: pull the newest frames, expire stale seeks, steer audio and
    /// correct drift.
    ///
    /// This is the real per-frame entry point; `tick` is the session-less form
    /// used by tests. The two coordination loops that keep picture and sound
    /// together both live here:
    ///
    /// * `route_audio` picks which layer is audible at `t` and mutes the rest.
    /// * `handle_drift` nudges or seeks a layer whose decoded PTS has wandered
    ///   away from the master clock.
    ///
    /// Both were previously defined but never called, so every layer was silent
    /// and nothing corrected drift.
    pub fn tick_with_session(&mut self, session: &Session) -> HashMap<u64, LayerPresentation> {
        let out = self.tick();
        let t = self.clock.position_us();
        self.route_audio(t, session);
        // Correct drift for every layer that has a presented frame. A layer
        // holding a still has nothing to correct against, and `handle_drift`
        // already ignores pending seeks.
        let ids: Vec<u64> = self.layers.keys().copied().collect();
        for id in ids {
            let Some(pts) = self
                .layers
                .get(&id)
                .and_then(|s| s.presented.as_ref())
                .map(|p| p.frame.pts_us)
            else {
                continue;
            };
            self.handle_drift(id, pts, t);
        }
        out
    }

    /// One UI tick: pull the newest frames, expire stale seeks and return the
    /// presentation map.
    pub fn tick(&mut self) -> HashMap<u64, LayerPresentation> {
        let t = self.clock.position_us();
        let ids: Vec<u64> = self.layers.keys().copied().collect();
        let mut out = HashMap::new();
        for id in ids {
            let Some(state) = self.layers.get_mut(&id) else {
                continue;
            };
            let mut newest = None;
            while let Some(f) = state.track.latest_frame() {
                newest = Some(f);
            }
            let mut pts_changed = false;
            if let Some(f) = newest {
                let changed = state.presented.as_ref().map(|p| p.frame.pts_us) != Some(f.frame.pts_us);
                if changed {
                    state.presented = Some(f);
                    state.frames_presented = state.frames_presented.saturating_add(1);
                    pts_changed = true;
                }
            }
            if let Some(err) = state.track.poll_error() {
                state.error = Some(err);
            }
            if state.seek_pending
                && state
                    .last_seek_at
                    .is_some_and(|at| at.elapsed().as_millis() > SEEK_PENDING_TIMEOUT_MS)
            {
                state.seek_pending = false;
            }
            let duration = state.duration_us();
            let offset = state.offset_us;
            let presented_pts = state.presented.as_ref().map(|p| p.frame.pts_us);
            let wanted = clamp_track_time(t + offset, duration);
            let waiting_for_target = presented_pts.is_none_or(|p| p != wanted);
            out.insert(
                id,
                LayerPresentation {
                    layer_id: id,
                    pts_us: presented_pts,
                    holding_first: t + offset < 0,
                    holding_last: t + offset > duration,
                    pts_changed: pts_changed || waiting_for_target,
                },
            );
            state.last_presented_pts = presented_pts;
        }
        out
    }

    /// Copy the presented frame of a layer to RGBA using its crop and colour
    /// range. Reports failures instead of panicking.
    pub fn copy_frame_rgba(&self, layer_id: u64, crop: &CropRect) -> Result<RgbaImage, String> {
        let state = self
            .layers
            .get(&layer_id)
            .ok_or_else(|| "layer not found".to_string())?;
        let presented = state
            .presented
            .as_ref()
            .ok_or_else(|| "no decoded frame yet".to_string())?;
        presented
            .frame
            .crop_to_rgba(crop, presented.color)
            .map_err(|e| e.to_string())
    }

    /// Apply engine-affecting preferences.
    pub fn apply_preferences(&mut self, prefs: &Preferences) {
        self.keep_sound_going = prefs.keep_sound_going;
    }

    /// Stop every worker and the audio stream.
    pub fn shutdown(&mut self) {
        self.layers.clear();
        self.audio.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::geom::NormalizedRect;
    use crate::model::session::{LayerConfig, LayerRole};
    use crate::model::timeline::layer_active;

    /// Build a runtime with a layer that has a fake probe result and no worker.
    ///
    /// The layer is driven by a `TrackHandle` created through
    /// `spawn_track_synthetic`, which never opens a file.
    fn runtime_with_layer(id: u64, duration_us: i64) -> (Runtime, LayerHandleGuard) {
        let mut rt = Runtime::new_silent();
        let (track, guard) = super::super::track::synthetic_track(id, duration_us);
        let mut state = LayerState::new(track);
        state.info = Some(MediaInfo {
            fps: (30, 1),
            width: 1920,
            height: 1080,
            duration_us,
            pixel_format: "yuv420p".into(),
            rotation_deg: 0,
            audio_channels: None,
            audio_sample_rate: None,
            converted: false,
            backend: DecodeBackend::Software,
        });
        rt.insert_layer(state);
        (rt, guard)
    }

    struct LayerHandleGuard(u64);

    impl Drop for LayerHandleGuard {
        fn drop(&mut self) {
            let _ = self.0;
        }
    }

    #[test]
    fn runtime_without_device_falls_back_to_instant_clock() {
        let rt = Runtime::new_silent();
        assert!(!rt.clock.is_audio_driven());
        assert!(!rt.audio.device_available());
    }

    #[test]
    fn media_time_follows_offset_and_clamps() {
        let (mut rt, _g) = runtime_with_layer(1, 10_000_000);
        rt.set_offset(1, 1_500_000);
        // Timeline 0 -> media 1.5 s.
        assert_eq!(rt.media_time_for(1, 0), Some(1_500_000));
        // Before the layer's span the first frame is held.
        assert_eq!(rt.media_time_for(1, -5_000_000), Some(0));
        // After the span the last frame is held.
        assert_eq!(rt.media_time_for(1, 20_000_000), Some(10_000_000));
    }

    #[test]
    fn union_uses_each_layers_offset() {
        let (mut rt, _g1) = runtime_with_layer(1, 10_000_000);
        let (track2, _g2) = super::super::track::synthetic_track(2, 8_000_000);
        let mut s2 = LayerState::new(track2);
        s2.info = Some(MediaInfo {
            fps: (30, 1),
            width: 640,
            height: 480,
            duration_us: 8_000_000,
            pixel_format: "yuv420p".into(),
            rotation_deg: 0,
            audio_channels: None,
            audio_sample_rate: None,
            converted: false,
            backend: DecodeBackend::Software,
        });
        rt.insert_layer(s2);
        rt.set_offset(1, 0);
        rt.set_offset(2, 3_000_000);
        let u = rt.union();
        assert_eq!(u.start_us, -3_000_000);
        assert_eq!(u.end_us, 10_000_000);
    }

    #[test]
    fn drift_handling_issues_nudges_and_seeks() {
        let (mut rt, _g) = runtime_with_layer(1, 10_000_000);
        rt.set_offset(1, 0);
        assert_eq!(rt.handle_drift(1, 2_000, 0), DriftAction::Ignore);
        assert!(matches!(
            rt.handle_drift(1, 40_000, 0),
            DriftAction::Nudge(_)
        ));
        assert_eq!(rt.handle_drift(1, 200_000, 0), DriftAction::Seek);
    }

    #[test]
    fn seek_pending_suppresses_further_actions() {
        let (mut rt, _g) = runtime_with_layer(1, 10_000_000);
        assert_eq!(rt.handle_drift(1, 500_000, 0), DriftAction::Seek);
        assert_eq!(
            rt.handle_drift(1, 500_000, 0),
            DriftAction::Ignore,
            "a pending seek must not be re-issued every tick"
        );
        rt.clear_seek_pending(1);
        assert_eq!(rt.handle_drift(1, 500_000, 0), DriftAction::Seek);
    }

    fn sample_session_with_layers(layers: Vec<LayerConfig>) -> Session {
        let mut s = Session::new("t", "T", crate::model::CanvasAspect::Widescreen);
        s.layers = layers;
        s.sanitize();
        s
    }

    fn layer(id: u64, z: i32) -> LayerConfig {
        LayerConfig::new(
            id,
            format!("L{id}"),
            LayerRole::Reaction1,
            "/tmp/x.mp4",
            NormalizedRect::full(),
            z,
        )
    }

    fn runtime_with_durations(durations: &[(u64, i64)]) -> Runtime {
        let mut rt = Runtime::new_silent();
        for (id, d) in durations {
            let (track, guard) = super::super::track::synthetic_track(*id, *d);
            std::mem::forget(guard);
            let mut state = LayerState::new(track);
            state.info = Some(MediaInfo {
                fps: (30, 1),
                width: 640,
                height: 360,
                duration_us: *d,
                pixel_format: "yuv420p".into(),
                rotation_deg: 0,
                audio_channels: None,
                audio_sample_rate: None,
                converted: false,
                backend: DecodeBackend::Software,
            });
            rt.insert_layer(state);
        }
        rt
    }

    #[test]
    fn audio_lending_prefers_highest_z_when_master_inactive() {
        let mut rt = runtime_with_durations(&[(1, 5_000_000), (2, 10_000_000), (3, 10_000_000)]);
        rt.master_audio = Some(1);
        rt.keep_sound_going = true;
        let session = sample_session_with_layers(vec![layer(1, 0), layer(2, 1), layer(3, 2)]);

        let r = rt.route_audio(1_000_000, &session);
        assert_eq!(r.active_audio_layer, Some(1));
        assert!(!r.lent);

        let r = rt.route_audio(7_000_000, &session);
        assert_eq!(r.active_audio_layer, Some(3));
        assert!(r.lent);

        rt.keep_sound_going = false;
        let r = rt.route_audio(7_000_000, &session);
        assert_eq!(r.active_audio_layer, None);
        assert!(!r.lent);
    }

    #[test]
    fn lending_reverts_when_the_master_returns() {
        let mut rt = runtime_with_durations(&[(1, 10_000_000), (2, 10_000_000)]);
        rt.master_audio = Some(1);
        rt.keep_sound_going = true;
        let session = sample_session_with_layers(vec![layer(1, 0), layer(2, 1)]);
        assert_eq!(rt.route_audio(8_000_000, &session).active_audio_layer, Some(1));
        // Master 1 is 0..10 s, so it is active again at 9 s.
        assert_eq!(rt.route_audio(9_000_000, &session).active_audio_layer, Some(1));
    }

    #[test]
    fn copy_frame_reports_missing_frames_as_errors() {
        let rt = Runtime::new_silent();
        let err = rt.copy_frame_rgba(42, &CropRect::none()).unwrap_err();
        assert!(err.contains("layer not found"));
    }

    #[test]
    fn apply_preferences_propagates_keep_sound_going() {
        let mut rt = Runtime::new_silent();
        let mut prefs = Preferences::default();
        prefs.keep_sound_going = false;
        rt.apply_preferences(&prefs);
        assert!(!rt.keep_sound_going);
    }

    #[test]
    fn step_one_frame_all_moves_the_clock_by_one_frame() {
        let mut rt = runtime_with_durations(&[(1, 10_000_000)]);
        rt.clock.set_position_us(1_000_000);
        rt.step_one_frame_all(true);
        assert_eq!(rt.clock.position_us(), 1_000_000 + 33_333);
        rt.step_one_frame_all(false);
        // Clamped at zero.
        rt.clock.set_position_us(0);
        rt.step_one_frame_all(false);
        assert_eq!(rt.clock.position_us(), 0);
    }

    #[test]
    fn activity_respects_offsets() {
        let mut rt = runtime_with_durations(&[(1, 5_000_000)]);
        rt.set_offset(1, 2_000_000);
        // Span is [-2s, 3s].
        assert!(rt.is_active(1, -1_000_000));
        assert!(!rt.is_active(1, -3_000_000));
        assert!(!rt.is_active(1, 4_000_000));
        let _ = layer_active;
    }
}
