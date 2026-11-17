//! The master clock.
//!
//! Timeline time is advanced by the number of frames the cpal callback handed
//! to the device, not by any single layer's timestamps, so the audio device is
//! the timebase whenever there is one. The UI interpolates between audio
//! updates with [`std::time::Instant`] to get sub-millisecond presentation
//! smoothness; with no audio device (or when no layer has sound) the same
//! Instant machinery *is* the clock.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Sample rate assumed before the device reports one.
pub const DEFAULT_RATE: u32 = 48_000;

/// The UI will not interpolate further than this past the last audio update; if
/// the device stalls, time stops rather than running away.
pub const MAX_INTERPOLATION_US: i64 = 100_000;

struct Anchor {
    instant: Instant,
    position_us: i64,
}

/// Shared, lock-light clock. One per app, cloned into the audio callback.
pub struct MasterClock {
    anchor: Mutex<Anchor>,
    written_us: AtomicI64,
    latency_us: AtomicI64,
    playing: AtomicBool,
    audio_live: AtomicBool,
    sample_rate: AtomicU32,
}

impl MasterClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            anchor: Mutex::new(Anchor {
                instant: Instant::now(),
                position_us: 0,
            }),
            written_us: AtomicI64::new(0),
            latency_us: AtomicI64::new(0),
            playing: AtomicBool::new(false),
            audio_live: AtomicBool::new(false),
            sample_rate: AtomicU32::new(DEFAULT_RATE),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Anchor> {
        match self.anchor.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed).max(8_000)
    }

    pub fn set_sample_rate(&self, rate: u32) {
        if rate >= 8_000 {
            self.sample_rate.store(rate, Ordering::Relaxed);
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Start or stop the clock. Pausing keeps the position exactly where it is.
    pub fn set_playing(&self, playing: bool) {
        let mut anchor = self.lock();
        if playing == self.playing.load(Ordering::Relaxed) {
            anchor.instant = Instant::now();
            return;
        }
        // Freeze wherever interpolation had got to, then flip.
        let now = Instant::now();
        if !playing {
            anchor.position_us += (now - anchor.instant).as_micros() as i64;
        }
        anchor.instant = now;
        // Keep the audio counters in step so a resume does not jump.
        self.written_us.store(anchor.position_us, Ordering::Relaxed);
        drop(anchor);
        self.playing.store(playing, Ordering::Relaxed);
    }

    /// True when the cpal callback is the timebase.
    pub fn audio_live(&self) -> bool {
        self.audio_live.load(Ordering::Relaxed)
    }

    pub fn set_audio_live(&self, live: bool) {
        if live == self.audio_live.load(Ordering::Relaxed) {
            return;
        }
        let mut anchor = self.lock();
        // Re-anchor at the current position so switching timebase cannot jump.
        let now = Instant::now();
        let elapsed = (now - anchor.instant).as_micros() as i64;
        if self.playing.load(Ordering::Relaxed) && self.audio_live() {
            anchor.position_us += elapsed.min(MAX_INTERPOLATION_US);
        }
        anchor.instant = now;
        self.audio_live.store(live, Ordering::Relaxed);
        self.written_us.store(anchor.position_us, Ordering::Relaxed);
    }

    pub fn latency_us(&self) -> i64 {
        self.latency_us.load(Ordering::Relaxed)
    }

    /// Position without interpolation, which is what the audio callback last
    /// published. Exposed for tests and for the diagnostics line.
    pub fn stored_position_us(&self) -> i64 {
        self.lock().position_us
    }

    /// Timeline position in microseconds, interpolated for smooth presentation.
    pub fn position_us(&self) -> i64 {
        let mut anchor = self.lock();
        if !self.playing.load(Ordering::Relaxed) {
            return anchor.position_us;
        }
        let now = Instant::now();
        let elapsed = (now - anchor.instant).as_micros() as i64;
        if self.audio_live.load(Ordering::Relaxed) {
            anchor.position_us + elapsed.clamp(0, MAX_INTERPOLATION_US)
        } else {
            // The Instant is the timebase: advance and re-anchor.
            anchor.position_us += elapsed;
            anchor.instant = now;
            self.written_us.store(anchor.position_us, Ordering::Relaxed);
            anchor.position_us
        }
    }

    /// Jump the clock, e.g. after a scrub.
    pub fn set_position_us(&self, us: i64) {
        let mut anchor = self.lock();
        anchor.position_us = us;
        anchor.instant = Instant::now();
        self.written_us.store(us, Ordering::Relaxed);
    }

    /// Called from the cpal output callback once per buffer: `frames` samples
    /// per channel have been handed to the device, and `latency_us` is how far
    /// ahead of the speaker they are.
    pub fn advance_frames(&self, frames: i64, latency_us: i64) {
        let delta = frames * 1_000_000 / i64::from(self.sample_rate());
        let written = self.written_us.load(Ordering::Relaxed) + delta;
        self.written_us.store(written, Ordering::Relaxed);
        let latency = latency_us.clamp(0, 1_000_000);
        self.latency_us.store(latency, Ordering::Relaxed);
        let mut anchor = self.lock();
        anchor.position_us = written - latency;
        anchor.instant = Instant::now();
    }

    /// How far interpolation has drifted from the last exact position; used by
    /// the diagnostics line and by the tests.
    pub fn interpolation_us(&self) -> i64 {
        let anchor = self.lock();
        (anchor.position_us - self.written_us.load(Ordering::Relaxed))
            + self.latency_us.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn audio_frames_advance_the_clock_exactly() {
        let clock = MasterClock::new();
        clock.set_sample_rate(48_000);
        clock.set_playing(true);
        clock.set_audio_live(true);
        clock.advance_frames(480, 0);
        assert_eq!(clock.stored_position_us(), 10_000);
        clock.advance_frames(480, 0);
        assert_eq!(clock.stored_position_us(), 20_000);
        // One second of audio at 48 kHz is one second.
        clock.advance_frames(48_000, 0);
        assert_eq!(clock.stored_position_us(), 1_020_000);
    }

    #[test]
    fn latency_is_subtracted_from_the_published_position() {
        let clock = MasterClock::new();
        clock.set_playing(true);
        clock.set_audio_live(true);
        clock.advance_frames(48_000, 21_000);
        assert_eq!(clock.stored_position_us(), 1_000_000 - 21_000);
        assert_eq!(clock.latency_us(), 21_000);
    }

    #[test]
    fn pausing_freezes_the_position() {
        let clock = MasterClock::new();
        clock.set_playing(true);
        clock.set_audio_live(false);
        sleep(Duration::from_millis(5));
        let running = clock.position_us();
        assert!(running > 0);
        clock.set_playing(false);
        let paused = clock.position_us();
        sleep(Duration::from_millis(5));
        assert_eq!(clock.position_us(), paused);
    }

    #[test]
    fn seeking_moves_the_clock_and_keeps_it_there() {
        let clock = MasterClock::new();
        clock.set_playing(true);
        clock.set_position_us(4_000_000);
        let after = clock.position_us();
        assert!((4_000_000..4_100_000).contains(&after), "{after}");
        clock.set_playing(false);
        clock.set_position_us(-1_500_000);
        assert_eq!(clock.position_us(), -1_500_000);
    }

    #[test]
    fn instant_driven_mode_advances_between_reads() {
        let clock = MasterClock::new();
        clock.set_audio_live(false);
        clock.set_playing(true);
        let first = clock.position_us();
        sleep(Duration::from_millis(10));
        let second = clock.position_us();
        assert!(second > first, "{first} -> {second}");
        assert!(second - first >= 5_000, "{}", second - first);
    }

    #[test]
    fn interpolation_is_bounded_when_the_device_stalls() {
        let clock = MasterClock::new();
        clock.set_playing(true);
        clock.set_audio_live(true);
        clock.advance_frames(48, 0); // 1 ms of audio
        sleep(Duration::from_millis(10));
        let pos = clock.position_us();
        assert!(pos <= 1_000 + MAX_INTERPOLATION_US + 1_000);
    }

    #[test]
    fn switching_timebase_does_not_jump() {
        let clock = MasterClock::new();
        clock.set_playing(true);
        clock.set_audio_live(false);
        sleep(Duration::from_millis(8));
        let before = clock.position_us();
        clock.set_audio_live(true);
        let after = clock.position_us();
        assert!((after - before).abs() < 5_000, "{before} -> {after}");
    }
}
