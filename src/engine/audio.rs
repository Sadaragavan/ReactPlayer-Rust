//! Audio: per-track lock-free SPSC ring buffers mixed inside a single cpal
//! output callback, plus the drift resampler.
//!
//! ## Design
//!
//! * The decode worker owns the **producer** side and pushes interleaved stereo
//!   `f32` [`PcmChunk`]s (already resampled to the device rate by swresample).
//! * The cpal callback owns the **consumer** side of every track. It pops
//!   slices, applies the fractional drift resampler, then mixes with per-track
//!   volume/mute and the master volume.
//! * On seek the callback drops every queued sample whose `epoch` is older than
//!   the current seek epoch. The consumer never gets cleared from the producer
//!   side -- the producer only ever writes.
//! * The master clock is advanced by the number of frames the callback
//!   consumed, with the cpal device-vs-callback timestamp delta as latency
//!   compensation.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::audio_resample::DriftResampler;
use super::sync_clock::{duration_us, MasterClock};

/// One chunk of interleaved stereo audio from a decode worker.
#[derive(Debug, Clone, PartialEq)]
pub struct PcmChunk {
    /// Media timestamp of the first frame, in microseconds.
    pub pts_us: i64,
    /// Seek epoch at push time. The callback drops chunks from older epochs.
    pub epoch: u64,
    /// Volume applied by the producer (kept for completeness; the callback
    /// applies the live volume instead).
    pub volume: f32,
    /// Interleaved stereo samples.
    pub samples: Vec<f32>,
}

/// The producer-side half of one track's audio.
pub struct AudioRing {
    /// `Vec<f32>` values travel through the ring as `f32` samples; chunks carry
    /// their own epoch/volume header inside the sample stream (see
    /// [`AudioRing::push_audio`]).
    producer: Mutex<Option<HeapProd<f32>>>,
    capacity_samples: usize,
    source_rate: AtomicU32,
    source_channels: AtomicU32,
    epoch: Arc<AtomicU64>,
    /// Sample-exact position of the next pushed sample, in media microseconds.
    next_pts_us: AtomicI64,
}

impl std::fmt::Debug for AudioRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioRing")
            .field("capacity_samples", &self.capacity_samples)
            .field("source_rate", &self.source_rate.load(Ordering::Relaxed))
            .finish()
    }
}

impl AudioRing {
    /// Create a producer ring with `rate` capacity worth of stereo samples
    /// (about 250 ms).
    pub fn new(rate: u32, channels: u16) -> Arc<Self> {
        let capacity = (rate as usize).saturating_mul(channels as usize) / 4;
        Arc::new(Self {
            producer: Mutex::new(None),
            capacity_samples: capacity.max(1024),
            source_rate: AtomicU32::new(rate),
            source_channels: AtomicU32::new(channels as u32),
            epoch: Arc::new(AtomicU64::new(0)),
            next_pts_us: AtomicI64::new(0),
        })
    }

    /// Update the stream's sample rate/channels (before audio flows).
    pub fn configure(&self, rate: u32, channels: u16) {
        self.source_rate.store(rate, Ordering::Relaxed);
        self.source_channels.store(channels as u32, Ordering::Relaxed);
    }

    /// Connect a consumer. Replaces (and drops) any previous consumer, which is
    /// what a device change does.
    pub fn attach_consumer(&self) -> AudioConsumer {
        let (prod, cons) = HeapRb::<f32>::new(self.capacity_samples).split();
        *self.producer.lock().unwrap_or_else(|p| p.into_inner()) = Some(prod);
        AudioConsumer {
            consumer: cons,
            epoch: Arc::clone(&self.epoch),
            resampler: DriftResampler::new(2),
            pending: Vec::new(),
            pending_epoch: u64::MAX,
            rate_bits: AtomicU32::new(1.0f32.to_bits()),
            volume_bits: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
            drops: 0,
            frames_played: 0,
        }
    }

    /// Current seek epoch.
    pub fn epoch(&self) -> &Arc<AtomicU64> {
        &self.epoch
    }

    /// Note a seek: subsequent pushes carry the new epoch and producers may
    /// reset their resamplers. The consumer side drops stale chunks by itself.
    pub fn mark_seek(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::Release);
        self.next_pts_us.store(0, Ordering::Relaxed);
    }

    /// Push a chunk. Never blocks: if the ring is full the oldest samples are
    /// dropped by dropping *new* data, which keeps the producer real-time safe.
    #[allow(clippy::cast_possible_truncation)]
    pub fn push_audio(&self, chunk: PcmChunk) {
        let mut guard = self.producer.lock().unwrap_or_else(|p| p.into_inner());
        let Some(prod) = guard.as_mut() else {
            return;
        };
        // The ring is a bare interleaved stereo `f32` stream; the seek epoch
        // is published through the shared atomic instead of a per-chunk header
        // (`mark_seek`). Partial writes are fine: the consumer simply reads
        // whatever is available and pads the rest with silence.
        let _ = prod.push_slice(&chunk.samples);
    }

    /// Number of stereo frames buffered (approximate, for the UI meter).
    pub fn buffered_frames(&self) -> usize {
        let guard = self.producer.lock().unwrap_or_else(|p| p.into_inner());
        match guard.as_ref() {
            Some(p) => p.occupied_len() / 2,
            None => 0,
        }
    }
}

/// Consumer-side handle for one track, owned by the mixer.
pub struct AudioConsumer {
    consumer: HeapCons<f32>,
    epoch: Arc<AtomicU64>,
    resampler: DriftResampler,
    pending: Vec<f32>,
    pending_epoch: u64,
    rate_bits: AtomicU32,
    volume_bits: AtomicU32,
    muted: AtomicBool,
    /// Count of samples discarded because of stale epochs.
    drops: u64,
    /// Stereo frames produced since the last reset.
    frames_played: u64,
}

impl AudioConsumer {
    /// Set the drift rate (clamped by the resampler).
    pub fn set_rate(&self, rate: f32) {
        self.rate_bits.store(rate.to_bits(), Ordering::Relaxed);
        // The resampler itself is only touched from the audio callback; the
        // stored ratio is applied there.
    }

    /// Set the per-track volume.
    pub fn set_volume(&self, volume: f32) {
        self.volume_bits
            .store(volume.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    /// Mute/unmute.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Samples dropped due to seeks.
    pub fn drops(&self) -> u64 {
        self.drops
    }

    /// Reset the played-frame counter (used after seeks).
    pub fn take_frames_played(&mut self) -> u64 {
        let n = self.frames_played;
        self.frames_played = 0;
        n
    }

    /// Pull `frames` stereo frames into `out` (interleaved, length
    /// `frames * 2`), applying the drift ratio. Missing data reads as silence.
    pub fn fill(&mut self, out: &mut [f32]) {
        let frames = out.len() / 2;
        for s in out.iter_mut() {
            *s = 0.0;
        }
        if frames == 0 {
            return;
        }
        self.resampler
            .set_ratio(f32::from_bits(self.rate_bits.load(Ordering::Relaxed)));

        // Steady state: the ring holds a bare interleaved sample stream. We
        // keep `frames + history` stereo frames of input available and then
        // pull exactly `frames` interpolated output frames.
        //
        // When the epoch changed since the last callback, everything queued
        // belongs to the old position and must be skipped *and counted*.
        let current_epoch = self.epoch.load(Ordering::Acquire);
        if current_epoch != self.pending_epoch {
            self.pending_epoch = current_epoch;
            let stale = self.consumer.occupied_len();
            if stale > 0 {
                let skipped = self.consumer.skip(stale);
                self.drops = self.drops.saturating_add(skipped as u64);
            }
            self.pending.clear();
            self.resampler.reset();
        }

        let needed = frames * 2 + 8;
        while self.pending.len() < needed {
            let want = needed - self.pending.len();
            let mut buf = vec![0.0f32; want];
            let n = self.consumer.pop_slice(&mut buf);
            if n == 0 {
                break;
            }
            self.pending.extend_from_slice(&buf[..n]);
        }

        let mut scratch = vec![0.0f32; frames * 2 + 16];
        let produced = self.resampler.process(&self.pending, &mut scratch);
        let vol = f32::from_bits(self.volume_bits.load(Ordering::Relaxed));
        let muted = self.muted.load(Ordering::Relaxed);
        let gain = if muted { 0.0 } else { vol };
        let n = produced.min(frames);
        for i in 0..n {
            out[i * 2] = scratch[i * 2] * gain;
            out[i * 2 + 1] = scratch[i * 2 + 1] * gain;
        }
        self.frames_played += n as u64;

        // Keep the tail that the interpolator still needs.
        if self.pending.len() > 8 {
            let keep_from = self.pending.len() - 8;
            self.pending.drain(0..keep_from);
        }
    }

}

/// Snapshot of one track for the mixer.
pub struct MixSource {
    /// The consumer to pull from.
    pub consumer: AudioConsumer,
    /// Layer id (for diagnostics).
    pub layer_id: u64,
}

/// The audio output device plus the mixer state shared with the callback.
pub struct AudioEngine {
    /// The cpal stream. Kept alive for as long as audio should play.
    stream: Option<cpal::Stream>,
    /// Device sample rate used for every track's resampler.
    pub sample_rate: u32,
    /// Master volume in `0..=1`.
    master_volume: Arc<AtomicU32>,
    /// Layer ids and their consumers, guarded because the callback needs them
    /// and the UI updates them when tracks are added/removed.
    sources: Arc<Mutex<Vec<MixSource>>>,
    /// True when a real output stream is running.
    pub device_available: bool,
    /// Master clock advanced by the callback.
    pub clock: Arc<MasterClock>,
    /// Last error reported by cpal.
    pub last_error: Arc<Mutex<Option<String>>>,
    /// Number of stereo frames the callback has produced.
    pub frames_out: Arc<AtomicU64>,
}

impl AudioEngine {
    /// An engine with no device at all. The clock then runs on `Instant`.
    pub fn silent() -> Self {
        Self::try_build(Arc::new(MasterClock::new()), false)
    }

    /// An engine that opens the default output device (falling back to a
    /// silent engine when no device exists).
    ///
    /// `clock` is the engine's master clock: the output callback is what
    /// advances it, so the caller and the callback must share one instance.
    pub fn with_device(clock: Arc<MasterClock>) -> Self {
        Self::try_build(clock, true)
    }

    /// Try to open the default output device. Always succeeds: when no device
    /// exists the engine runs without a stream and the clock falls back to
    /// `Instant`.
    pub fn new(clock: Arc<MasterClock>) -> Self {
        Self::try_build(clock, true)
    }

    fn try_build(clock: Arc<MasterClock>, use_device: bool) -> Self {
        if !use_device {
            clock.set_audio_driven(false);
            return Self {
                stream: None,
                sample_rate: 48_000,
                master_volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
                sources: Arc::new(Mutex::new(Vec::new())),
                device_available: false,
                clock,
                last_error: Arc::new(Mutex::new(None)),
                frames_out: Arc::new(AtomicU64::new(0)),
            };
        }
        let master_volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let sources: Arc<Mutex<Vec<MixSource>>> = Arc::new(Mutex::new(Vec::new()));
        let last_error = Arc::new(Mutex::new(None));
        let frames_out = Arc::new(AtomicU64::new(0));

        match Self::try_open(
            Arc::clone(&clock),
            Arc::clone(&master_volume),
            Arc::clone(&sources),
            Arc::clone(&last_error),
            Arc::clone(&frames_out),
        ) {
            Ok((stream, rate)) => {
                clock.set_audio_driven(true);
                Self {
                    stream: Some(stream),
                    sample_rate: rate,
                    master_volume,
                    sources,
                    device_available: true,
                    clock,
                    last_error,
                    frames_out,
                }
            }
            Err(e) => {
                clock.set_audio_driven(false);
                if let Ok(mut g) = last_error.lock() {
                    *g = Some(e.clone());
                }
                Self {
                    stream: None,
                    sample_rate: 48_000,
                    master_volume,
                    sources,
                    device_available: false,
                    clock,
                    last_error,
                    frames_out,
                }
            }
        }
    }

    fn try_open(
        clock: Arc<MasterClock>,
        master_volume: Arc<AtomicU32>,
        sources: Arc<Mutex<Vec<MixSource>>>,
        last_error: Arc<Mutex<Option<String>>>,
        frames_out: Arc<AtomicU64>,
    ) -> Result<(cpal::Stream, u32), String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default output device".to_string())?;
        let config = device
            .default_output_config()
            .map_err(|e| format!("default_output_config: {e}"))?;
        let sample_format = config.sample_format();
        if sample_format != cpal::SampleFormat::F32 {
            // The mixer produces f32; a device that only offers another format
            // would need a conversion layer we do not pretend to have.
            return Err(format!(
                "default output device uses {sample_format:?} samples; reaction-player mixes in f32"
            ));
        }
        // `SampleRate` has been a plain `u32` alias since 0.16.
        let sample_rate = config.sample_rate();
        let channels = config.channels() as usize;
        let stream_config = config.config();
        let err_flag = Arc::clone(&last_error);
        let clock_cb = Arc::clone(&clock);
        let sources_cb = Arc::clone(&sources);
        let master_cb = Arc::clone(&master_volume);
        let frames_cb = Arc::clone(&frames_out);
        let rate = sample_rate;

        let stream = device
            .build_output_stream(
                stream_config,
                // The info type is inferred: naming it would pin us to a path
                // that moved between cpal releases.
                move |data: &mut [f32], info: &_| {
                    let frames = data.len() / channels;
                    let ts = info.timestamp();
                    // Output latency: the predicted DAC instant minus the
                    // instant the callback ran. cpal calls the former
                    // `playback` for output streams (`device` is the input
                    // counterpart). A host that reports no ordering yields
                    // zero rather than a panic.
                    let latency_us = duration_us(
                        ts.playback
                            .checked_duration_since(ts.callback)
                            .unwrap_or(Duration::ZERO),
                    );

                    let start = clock_cb.position_us();
                    mix(
                        data,
                        channels,
                        &sources_cb,
                        f32::from_bits(master_cb.load(Ordering::Relaxed)),
                    );
                    clock_cb.publish_audio_anchor(start, latency_us, frames as u32, rate);
                    frames_cb.fetch_add(frames as u64, Ordering::Relaxed);
                },
                move |err| {
                    if let Ok(mut g) = err_flag.lock() {
                        *g = Some(format!("audio stream error: {err}"));
                    }
                },
                None,
            )
            .map_err(|e| format!("build_output_stream: {e}"))?;
        stream.play().map_err(|e| format!("stream play: {e}"))?;
        Ok((stream, sample_rate))
    }

    /// Add a track's consumer to the mixer. Call before the track produces.
    pub fn add_source(&self, layer_id: u64, consumer: AudioConsumer) {
        if let Ok(mut g) = self.sources.lock() {
            g.push(MixSource {
                consumer,
                layer_id,
            });
        }
    }

    /// Remove a track from the mixer.
    pub fn remove_source(&self, layer_id: u64) {
        if let Ok(mut g) = self.sources.lock() {
            g.retain(|s| s.layer_id != layer_id);
        }
    }

    /// Set the per-track drift rate.
    pub fn set_track_rate(&self, layer_id: u64, rate: f32) {
        if let Ok(g) = self.sources.lock()
            && let Some(s) = g.iter().find(|s| s.layer_id == layer_id)
        {
            s.consumer.set_rate(rate);
        }
    }

    /// Set the per-track volume.
    pub fn set_track_volume(&self, layer_id: u64, volume: f32) {
        if let Ok(g) = self.sources.lock()
            && let Some(s) = g.iter().find(|s| s.layer_id == layer_id)
        {
            s.consumer.set_volume(volume);
        }
    }

    /// Set the per-track mute.
    pub fn set_track_muted(&self, layer_id: u64, muted: bool) {
        if let Ok(g) = self.sources.lock()
            && let Some(s) = g.iter().find(|s| s.layer_id == layer_id)
        {
            s.consumer.set_muted(muted);
        }
    }

    /// Master volume in `0..=1`.
    pub fn set_master_volume(&self, volume: f32) {
        self.master_volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// Current master volume.
    pub fn master_volume(&self) -> f32 {
        f32::from_bits(self.master_volume.load(Ordering::Relaxed))
    }

    /// True when a real device is running.
    pub fn device_available(&self) -> bool {
        self.device_available
    }

    /// Stop and drop the stream.
    pub fn shutdown(&mut self) {
        self.stream = None;
        self.device_available = false;
        self.clock.set_audio_driven(false);
    }
}

/// Mix all sources into `data`. Called from the audio callback; must not
/// allocate, lock for long or block.
fn mix(data: &mut [f32], channels: usize, sources: &Mutex<Vec<MixSource>>, master: f32) {
    let channels = channels.max(1);
    let frames = data.len() / channels;
    for s in data.iter_mut() {
        *s = 0.0;
    }
    if frames == 0 {
        return;
    }
    let mut mix_buf = vec![0.0f32; frames * 2];
    if let Ok(mut guard) = sources.lock() {
        for src in guard.iter_mut() {
            src.consumer.fill(&mut mix_buf);
            for f in 0..frames {
                let l = mix_buf[f * 2];
                let r = mix_buf[f * 2 + 1];
                let base = f * channels;
                if base < data.len() {
                    data[base] += l;
                }
                if channels >= 2 && base + 1 < data.len() {
                    data[base + 1] += r;
                } else if channels == 1 && base < data.len() {
                    data[base] += r * 0.5;
                }
                for extra in 2..channels {
                    if base + extra < data.len() {
                        data[base + extra] += (l + r) * 0.5;
                    }
                }
            }
        }
    }
    for s in data.iter_mut() {
        *s = (*s * master).clamp(-1.0, 1.0);
    }
}

/// Headless mixer used by tests: no device, deterministic output.
pub struct OfflineMixer {
    /// The mixed output sample rate.
    pub sample_rate: u32,
}

impl OfflineMixer {
    /// Mix `frames` stereo frames from `sources` into a fresh buffer.
    pub fn mix(sample_rate: u32, frames: usize, sources: &Mutex<Vec<MixSource>>, master: f32) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        mix(&mut out, 2, sources, master);
        let _ = sample_rate;
        out
    }
}

/// Spawn a thread that pumps a fake producer into a floating consumer. Used by
/// tests to prove the SPSC wiring and epoch dropping without a sound card.
pub fn spawn_test_producer(ring: Arc<AudioRing>, epoch: u64, chunks: usize) -> Receiver<()> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for i in 0..chunks {
            ring.push_audio(PcmChunk {
                pts_us: (i as i64) * 10_000,
                epoch,
                volume: 1.0,
                samples: vec![0.25; 960],
            });
        }
        let _ = tx.send(());
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring_with_consumer() -> (Arc<AudioRing>, AudioConsumer) {
        let ring = AudioRing::new(48_000, 2);
        let cons = ring.attach_consumer();
        (ring, cons)
    }

    #[test]
    fn ring_round_trips_samples() {
        let (ring, mut cons) = ring_with_consumer();
        ring.push_audio(PcmChunk {
            pts_us: 0,
            epoch: 0,
            volume: 1.0,
            samples: vec![0.5; 4800],
        });
        let mut out = vec![0.0f32; 2048];
        cons.fill(&mut out);
        assert!(out.iter().any(|s| (*s - 0.5).abs() < 1e-3), "samples must flow");
    }

    #[test]
    fn stale_epoch_chunks_are_dropped() {
        let (ring, mut cons) = ring_with_consumer();
        ring.push_audio(PcmChunk {
            pts_us: 0,
            epoch: 0,
            volume: 1.0,
            samples: vec![0.9; 4800],
        });
        // A seek bumps the epoch.
        ring.mark_seek(1);
        let mut out = vec![0.0f32; 2048];
        cons.fill(&mut out);
        assert!(
            out.iter().all(|s| s.abs() < 1e-6),
            "pre-seek audio must be discarded"
        );
        assert!(cons.drops() > 0, "dropped samples are counted");

        // Fresh audio for the new epoch is heard.
        ring.push_audio(PcmChunk {
            pts_us: 0,
            epoch: 1,
            volume: 1.0,
            samples: vec![0.4; 4800],
        });
        let mut out = vec![0.0f32; 2048];
        cons.fill(&mut out);
        assert!(out.iter().any(|s| (*s - 0.4).abs() < 1e-2));
    }

    #[test]
    fn volume_and_mute_scale_output() {
        let (ring, mut cons) = ring_with_consumer();
        cons.set_volume(0.5);
        ring.push_audio(PcmChunk {
            pts_us: 0,
            epoch: 0,
            volume: 1.0,
            samples: vec![0.8; 4800],
        });
        let mut out = vec![0.0f32; 1024];
        cons.fill(&mut out);
        let peak = out.iter().cloned().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!((peak - 0.4).abs() < 0.02, "peak was {peak}");

        cons.set_muted(true);
        ring.push_audio(PcmChunk {
            pts_us: 0,
            epoch: 0,
            volume: 1.0,
            samples: vec![0.8; 4800],
        });
        let mut out = vec![0.0f32; 1024];
        cons.fill(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn empty_ring_produces_silence_not_panic() {
        let (_ring, mut cons) = ring_with_consumer();
        let mut out = vec![7.0f32; 512];
        cons.fill(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn producer_never_blocks_when_ring_is_full() {
        let (ring, _cons) = ring_with_consumer();
        for _ in 0..100 {
            ring.push_audio(PcmChunk {
                pts_us: 0,
                epoch: 0,
                volume: 1.0,
                samples: vec![0.1; 4096],
            });
        }
        // Reaching here without deadlocking is the assertion.
    }

    #[test]
    fn mixer_sums_two_sources() {
        let a = AudioRing::new(48_000, 2);
        let ca = a.attach_consumer();
        let b = AudioRing::new(48_000, 2);
        let cb = b.attach_consumer();
        a.push_audio(PcmChunk {
            pts_us: 0,
            epoch: 0,
            volume: 1.0,
            samples: vec![0.2; 4800],
        });
        b.push_audio(PcmChunk {
            pts_us: 0,
            epoch: 0,
            volume: 1.0,
            samples: vec![0.3; 4800],
        });
        let sources = Mutex::new(vec![
            MixSource {
                consumer: ca,
                layer_id: 1,
            },
            MixSource {
                consumer: cb,
                layer_id: 2,
            },
        ]);
        let out = OfflineMixer::mix(48_000, 512, &sources, 1.0);
        let peak = out.iter().cloned().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!((peak - 0.5).abs() < 0.02, "peak was {peak}");
    }

    #[test]
    fn master_volume_scales_the_mix() {
        let a = AudioRing::new(48_000, 2);
        let ca = a.attach_consumer();
        a.push_audio(PcmChunk {
            pts_us: 0,
            epoch: 0,
            volume: 1.0,
            samples: vec![0.6; 4800],
        });
        let sources = Mutex::new(vec![MixSource {
            consumer: ca,
            layer_id: 1,
        }]);
        let out = OfflineMixer::mix(48_000, 512, &sources, 0.5);
        let peak = out.iter().cloned().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!((peak - 0.3).abs() < 0.02, "peak was {peak}");
    }

    #[test]
    fn buffered_frames_reports_progress() {
        let (ring, _cons) = ring_with_consumer();
        ring.push_audio(PcmChunk {
            pts_us: 0,
            epoch: 0,
            volume: 1.0,
            samples: vec![0.0; 1920],
        });
        assert!(ring.buffered_frames() > 0);
    }

    #[test]
    fn test_producer_thread_delivers_chunks() {
        let ring = AudioRing::new(48_000, 2);
        let mut cons = ring.attach_consumer();
        let rx = spawn_test_producer(Arc::clone(&ring), 0, 8);
        rx.recv().unwrap();
        let mut out = vec![0.0f32; 8192];
        cons.fill(&mut out);
        assert!(out.iter().any(|s| (*s - 0.25).abs() < 1e-3));
    }
}
