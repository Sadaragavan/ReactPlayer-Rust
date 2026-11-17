//! The runtime: everything that turns a session into pictures and sound.
//!
//! The app owns the [`Session`] (plain data); this owns the tracks, the audio
//! device and the clock, and is driven once per frame from the UI thread. All
//! the sync decisions from `model::timeline` are applied here:
//!
//! * `target_pts_us = clock + offset_us` decides which frame a layer shows,
//! * drift under 5 ms is ignored, 5..100 ms is absorbed (frames held or dropped
//!   for picture, a rate nudge for sound) and over 100 ms seeks,
//! * the union timeline decides where each layer starts and ends, and playback
//!   stops when every layer is past its end.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::model::session::{now_ms, Layer, Session};
use crate::model::settings::Settings;
use crate::model::timeline::{self, DriftClass, Union};

use super::audio::{AudioEngine, AudioSlot, AudioStatus, MAX_TRACKS};
use super::hwaccel::HwBackend;
use super::sync_clock::MasterClock;
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
    /// Epoch the accepted frames carry.
    epoch: u64,
    /// Set when a seek was sent but no frame with the new epoch has arrived.
    awaiting_epoch: bool,
    /// Last presented timestamp, in microseconds of the source.
    presented_us: i64,
    /// Target the last seek aimed at, so we do not re-seek every frame.
    last_seek_us: i64,
    last_seek_ms: i64,
    /// Set after a seek until drift is small again (hysteresis).
    seeking: bool,
    /// Last rate factor sent to the audio worker.
    last_rate: f64,
    /// Whether the worker was last told to play, so commands only go out on a
    /// transition instead of every tick.
    worker_playing: bool,
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
            awaiting_epoch: true,
            presented_us: 0,
            last_seek_us: i64::MIN,
            last_seek_ms: 0,
            seeking: true,
            last_rate: 1.0,
            worker_playing: false,
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
    slots: HashMap<String, usize>,
    used_slots: Vec<bool>,
    playing: bool,
    /// The layer the UI has focused; in unlinked mode its sound is the one that
    /// plays.
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
            master_volume: 1.0,
            device_rate,
        };
        runtime.set_master_volume(settings.master_volume);
        runtime
    }

    pub fn audio_status(&self) -> AudioStatus {
        self.audio.status().clone()
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
            .or_else(|| state.shared.as_ref().and_then(|shared| shared.history_first()))
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
            .or_else(|| state.shared.as_ref().and_then(|shared| shared.error()))
    }

    /// True until the probe lands, so the layer row can say "opening".
    pub fn is_probing(&self, layer_id: &str) -> bool {
        self.layers
            .get(layer_id)
            .and_then(|state| state.shared.as_ref())
            .map(|shared| shared.info().is_none() && shared.error().is_none())
            .unwrap_or(true)
    }

    /// Open tracks for new layers and close tracks for layers that are gone.
    /// Returns the probe results that arrived, so the app can write durations,
    /// frame rates and audio flags back into the session.
    pub fn sync_session(
        &mut self,
        session: &Session,
        settings: &Settings,
    ) -> Vec<(String, MediaInfo)> {
        let ids: Vec<String> = session.layers.iter().map(|layer| layer.id.clone()).collect();
        self.layers.retain(|id, _| {
            let keep = ids.iter().any(|kept| kept == id);
            if !keep
                && let Some(slot) = self.slots.remove(id) {
                self.used_slots[slot] = false;
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
                let track = Track::spawn(TrackConfig {
                    source: layer.source.clone(),
                    hw_accel: settings.hw_accel,
                    audio_rate: self.device_rate,
                    audio_slot: slot.clone(),
                });
                let mut state = LayerState::new();
                state.epoch = track.shared.epoch();
                state.shared = Some(track.shared.clone());
                state.slot = slot;
                state.track = Some(track);
                self.layers.insert(layer.id.clone(), state);
            }
            if let Some(info) = self
                .layers
                .get(&layer.id)
                .and_then(|state| state.shared.as_ref())
                .and_then(|shared| shared.info())
            {
                probed.push((layer.id.clone(), info));
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
        let epoch = self.layers.get(&layer.id).map(|state| state.epoch);
        let Some(state) = self.layers.get_mut(&layer.id) else {
            return;
        };
        state.last_seek_us = target;
        state.last_seek_ms = now_ms();
        state.seeking = true;
        state.pending = None;
        state.awaiting_epoch = true;
        state.epoch = epoch.unwrap_or(state.epoch);
        if let Some(shared) = &state.shared {
            shared.clear_history();
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
                state.unlinked_playing = true;
                state.unlinked_last_tick = Some(Instant::now());
                if let Some(track) = &state.track {
                    track.try_send(Command::Play);
                }
            }
        }
        self.update_audio(session, settings);
    }

    pub fn pause(&mut self, session: &Session) {
        self.playing = false;
        self.clock.set_playing(false);
        for layer in &session.layers {
            if let Some(state) = self.layers.get(&layer.id)
                && let Some(track) = &state.track {
                track.try_send(Command::Pause);
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

    /// Per-track transport for unlinked timelines.
    pub fn set_unlinked_playing(&mut self, layer_id: &str, playing: bool) {
        if let Some(state) = self.layers.get_mut(layer_id) {
            state.unlinked_playing = playing;
            state.unlinked_last_tick = Some(Instant::now());
            if let Some(track) = &state.track {
                if playing {
                    track.try_send(Command::Play);
                } else {
                    track.try_send(Command::Pause);
                }
            }
        }
    }

    pub fn seek_unlinked(&mut self, session: &Session, layer_id: &str, local_us: i64) {
        let Some(layer) = session.layers.iter().find(|layer| layer.id == layer_id) else {
            return;
        };
        let target = timeline::clamp_pts_us(local_us, layer.duration_us.max(1));
        if let Some(state) = self.layers.get_mut(layer_id) {
            state.unlinked_us = target;
            state.unlinked_last_tick = Some(Instant::now());
            state.pending = None;
            state.awaiting_epoch = true;
            if let Some(shared) = &state.shared {
                shared.clear_history();
            }
            if let Some(track) = &state.track {
                track.send(Command::Seek { target_us: target });
            }
        }
    }

    /// The decoded frames around a layer's current position, oldest first.
    pub fn history(&self, layer_id: &str) -> Vec<Arc<VideoFrame>> {
        self.layers
            .get(layer_id)
            .and_then(|state| state.shared.as_ref())
            .map(|shared| shared.history_snapshot())
            .unwrap_or_default()
    }

    /// Step a single layer while the timelines are unlinked, which is how the
    /// alignment studio walks one side of the comparison.
    pub fn step_unlinked(&mut self, session: &Session, layer_id: &str, steps: i64) {
        let Some(layer) = session.layers.iter().find(|layer| layer.id == layer_id) else {
            return;
        };
        let current = self
            .layers
            .get(layer_id)
            .map(|state| state.unlinked_us)
            .unwrap_or_else(|| self.presented_us(layer_id));
        let next = timeline::step_pts_us(current, layer.frame_us(), steps, layer.duration_us.max(1));
        self.seek_unlinked(session, layer_id, next);
    }

    /// Step exact frames. Backward steps come out of the decoded history, so
    /// they are instant and never round to a keyframe.
    pub fn step(&mut self, session: &Session, steps: i64) {
        if steps == 0 {
            return;
        }
        if self.playing {
            self.pause(session);
        }
        // The focused layer (or the master) decides how long a frame is.
        let reference = self
            .focused
            .as_ref()
            .and_then(|id| session.layers.iter().find(|layer| &layer.id == id))
            .or_else(|| session.master())
            .or_else(|| session.layers.first());
        let frame_us = reference.map(|layer| layer.frame_us()).unwrap_or(33_333);

        for layer in &session.layers {
            let Some(state) = self.layers.get_mut(&layer.id) else {
                continue;
            };
            if steps < 0
                && let Some(shared) = &state.shared
                && let Some(frame) = shared.frame_before(state.presented_us, frame_us)
            {
                state.presented_us = frame.pts_us;
                state.presented = Some(frame);
                state.pending = None;
                continue;
            }
            if let Some(track) = &state.track {
                track.try_send(Command::Step { delta: steps });
            }
        }

        let union = session.union();
        let position = self.position_us() + steps.saturating_mul(frame_us);
        self.clock
            .set_position_us(position.clamp(union.start_us, union.end_us));
    }

    /// One frame of the app: drain queues, pick frames, keep sound in step.
    pub fn tick(&mut self, session: &Session, settings: &Settings) -> Tick {
        let union = session.union();
        if self.playing && union.is_finished(self.clock.position_us()) {
            self.pause(session);
        }

        let now_us = self.clock.position_us();
        let audio_live = self.playing
            && self.audio.status().is_running()
            && self.layers.values().any(|state| {
                state
                    .slot
                    .as_ref()
                    .map(|slot| slot.is_active())
                    .unwrap_or(false)
            });
        self.clock.set_audio_live(audio_live);

        for layer in &session.layers {
            self.advance_layer(layer, session, now_us);
        }
        self.update_audio(session, settings);

        Tick {
            position_us: self.clock.position_us(),
            playing: self.playing,
            union,
        }
    }

    fn advance_layer(&mut self, layer: &Layer, session: &Session, now_us: i64) {
        let duration = layer.duration_us.max(1);
        let target = if session.is_linked {
            now_us + layer.offset_us
        } else {
            // Unlinked: the layer runs on its own position, advanced here.
            let Some(state) = self.layers.get_mut(&layer.id) else {
                return;
            };
            let now = Instant::now();
            if state.unlinked_playing
                && let Some(last) = state.unlinked_last_tick {
                let elapsed = now.saturating_duration_since(last).as_micros() as i64;
                state.unlinked_us = (state.unlinked_us + elapsed).min(duration - 1);
            }
            state.unlinked_last_tick = Some(now);
            state.unlinked_us + layer.offset_us
        };

        let playing = self.playing;
        let Some(state) = self.layers.get_mut(&layer.id) else {
            return;
        };
        let LayerState {
            track,
            shared,
            presented,
            pending,
            epoch,
            awaiting_epoch,
            presented_us,
            last_seek_us,
            last_seek_ms,
            seeking,
            last_rate,
            worker_playing,
            ..
        } = state;

        let inside = target >= 0 && target < duration;
        // Only decode while there is something to show; outside its footage a
        // layer holds a frame for free.
        let want_worker = playing && inside;
        if *worker_playing != want_worker {
            *worker_playing = want_worker;
            if let Some(track) = track.as_ref() {
                if want_worker {
                    track.try_send(Command::Play);
                } else {
                    track.try_send(Command::Pause);
                }
            }
        }

        if !inside {
            if presented.is_none() {
                *presented = shared.as_ref().and_then(|shared| shared.history_first());
            }
            if let Some(frame) = presented.as_ref() {
                *presented_us = frame.pts_us;
            }
            return;
        }

        let half_frame = layer.frame_us() / 2;

        // A frame that was already too new for the previous tick.
        if let Some(frame) = pending.as_ref()
            && frame.pts_us <= target + half_frame {
                *presented = Some(frame.clone());
                *presented_us = frame.pts_us;
                *pending = None;
            }

        let mut accept_epoch = *epoch;
        let mut awaiting = *awaiting_epoch;
        while pending.is_none() {
            let Some((frame_epoch, frame)) = track.as_mut().and_then(|track| track.try_frame())
            else {
                break;
            };
            if awaiting {
                accept_epoch = frame_epoch;
                awaiting = false;
            }
            if frame_epoch != accept_epoch {
                // Left over from before a seek.
                continue;
            }
            if frame.pts_us <= target + half_frame {
                *presented_us = frame.pts_us;
                *presented = Some(frame);
            } else {
                *pending = Some(frame);
            }
        }
        *epoch = accept_epoch;
        *awaiting_epoch = awaiting;

        if let Some(frame) = presented.as_ref() {
            *presented_us = frame.pts_us;
        }

        let drift = *presented_us - target;
        let mut seek_now = false;
        let mut rate = 1.0;
        match timeline::classify_drift(drift) {
            DriftClass::Ignore => *seeking = false,
            DriftClass::Nudge(nudge) => {
                rate = nudge;
                if drift.abs() < timeline::SEEK_HYSTERESIS_US {
                    *seeking = false;
                }
            }
            DriftClass::Seek => {
                let now = now_ms();
                let cooled = now - *last_seek_ms > SEEK_BACKOFF_MS;
                // Hysteresis: never chase the same target twice in a row.
                if (cooled || !*seeking) && *last_seek_us != target {
                    seek_now = true;
                    *seeking = true;
                    *last_seek_us = target;
                    *last_seek_ms = now;
                    *pending = None;
                    *awaiting_epoch = true;
                }
            }
        }

        if seek_now {
            if let Some(shared) = shared.as_ref() {
                shared.clear_history();
            }
            if let Some(track) = track.as_ref() {
                let clamped = timeline::clamp_pts_us(target.max(0), duration);
                track.send(Command::Seek {
                    target_us: clamped.max(0),
                });
            }
        }

        if playing && (rate - *last_rate).abs() > 1e-4 {
            *last_rate = rate;
            if let Some(track) = track.as_ref() {
                track.try_send(Command::SetRate(rate));
            }
        }
        if let Some(shared) = shared.as_ref() {
            shared.note_presented(*presented_us);
        }
    }

    /// Decide which layers may be heard, and hand that to the mixer. Every
    /// unmuted layer with footage on screen mixes in on its own volume; when the
    /// master has nothing on screen and "keep the sound going" is on, the
    /// highest-priority active layer is unmuted instead.
    fn update_audio(&mut self, session: &Session, settings: &Settings) {
        let now = self.clock.position_us();
        let master = session.master().map(|layer| layer.id.clone());
        let master_ok = master
            .as_ref()
            .map(|id| {
                session
                    .layer(id)
                    .map(|layer| layer.is_active_at(now) && !layer.muted)
                    .unwrap_or(false)
            })
            .unwrap_or(true);
        let lender = if !master_ok && settings.keep_sound_going {
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
            let lending = lender.as_deref() == Some(layer.id.as_str());
            let muted = layer.muted && !lending;
            let unlinked_ok = session.is_linked
                || self.focused.as_deref() == Some(layer.id.as_str())
                || state.unlinked_playing;
            let active = self.playing && layer.is_active_at(now) && !muted && unlinked_ok;
            slot.set_volume(layer.volume);
            slot.set_muted(muted);
            slot.set_active(active);
        }
    }

    /// Silence everything, e.g. when leaving the player.
    pub fn stop(&mut self, session: &Session) {
        self.pause(session);
    }

    /// Drop a layer's decoder and free its audio slot, so the next
    /// [`Self::sync_session`] opens the file again. Used when a session points a
    /// layer at a different file: the worker holds the old path, and only a
    /// fresh one will pick the new up.
    pub fn forget_layer(&mut self, layer_id: &str) {
        if let Some(state) = self.layers.remove(layer_id)
            && let Some(track) = state.track.as_ref() {
            track.try_send(Command::Shutdown);
        }
        if let Some(slot) = self.slots.remove(layer_id) {
            self.used_slots[slot] = false;
            self.audio.silence(slot);
        }
    }

    /// True when at least one layer has a decoder open, i.e. whether the player
    /// has anything to draw.
    pub fn has_media(&self) -> bool {
        self.layers
            .values()
            .any(|state| state.shared.is_some())
    }

    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Tracks are dropped with the map, which joins their threads.
        self.audio.silence_all();
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
        assert_eq!(runtime.layer_count(), 0);
        assert!(!runtime.has_media());
    }

    #[test]
    fn seeking_clamps_into_the_union() {
        let mut runtime = Runtime::new(&Settings::default());
        let mut session = Session::new("t");
        let mut a = silent_layer("a", 0);
        a.duration_us = 5_000_000;
        session.layers.push(a);
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
    fn tick_reports_the_union_and_does_not_play_past_the_end() {
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
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            runtime.error(&session.layers[0].id).is_some(),
            "expected a decode error"
        );
        assert_eq!(
            runtime.backend(&session.layers[0].id),
            HwBackend::Software
        );
        runtime.stop(&session);
    }

    #[test]
    fn stepping_moves_the_clock_by_whole_frames() {
        let mut runtime = Runtime::new(&Settings::default());
        let mut session = Session::new("t");
        let mut a = silent_layer("a", 0);
        a.fps_num = 30;
        a.fps_den = 1;
        a.duration_us = 4_000_000;
        session.layers.push(a);
        runtime.seek_union(&session, 1_000_000);
        runtime.step(&session, 1);
        assert_eq!(runtime.position_us(), 1_033_333);
        runtime.step(&session, -3);
        assert_eq!(runtime.position_us(), 933_334);
    }

    #[test]
    fn unlinked_layers_keep_their_own_position() {
        let mut runtime = Runtime::new(&Settings::default());
        let mut session = Session::new("t");
        session.is_linked = false;
        let mut a = silent_layer("a", 0);
        a.duration_us = 4_000_000;
        let id = a.id.clone();
        session.layers.push(a);
        runtime.set_unlinked_playing(&id, true);
        runtime.seek_unlinked(&session, &id, 2_000_000);
        // The shared clock is untouched by a per-track seek.
        assert_eq!(runtime.position_us(), 0);
    }
}
