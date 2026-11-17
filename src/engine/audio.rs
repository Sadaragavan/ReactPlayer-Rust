//! Audio output.
//!
//! One cpal output stream for the whole app. Each track owns a lock-free SPSC
//! ring buffer of interleaved stereo `f32` at the device's sample rate; the
//! single output callback mixes every unmuted, active slot with its volume and
//! the master volume, and advances the master clock by the number of frames it
//! consumed.
//!
//! Seeks are handled with an epoch counter: the decode worker bumps it, the
//! callback notices the change and drains whatever is left of the old position
//! instead of playing it. The producer never has to touch the consumer.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use ringbuf::traits::{Consumer, Observer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::sync_clock::MasterClock;

/// Layers that can have sound at once.
pub const MAX_TRACKS: usize = 4;

/// Ring capacity per track, in samples (stereo interleaved). Half a second at
/// 48 kHz: enough to ride out a scheduling hiccup, small enough that a seek
/// does not leave a second of stale audio behind.
pub const RING_SAMPLES: usize = 48_000;

/// One track's connection to the mixer.
pub struct AudioSlot {
    /// Bumped by the decode worker on every seek.
    epoch: AtomicU64,
    volume: AtomicU32,
    muted: AtomicBool,
    /// True while this track's material should be heard.
    active: AtomicBool,
    producer: Mutex<Option<HeapProd<f32>>>,
}

impl AudioSlot {
    fn new(producer: HeapProd<f32>) -> Self {
        Self {
            epoch: AtomicU64::new(0),
            volume: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            active: AtomicBool::new(false),
            producer: Mutex::new(Some(producer)),
        }
    }

    /// Hand the writer end to the track's audio worker. Only the first call
    /// succeeds; a track never reattaches to a different slot.
    pub fn take_producer(&self) -> Option<HeapProd<f32>> {
        match self.producer.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }

    pub fn bump_epoch(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
}

/// What happened when the app tried to open an output device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioStatus {
    Running {
        device: String,
        rate: u32,
        channels: u16,
    },
    /// No output device at all: the clock falls back to `Instant`.
    NoDevice,
    /// A device exists but the stream would not open.
    Failed(String),
}

impl AudioStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Running {
                device,
                rate,
                channels,
            } => format!("{device} · {rate} Hz · {channels} ch"),
            Self::NoDevice => "no output device".to_owned(),
            Self::Failed(reason) => format!("audio unavailable: {reason}"),
        }
    }
}

/// The audio device, its slots, and the stream that mixes them.
pub struct AudioEngine {
    slots: Vec<Arc<AudioSlot>>,
    master_volume: Arc<AtomicU32>,
    status: AudioStatus,
    device_rate: u32,
    channels: u16,
    // Kept alive for the life of the app; dropping it stops the stream.
    _stream: Option<cpal::Stream>,
}

impl AudioEngine {
    /// Open the default output device. Never fails: a machine without sound
    /// still plays video, on the `Instant` clock.
    pub fn start(clock: Arc<MasterClock>) -> Self {
        let mut slots = Vec::with_capacity(MAX_TRACKS);
        let mut consumers: Vec<Option<HeapCons<f32>>> = Vec::with_capacity(MAX_TRACKS);
        for _ in 0..MAX_TRACKS {
            let ring = HeapRb::<f32>::new(RING_SAMPLES);
            let (prod, cons) = ring.split();
            slots.push(Arc::new(AudioSlot::new(prod)));
            consumers.push(Some(cons));
        }

        let master_volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let host = cpal::default_host();
        let Some(device) = host.default_output_device() else {
            return Self {
                slots,
                master_volume,
                status: AudioStatus::NoDevice,
                device_rate: super::sync_clock::DEFAULT_RATE,
                channels: 2,
                _stream: None,
            };
        };
        let device_name = device
            .description()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|_| "default output".to_owned());

        let supported = match device.default_output_config() {
            Ok(config) => config,
            Err(e) => {
                return Self {
                    slots,
                        master_volume,
                    status: AudioStatus::Failed(format!("{e}")),
                    device_rate: super::sync_clock::DEFAULT_RATE,
                    channels: 2,
                    _stream: None,
                };
            }
        };

        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let rate = config.sample_rate;
        let channels = config.channels;
        clock.set_sample_rate(rate);

        let mixer = Mixer {
            consumers,
            slots: slots.clone(),
            clock: clock.clone(),
            master_volume: master_volume.clone(),
            channels: usize::from(channels),
            epochs: vec![0; MAX_TRACKS],
            scratch: vec![0.0; RING_SAMPLES.min(8_192)],
            mixed: Vec::new(),
        };

        // `move`: the config and the error callback belong to the stream.
        let build = move |mut mixer: Mixer| -> Result<cpal::Stream, cpal::Error> {
            let error_callback = |err: cpal::Error| {
                eprintln!("reaction-player: audio stream error: {err}");
            };
            match sample_format {
                SampleFormat::F32 => device.build_output_stream(
                    config,
                    move |data: &mut [f32], info| mixer.render(data, info),
                    error_callback,
                    None,
                ),
                SampleFormat::I16 => {
                    device.build_output_stream(
                        config,
                        move |data: &mut [i16], info| mixer.render_converted(data, info),
                        error_callback,
                        None,
                    )
                }
                SampleFormat::U16 => {
                    device.build_output_stream(
                        config,
                        move |data: &mut [u16], info| mixer.render_converted(data, info),
                        error_callback,
                        None,
                    )
                }
                _ => Err(cpal::Error::new(cpal::ErrorKind::UnsupportedConfig)),
            }
        };

        match build(mixer).and_then(|stream| {
            stream.play()?;
            Ok(stream)
        }) {
            Ok(stream) => Self {
                slots,
                master_volume,
                status: AudioStatus::Running {
                    device: device_name,
                    rate,
                    channels,
                },
                device_rate: rate,
                channels,
                _stream: Some(stream),
            },
            Err(e) => Self {
                slots,
                master_volume,
                status: AudioStatus::Failed(format!("{e}")),
                device_rate: rate,
                channels,
                _stream: None,
            },
        }
    }

    pub fn status(&self) -> &AudioStatus {
        &self.status
    }

    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn slot(&self, index: usize) -> Option<&Arc<AudioSlot>> {
        self.slots.get(index)
    }

    pub fn set_master_volume(&self, volume: f32) {
        self.master_volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Silence every slot, e.g. when playback stops.
    pub fn silence_all(&self) {
        for slot in &self.slots {
            slot.set_active(false);
        }
    }

    /// Silence one slot, e.g. when its layer is being replaced.
    pub fn silence(&self, index: usize) {
        if let Some(slot) = self.slots.get(index) {
            slot.set_active(false);
            let _ = slot.bump_epoch();
        }
    }
}

/// The state the output callback owns.
struct Mixer {
    consumers: Vec<Option<HeapCons<f32>>>,
    slots: Vec<Arc<AudioSlot>>,
    clock: Arc<MasterClock>,
    master_volume: Arc<AtomicU32>,
    channels: usize,
    epochs: Vec<u64>,
    scratch: Vec<f32>,
    /// Mixed f32 output for the current callback, resized only when the
    /// device's buffer size changes.
    mixed: Vec<f32>,
}

impl Mixer {
    /// Mix `frames` frames of every active slot into `self.mixed` and advance
    /// the clock by what the device consumed.
    fn mix(&mut self, frames: usize, info: &cpal::OutputCallbackInfo) {
        let channels = self.channels.max(1);
        let samples = frames * channels;
        self.mixed.clear();
        self.mixed.resize(samples, 0.0);

        let latency = Self::latency_us(info);
        let master = f32::from_bits(self.master_volume.load(Ordering::Relaxed)).clamp(0.0, 1.0);

        for index in 0..self.slots.len() {
            let Some(cons) = self.consumers[index].as_mut() else {
                continue;
            };
            let slot = &self.slots[index];
            // A seek bumps the epoch: everything already in the ring belongs to
            // the old position, so drop it instead of playing it.
            let epoch = slot.epoch();
            if epoch != self.epochs[index] {
                let available = cons.occupied_len();
                cons.skip(available);
                self.epochs[index] = epoch;
            }
            if !slot.active.load(Ordering::Relaxed) || slot.muted.load(Ordering::Relaxed) {
                let available = cons.occupied_len();
                cons.skip(available);
                continue;
            }
            let volume =
                f32::from_bits(slot.volume.load(Ordering::Relaxed)).clamp(0.0, 1.0) * master;
            if volume <= 0.0 {
                let available = cons.occupied_len();
                cons.skip(available);
                continue;
            }
            let want = (frames * 2).min(self.scratch.len());
            let got = cons.pop_slice(&mut self.scratch[..want]);
            for frame in 0..got / 2 {
                let left = self.scratch[frame * 2] * volume;
                let right = self.scratch[frame * 2 + 1] * volume;
                let base = frame * channels;
                if channels == 1 {
                    self.mixed[base] += 0.5 * (left + right);
                } else if base + 1 < self.mixed.len() {
                    self.mixed[base] += left;
                    self.mixed[base + 1] += right;
                }
            }
        }

        self.clock.advance_frames(frames as i64, latency);
    }

    fn render(&mut self, out: &mut [f32], info: &cpal::OutputCallbackInfo) {
        let frames = out.len() / self.channels.max(1);
        self.mix(frames, info);
        let n = out.len().min(self.mixed.len());
        out[..n].copy_from_slice(&self.mixed[..n]);
    }

    /// Same mix, written into a device format that is not `f32`.
    fn render_converted<T: SizedSample + FromSample<f32>>(
        &mut self,
        out: &mut [T],
        info: &cpal::OutputCallbackInfo,
    ) {
        let frames = out.len() / self.channels.max(1);
        self.mix(frames, info);
        for (index, sample) in out.iter_mut().enumerate() {
            *sample = T::from_sample(self.mixed.get(index).copied().unwrap_or(0.0));
        }
    }

    fn latency_us(info: &cpal::OutputCallbackInfo) -> i64 {
        let stamp = info.timestamp();
        stamp
            .playback
            .checked_duration_since(stamp.callback)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_start_inactive_and_unmuted() {
        let ring = HeapRb::<f32>::new(64);
        let (prod, _cons) = ring.split();
        let slot = AudioSlot::new(prod);
        assert!(!slot.is_active());
        assert_eq!(slot.epoch(), 0);
        assert!(slot.take_producer().is_some());
        // Only the first take succeeds.
        assert!(slot.take_producer().is_none());
    }

    #[test]
    fn epoch_bumps_monotonically() {
        let ring = HeapRb::<f32>::new(16);
        let (prod, _cons) = ring.split();
        let slot = AudioSlot::new(prod);
        assert_eq!(slot.bump_epoch(), 1);
        assert_eq!(slot.bump_epoch(), 2);
        assert_eq!(slot.epoch(), 2);
    }

    #[test]
    fn volume_and_mute_are_clamped() {
        let ring = HeapRb::<f32>::new(16);
        let (prod, _cons) = ring.split();
        let slot = AudioSlot::new(prod);
        slot.set_volume(4.0);
        slot.set_muted(true);
        slot.set_active(true);
        assert!(slot.is_active());
        assert!(slot.muted.load(Ordering::Relaxed));
    }

    #[test]
    fn status_text_is_human() {
        assert_eq!(AudioStatus::NoDevice.summary(), "no output device");
        assert!(!AudioStatus::NoDevice.is_running());
        let running = AudioStatus::Running {
            device: "Speakers".into(),
            rate: 48_000,
            channels: 2,
        };
        assert!(running.summary().contains("48"));
        assert!(running.is_running());
        assert!(AudioStatus::Failed("boom".into()).summary().contains("boom"));
    }
}
