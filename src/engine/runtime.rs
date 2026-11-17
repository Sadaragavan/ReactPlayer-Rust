//! The runtime: everything that turns a session into pictures and sound.
//!
//! The app owns the [`Session`] (plain data); this owns the tracks, the audio
//! device and the clock, and is driven once per frame from the UI thread.
//! All the sync decisions from `model::timeline` are applied here:
//!
//! * `target_pts_us = clock + offset_us` decides which frame a layer shows,
//! * drift under 5 ms is ignored, 5..100 ms is absorbed (frames held or
//!   dropped for picture, a rate nudge for sound) and over 100 ms seeks,
//! * the union timeline decides where each layer starts and ends, and playback
//!   stops when every layer is past its end.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::model::session::{Layer, Session};
use crate::model::settings::Settings;
use crate::model::timeline::{self, DriftClass, Target, Union};
use crate::model::session::now_ms;

use super::audio::{AudioEngine, AudioSlot, AudioStatus, MAX_TRACKS};
use super::hwaccel::HwBackend;
use super::sync_clock::{MasterClock, MAX_INTERPOLATION_US};
use super::track::{Command, MediaInfo, Track, TrackConfig, TrackShared};
use super::video_frame::VideoFrame;

/// How long to wait between seeks for the same layer.
const SEEK_BACKOFF_MS: i64 = 250;

/// Everything the UI needs to know after a tick.
#[derive(Clone, Copy, Debug)]
pub struct Tick {
    pub position_us: i64,
    pub playing: bool,
    pub union: Union,
}

/// One layer's playback state.
struct LayerState {
    track: Option<Track>,
    shared: Option<Arc<TrackShared>>,
    slot: Option<Arc<AudioSlot>>,
    /// The frame on screen.
    presented: Option<Arc<VideoFrame>>,
    /// A decoded frame that is ahead of the clock, kept for the next tick.
    pending: Option<Arc<VideoFrame>>,
    /// Epoch of the frames currently accepted.
    epoch: u64,
    /// Last presented timestamp, in microseconds of the source.
    presented_us: i64,
    /// Target the last seek aimed at, so we do not re-seek every frame.
    last_seek_us: i64,
    last_seek_ms: i64,
    /// Set after a seek until drift is small again (hysteresis).
    seeking: bool,
    error: Option<String>,
    // Unlinked mode.
    unlinked_us: i64,
    unlinked_playing: bool,
    unlinked_last_tick: Option<Instant>,
}

impl LayerState {
    fn new() -> Self {
        Self {
            track: None,
            shared: None,
            slot: None,
            presented: None,
            pending: None,
            epoch: 0,
            presented_us: 0,
            last_seek_us: 0,
            last_seek_ms: 0,
            seeking: false,
            error: None,
            unlinked_us: 0,
            unlinked_playing: false,
            unlinked_last_tick: None,
        }
    }
}

/// The engine side of the app.
pub struct Runtime {
    pub clock: Arc<MasterClock>,
    audio: AudioEngine,
    layers: HashMap<String, LayerState>,
    /// Which audio slot each layer got.
    slots: HashMap<String, usize>,
    used_slots: Vec<bool>,
    playing: bool,
    /// The layer the UI has focused, used to pick whose sound plays when the
    /// timelines are unlinked.
    focused: Option<String>,
    master_volume: f32,
    device_rate: u32,
}

impl Runtime {
    pub fn new(settings: &Settings) -> Self {
        let clock = MasterClock::new();
        let audio = AudioEngine::start(clock.clone());
        let device_rate = audio.device_rate();
        let mut runtime = Self {
            clock,
            audio,
            layers: HashMap::new(),
            slots: HashMap::new(),
            used_slots: vec![false; MAX_TRACKS],
            playing: false,
            focused: None,
            master_volume: settings.master_volume,
            device_rate,
        };
        runtime.set_master_volume(settings.master_volume);
        runtime
    }

    pub fn audio_status(&self) -> &AudioStatus {
        self.audio.status()
    }

    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn position_us(&self) -> i64 {
        self.clock.position_us()
    }

    pub fn set_master_volume(&mut self, volume: f32) {
        self.master_volume = volume.clamp(0.0, 1.0);
        self.audio.set_master_volume(self.master_volume);
    }

    pub fn focused(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    pub fn set_focused(&mut self, id: Option<String>) {
        self.focused = id;
    }

    /// The frame a layer is currently showing.
    pub fn frame(&self, layer_id: &str) -> Option<Arc<VideoFrame>> {
        let state = self.layers.get(layer_id)?;
        state
            .presented
            .clone()
            .or_else(|| state.shared.as_ref().and_then(|s| s.history_first()))
    }

    pub fn presented_us(&self, layer_id: &str) -> i64 {
        self.layers
            .get(layer_id)
            .map(|state| state.presented_us)
            .unwrap_or(0)
    }

    pub fn backend(&self, layer_id: &str) -> HwBackend {
        self.layers
            .get(layer_id)
            .and_then(|state| state.shared.as_ref())
            .map(|shared| shared.backend())
            .unwrap_or(HwBackend::Software)
    }

    pub fn converted(&self, layer_id: &str) -> bool {
        self.layers
            .get(layer_id)
            .and_then(|state| state.shared.as_ref())
            .map(|shared| shared.converted())
            .unwrap_or(false)
    }

    pub fn error(&self, layer_id: &str) -> Option<String> {
        let state = self.layers.get(layer_id)?;
        state
            .error
            .clone()
            .or_else(|| state.shared.as_ref().and_then(|s| s.error()))
    }

    pub fn is_probing(&self, layer_id: &str) -> bool {
        self.layers
            .get(layer_id)
            .and_then(|state| state.shared.as_ref())
            .map(|shared| shared.info().is_none() && shared.error().is_none())
            .unwrap_or(true)
    }

    /// Open tracks for new layers and close tracks for layers that are gone.
    /// Returns any probe results that arrived, so the app can write durations,
    /// frame rates and audio flags back into the session.
    pub fn sync_session(&mut self, session: &Session, settings: &Settings) -> Vec<(String, MediaInfo)> {
        let ids: Vec<String> = session.layers.iter().map(|l| l.id.clone()).collect();
        self.layers.retain(|id, _| {
            let keep = ids.contains(id);
            if !keep {
                if let Some(slot) = self.slots.remove(id) {
                    self.used_slots[slot] = false;
                }
            }
            keep
        });

        let mut probed = Vec::new();
        for layer in &session.layers {
            if !self.layers.contains_key(&layer.id) {
                let slot_index = self.allocate_slot();
                let slot = slot_index.and_then(|index| self.audio.slot(index).cloned());
                if let (Some(index), Some(_)) = (slot_index, slot.as_ref()) {
                    self.slots.insert(layer.id.clone(), index);
                }
                let config = TrackConfig {
                    source: layer.source.clone(),
                    hw_accel: settings.hw_accel,
                    audio_rate: self.device_rate,
                    audio_slot: slot.clone(),
                };
                let track = Track::spawn(config);
                let mut state = LayerState::new();
                state.shared = Some(track.shared.clone());
                state.slot = slot;
                state.epoch = track.shared.epoch();
                state.track = Some(track);
                state.unlinked_us = 0;
                self.layers.insert(layer.id.clone(), state);
            }
            if let Some(state) = self.layers.get(&layer.id) {
                if let Some(shared) = &state.shared {
                    if let Some(info) = shared.info() {
                        probed.push((layer.id.clone(), info));
                    }
                }
            }
        }
        probed
    }

    fn allocate_slot(&mut self) -> Option<usize> {
        for (index, used) in self.used_slots.iter_mut().enumerate() {
            if !*used {
                *used = true;
                return Some(index);
            }
        }
        None
    }

    /// Jump to a position on the union timeline.
    pub fn seek_union(&mut self, session: &Session, t_us: i64) {
        let union = session.union();
        let clamped = t_us.clamp(union.start_us, union.end_us);
        self.clock.set_position_us(clamped);
        for layer in &session.layers {
            self.seek_layer(layer, clamped);
        }
    }

    /// Jump to a position shown on the scrubber (0 = union start).
    pub fn seek_display(&mut self, session: &Session, display_us: i64) {
        let union = session.union();
        self.seek_union(session, union.from_display_us(display_us));
    }

    fn seek_layer(&mut self, layer: &Layer, master_us: i64) {
        let target = timeline::target_pts_us(master_us, layer.offset_us);
        let Some(state) = self.layers.get_mut(&layer.id) else {
            return;
        };
        state.last_seek_us = target;
        state.last_seek_ms = now_ms();
        state.seeking = true;
        state.pending = None;
        if let Some(shared) = &state.shared {
            shared.clear_history();
            state.epoch = shared.epoch();
        }
        if let Some(track) = &state.track {
            let clamped = timeline::clamp_pts_us(target.max(0), layer.duration_us.max(1));
            track.send(Command::Seek {
                target_us: clamped.max(0),
            });
            if !self.playing {
                track.send(Command::Pause);
            }
        }
    }

    /// Re-seek every layer so a changed offset takes effect immediately.
    pub fn apply_offsets(&mut self, session: &Session) {
        let now = self.clock.stored_position_us();
        for layer in &session.layers {
            self.seek_layer(layer, now);
        }
    }

    pub fn play(&mut self, session: &Session, settings: &Settings) {
        let union = session.union();
        let position = self.clock.position_us();
        if union.is_finished(position) || position < union.start_us {
            self.seek_union(session, union.start_us);
        }
        self.playing = true;
        self.clock.set_playing(true);
        for layer in &session.layers {
            if let Some(state) = self.layers.get_mut(&layer.id) {
                state.unlinked_playing = session.is_linked || self.focused.as_deref() == Some(&layer.id);
                state.unlinked_last_tick = Some(Instant::now());
                state.seeking = true;
                if let Some(track) = &state.track {
                    track.send(Command::Play);
                }
            }
        }
        self.update_audio(session, settings);
    }

    pub fn pause(&mut self, session: &Session) {
        self.playing = false;
        self.clock.set_playing(false);
        for layer in &session.layers {
            if let Some(state) = self.layers.get(&layer.id) {
                if let Some(track) = &state.track {
                    track.send(Command::Pause);
                }
            }
        }
        self.audio.silence_all();
    }

    pub fn toggle(&mut self, session: &Session, settings: &Settings) {
        if self.playing {
            self.pause(session);
        } else {
            self.play(session, settings);
        }
    }

    /// Step exact frames from the current position. While paused this uses the
    /// decoded history for backward steps, so it is instant and never seeks to
    /// a keyframe.
    pub fn step(&mut self, session: &Session, settings: &Settings, steps: i64) {
        if steps == 0 {
            return;
        }
        let was_playing = self.playing;
        if was_playing {
            self.pause(session);
        }
        for layer in &session.layers {
            let frame_us = if layer.fps_num == 0 {
                33_333
            } else {
                layer.frame_us()
            };
            let Some(state) = self.layers.get_mut(&layer.id) else {
                continue;
            };
            if steps < 0 {
                if let Some(shared) = &state.shared {
                    if let Some(frame) = shared.frame_before(state.presented_us, frame_us) {
                        state.presented_us = frame.pts_us;
                        state.presented = Some(frame);
                        state.pending = None;
                        continue;
                    }
                }
            }
            if let Some(track) = &state.track {
                track.send(Command::Step { delta: steps });
            }
        }
        // Keep the clock in step with the pictures.
        let position = self.clock.position_us() + steps * 33_333;
        let union = session.union();
        self.clock
            .set_position_us(position.clamp(union.start_us, union.end_us));
        let _ = settings;
    }

    /// One frame of the app: drain queues, pick frames, keep sound in step.
    pub fn tick(&mut self, session: &Session, settings: &Settings) -> Tick {
        let union = session.union();
        if self.playing && union.is_finished(self.clock.position_us()) {
            self.pause(session);
        }

        let now_us = self.clock.position_us();
        // The audio device is the timebase only while something is audible.
        let any_audio = self.layers.values().any(|state| {
            state
                .slot
                .as_ref()
                .map(|slot| slot.is_active())
                .unwrap_or(false)
        });
        let audio_live = self.audio.status().is_running() && any_audio && self.playing;
        self.clock.set_audio_live(audio_live);

        for layer in &session.layers {
            self.advance_layer(layer, session, now_us);
        }

        // Unlinked layers run on their own clock.
        if !session.is_linked {
            for layer in &session.layers {
                if let Some(state) = self.layers.get_mut(&layer.id) {
                    state.unlinked_last_tick = Some(Instant::now());
                }
            }
        }

        self.update_audio(session, settings);

        Tick {
            position_us: self.clock.position_us(),
            playing: self.playing,
            union,
        }
    }

    fn advance_layer(&mut self, layer: &Layer, session: &Session, now_us: i64) {
        // Where this layer wants to be.
        let target = if session.is_linked {
            now_us + layer.offset_us
        } else {
            let state = match self.layers.get_mut(&layer.id) {
                Some(state) => state,
                None => return,
            };
            let now = Instant::now();
            if state.unlinked_playing {
                if let Some(last) = state.unlinked_last_tick {
                    let elapsed = now.saturating_duration_since(last).as_micros() as i64;
                    state.unlinked_us = (state.unlinked_us + elapsed)
                        .clamp(0, layer.duration_us.max(1));
                }
            }
            state.unlinked_last_tick = Some(now);
            state.unlinked_us + layer.offset_us
        };

        let Some(state) = self.layers.get_mut(&layer.id) else {
            return;
        };
        state.presented_us = state.presented_us.max(0);

        let inside = target >= 0 && target < layer.duration_us.max(1);
        if !inside {
            // Before the start hold the first frame, after the end the last one.
            if state.presented.is_none() {
                state.presented = state
                    .shared
                    .as_ref()
                    .and_then(|shared| shared.history_first());
            }
            if let Some(frame) = &state.presented {
                state.presented_us = frame.pts_us;
            }
            return;
        }

        let half_frame = layer.frame_us() / 2;
        let Some(track) = state.track.as_mut() else {
            return;
        };
        let shared = state.shared.clone();

        // A frame was already waiting from a previous tick.
        if let Some(pending) = &state.pending {
            if pending.pts_us <= target + half_frame {
                state.presented = Some(pending.clone());
                state.presented_us = pending.pts_us;
                state.pending = None;
            }
        }

        while state.pending.is_none() {
            let Some((epoch, frame)) = track.try_frame() else {
                break;
            };
            if epoch != state.epoch {
                // Left over from before a seek.
                continue;
            }
            if frame.pts_us <= target + half_frame {
                state.presented_us = frame.pts_us;
                state.presented = Some(frame);
            } else {
                state.pending = Some(frame);
            }
        }

        if let Some(frame) = &state.presented {
            state.presented_us = frame.pts_us;
        }

        let drift = state.presented_us - target;
        let mut seek_again = false;
        let mut nudge = 1.0;
        match timeline::classify_drift(drift) {
            DriftClass::Ignore => state.seeking = false,
            DriftClass::Nudge(rate) => {
                nudge = rate;
                if drift.abs() < timeline::SEEK_HYSTERESIS_US {
                    state.seeking = false;
                }
            }
            DriftClass::Seek => {
                let now = now_ms();
                let cooled = now - state.last_seek_ms > SEEK_BACKOFF_MS;
                // Hysteresis: do not start another seek for the same layer while
                // the last one has not settled.
                if (cooled || !state.seeking) && state.last_seek_us != target {
                    seek_again = true;
                    state.seeking = true;
                    state.last_seek_us = target;
                    state.last_seek_ms = now;
                    state.pending = None;
                }
            }
        }

        if seek_again {
            if let Some(shared) = &shared {
                shared.clear_history();
                state.epoch = shared.epoch().wrapping_add(1);
            }
            let clamped = timeline::clamp_pts_us(target.max(0), layer.duration_us.max(1));
            track.send(Command::Seek {
                target_us: clamped.max(0),
            });
            if !self.playing {
                track.send(Command::Pause);
            }
        }

        let _ = nudge;
        if self.playing {
            let nudge_for_audio = nudge;
            track.send(Command::SetRate(nudge_for_audio));
        }
        if let Some(shared) = &shared {
            shared.note_presented(state.presented_us);
        }
    }

    /// Decide which layers may be heard, and hand that to the mixer.
    fn update_audio(&mut self, session: &Session, settings: &Settings) {
        let now = self.clock.position_us();
        let master = session.master().map(|layer| layer.id.clone());
        let master_ok = master.as_ref().is_none_or(|id| {
            session
                .layer(id)
                .map(|layer| layer.is_active_at(now) && !layer.muted)
                .unwrap_or(false)
        });
        let lender = if !master_ok && (settings.keep_sound_going || session.keep_sound_going) {
            session.lend_master(now).map(|layer| layer.id.clone())
        } else {
            None
        };

        for layer in &session.layers {
            let Some(state) = self.layers.get(&layer.id) else {
                continue;
            };
            let Some(slot) = &state.slot else {
                continue;
            };
            let is_master = master.as_deref() == Some(layer.id.as_str());
            let lending = lender.as_deref() == Some(layer.id.as_str());
            let in_footage = layer.is_active_at(now);
            let unlinked_ok = session.is_linked
                || self.focused.as_deref() == Some(layer.id.as_str())
                || state.unlinked_playing;
            let muted = layer.muted && !lending;
            let active = self.playing && in_footage && !muted && unlinked_ok && !layer.source.is_dir();
            // The master keeps its own sound; anything else mixes in on its own
            // volume, exactly like the mixer's contract says.
            let audible = if is_master || lending {
                active
            } else {
                active && !session.is_linked || (!session.is_linked && active)
            };
            slot.set_volume(layer.volume);
            slot.set_muted(muted);
            slot.set_active(audible);
        }
    }

    /// Silence everything, e.g. when leaving the player.
    pub fn stop(&mut self, session: &Session) {
        self.pause(session);
    }

    /// The longest the UI may interpolate before it stops trusting the anchor.
    pub fn interpolation_bound_us(&self) -> i64 {
        MAX_INTERPOLATION_US
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::geom::NormRect;
    use crate::model::session::{MediaSource, Role};

    fn silent_layer(name: &str, z: i32) -> Layer {
        let mut layer = Layer::new(
            name,
            Role::Reaction,
            MediaSource::file("/nonexistent/never-there.mp4"),
            NormRect::FULL,
            z,
        );
        layer.duration_us = 10_000_000;
        layer
    }

    #[test]
    fn runtime_starts_paused_and_empty() {
        let runtime = Runtime::new(&Settings::default());
        assert!(!runtime.is_playing());
        assert_eq!(runtime.position_us(), 0);
        assert!(runtime.focused().is_none());
    }

    #[test]
    fn seeking_clamps_into_the_union() {
        let mut runtime = Runtime::new(&Settings::default());
        let mut session = Session::new("t");
        let mut a = silent_layer("a", 0);
        a.duration_us = 5_000_000;
        session.layers.push(a);
        // Tracks cannot open (the files do not exist), but the clock still obeys
        // the union bounds.
        runtime.seek_union(&session, 99_000_000);
        assert_eq!(runtime.position_us(), 5_000_000);
        runtime.seek_union(&session, -99_000_000);
        assert_eq!(runtime.position_us(), 0);
    }

    #[test]
    fn offsets_move_the_union_bounds() {
        let mut runtime = Runtime::new(&Settings::default());
        let mut session = Session::new("t");
        let mut a = silent_layer("a", 0);
        a.duration_us = 10_000_000;
        a.offset_us = 1_500_000;
        session.layers.push(a);
        let union = session.union();
        assert_eq!(union.start_us, -1_500_000);
        assert_eq!(union.end_us, 8_500_000);
        runtime.seek_union(&session, 0);
        assert_eq!(runtime.position_us(), 0);
    }

    #[test]
    fn tick_reports_the_union_and_stops_at_the_end() {
        let mut runtime = Runtime::new(&Settings::default());
        let mut session = Session::new("t");
        let mut a = silent_layer("a", 0);
        a.duration_us = 2_000_000;
        session.layers.push(a);
        runtime.seek_union(&session, 2_000_000);
        let tick = runtime.tick(&session, &Settings::default());
        assert_eq!(tick.union.end_us, 2_000_000);
        assert!(!tick.playing);
    }

    #[test]
    fn missing_media_shows_up_as_an_error_not_a_panic() {
        let mut runtime = Runtime::new(&Settings::default());
        let mut session = Session::new("t");
        session.layers.push(silent_layer("gone", 0));
        runtime.sync_session(&session, &Settings::default());
        // Give the worker a moment to fail.
        std::thread::sleep(std::time::Duration::from_millis(80));
        let error = runtime.error(&session.layers[0].id);
        assert!(error.is_some(), "expected a decode error");
        assert_eq!(runtime.backend(&session.layers[0].id), HwBackend::Software);
        runtime.stop(&session);
    }

    #[test]
    fn unlinked_layers_keep_their_own_position() {
        let mut runtime = Runtime::new(&Settings::default());
        let mut session = Session::new("t");
        session.is_linked = false;
        let mut a = silent_layer("a", 0);
        a.duration_us = 4_000_000;
        session.layers.push(a);
        runtime.sync_session(&session, &Settings::default());
        runtime.seek_union(&session, 1_000_000);
        // With no track the position still follows the union clock.
        assert_eq!(runtime.position_us(), 1_000_000);
    }
}
