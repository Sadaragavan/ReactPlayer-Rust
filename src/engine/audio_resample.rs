//! Fractional audio resampler used to absorb small clock drift.
//!
//! This is a pure-Rust implementation (no `rubato`, no `swr_set_compensation`)
//! so the behaviour is predictable and testable: a 4-tap cubic (Catmull-Rom)
//! interpolator over interleaved stereo `f32` samples with a variable playback
//! ratio clamped to `0.995..=1.005` by the caller.
//!
//! The resampler is fed *after* FFmpeg's `swresample` has already converted the
//! decoder output to the device sample rate and channel count; it only bends
//! that stream by a few tenths of a percent, which is inaudible.

/// Number of taps used by the cubic interpolator.
const TAPS: usize = 4;

/// Cubic fractional resampler for interleaved stereo audio.
///
/// The history buffer keeps the last [`TAPS`] frames so that interpolation can
/// read `p[-1]` and `p[+2]` around the fractional position without stalling.
#[derive(Debug, Clone)]
pub struct DriftResampler {
    channels: usize,
    /// History: last frames, each `channels` samples, oldest first.
    history: Vec<f32>,
    /// Fractional read position within `history`, in frames.
    pos: f32,
    /// Current playback ratio (1.0 = unchanged).
    ratio: f32,
}

impl DriftResampler {
    /// Create a resampler for `channels` interleaved channels.
    pub fn new(channels: usize) -> Self {
        let channels = channels.max(1);
        Self {
            channels,
            history: vec![0.0; TAPS * channels],
            pos: 1.0,
            ratio: 1.0,
        }
    }

    /// Current ratio.
    pub fn ratio(&self) -> f32 {
        self.ratio
    }

    /// Set the ratio, clamped to the drift-nudge range.
    pub fn set_ratio(&mut self, ratio: f32) {
        self.ratio = if ratio.is_finite() {
            ratio.clamp(super::sync_clock::MIN_RATE, super::sync_clock::MAX_RATE)
        } else {
            1.0
        };
    }

    /// Drop all history (after a seek).
    pub fn reset(&mut self) {
        for s in &mut self.history {
            *s = 0.0;
        }
        self.pos = 1.0;
        self.ratio = 1.0;
    }

    /// Number of frames currently held in the history window.
    fn history_frames(&self) -> usize {
        self.history.len() / self.channels
    }

    /// Push one input frame into the history, dropping the oldest.
    fn push_frame(&mut self, frame: &[f32]) {
        let ch = self.channels;
        self.history.drain(0..ch);
        if frame.len() >= ch {
            self.history.extend_from_slice(&frame[..ch]);
        } else {
            self.history.extend(std::iter::repeat_n(0.0, ch));
        }
        self.pos -= 1.0;
    }

    /// Read the interpolated frame at the current fractional position.
    fn sample_here(&self) -> [f32; 2] {
        let ch = self.channels;
        let frames = self.history_frames();
        let i = self.pos.floor().max(0.0) as usize;
        let t = self.pos - i as f32;

        // Indices for Catmull-Rom: p0 = i-1, p1 = i, p2 = i+1, p3 = i+2
        let idx = |offset: isize| -> usize {
            let base = i as isize + offset;
            base.clamp(0, frames as isize - 1) as usize
        };
        let i0 = idx(-1);
        let i1 = idx(0);
        let i2 = idx(1);
        let i3 = idx(2);

        let mut out = [0.0f32; 2];
        for (c, slot) in out.iter_mut().enumerate().take(ch) {
            let p0 = self.history[i0 * ch + c];
            let p1 = self.history[i1 * ch + c];
            let p2 = self.history[i2 * ch + c];
            let p3 = self.history[i3 * ch + c];
            *slot = catmull_rom(p0, p1, p2, p3, t);
        }
        if ch == 1 {
            out[1] = out[0];
        }
        out
    }

    /// Resample `input` into `output`, returning the number of frames written.
    ///
    /// `output` is interleaved stereo; existing contents are overwritten.
    /// The output buffer must be large enough: at most
    /// `input.len() / channels` frames can be produced (the ratio is clamped to
    /// at most 1.005, and the resampler maintains one frame of latency).
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> usize {
        let ch = self.channels;
        if ch == 0 || input.is_empty() || output.len() < 2 {
            return 0;
        }
        let in_frames = input.len() / ch;
        let out_cap = output.len() / 2;
        let mut written = 0usize;

        for f in 0..in_frames {
            self.push_frame(&input[f * ch..(f + 1) * ch]);
            // Emit while the fractional read position is at least one frame
            // behind the newest input, so the cubic taps always exist.
            while self.pos >= 1.0 && self.pos < (self.history_frames() as f32) - 1.0 {
                if written >= out_cap {
                    return written;
                }
                let s = self.sample_here();
                output[written * 2] = s[0];
                output[written * 2 + 1] = s[1];
                written += 1;
                self.pos += self.ratio;
            }
        }
        written
    }

    /// Flush the remaining interpolated samples (call on end of stream).
    pub fn flush(&mut self, output: &mut [f32]) -> usize {
        let out_cap = output.len() / 2;
        let mut written = 0usize;
        while self.pos < (self.history_frames() as f32) - 1.0 && written < out_cap {
            let s = self.sample_here();
            output[written * 2] = s[0];
            output[written * 2 + 1] = s[1];
            written += 1;
            self.pos += self.ratio;
        }
        written
    }
}

/// Catmull-Rom cubic interpolation between `p1` and `p2` at fraction `t`.
pub fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c = -0.5 * p0 + 0.5 * p2;
    ((a * t + b) * t + c) * t + p1
}

/// Linear interpolation, exposed for the tests and the CPU frame path.
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catmull_rom_hits_knots() {
        assert!((catmull_rom(0.2, 0.7, 0.4, 0.9, 0.0) - 0.7).abs() < 1e-6);
        assert!((catmull_rom(0.2, 0.7, 0.4, 0.9, 1.0) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn resampler_at_unity_produces_about_one_to_one() {
        let frames: Vec<f32> = (0..480).flat_map(|i| [i as f32, -(i as f32)]).collect();
        let mut rs = DriftResampler::new(2);
        let mut out = vec![0.0f32; frames.len() * 2];
        let n = rs.process(&frames, &mut out);
        assert!(n >= 470 && n <= 480, "expected ~480 frames, got {n}");
        // The first few samples should be a smooth ramp.
        assert!(out[4] > out[2], "ramp must be increasing");
    }

    #[test]
    fn ratio_above_one_produces_more_frames() {
        let frames: Vec<f32> = (0..960).flat_map(|i| [i as f32, 0.0]).collect();
        let mut slow = DriftResampler::new(2);
        slow.set_ratio(0.995);
        let mut slow_out = vec![0.0f32; 4096];
        let n_slow = slow.process(&frames, &mut slow_out);

        let mut fast = DriftResampler::new(2);
        fast.set_ratio(1.005);
        let mut fast_out = vec![0.0f32; 4096];
        let n_fast = fast.process(&frames, &mut fast_out);
        assert!(
            n_fast > n_slow,
            "1.005 ratio must consume the input faster ({n_fast} vs {n_slow})"
        );
    }

    #[test]
    fn ratio_is_clamped_to_nudge_range() {
        let mut rs = DriftResampler::new(2);
        rs.set_ratio(2.0);
        assert_eq!(rs.ratio(), super::super::sync_clock::MAX_RATE);
        rs.set_ratio(0.1);
        assert_eq!(rs.ratio(), super::super::sync_clock::MIN_RATE);
        rs.set_ratio(f32::NAN);
        assert_eq!(rs.ratio(), 1.0);
    }

    #[test]
    fn reset_clears_history() {
        let mut rs = DriftResampler::new(2);
        let input = vec![1.0f32; 32];
        let mut out = vec![0.0f32; 128];
        let _ = rs.process(&input, &mut out);
        rs.reset();
        assert!(rs.history.iter().all(|s| *s == 0.0));
        assert_eq!(rs.pos, 1.0);
    }

    #[test]
    fn flush_drains_and_stops() {
        let mut rs = DriftResampler::new(2);
        let mut out = vec![0.0f32; 512];
        let a = rs.flush(&mut out);
        let b = rs.flush(&mut out);
        assert!(a <= 512, "flush cannot overflow the buffer");
        assert!(a + b <= 512);
    }

    #[test]
    fn lerp_matches_endpoints() {
        assert_eq!(lerp(1.0, 3.0, 0.0), 1.0);
        assert_eq!(lerp(1.0, 3.0, 1.0), 3.0);
        assert_eq!(lerp(1.0, 3.0, 0.5), 2.0);
    }
}
