//! A fractional resampler over interleaved stereo `f32`, used for the
//! +-0.5 % rate nudge that absorbs drift in the 5..100 ms band.
//!
//! Nothing else resamples: the decoder is already asked for the output device's
//! sample rate, so this only ever shifts the rate by half a percent. The
//! filter is linear interpolation, which at that ratio is inaudible and costs
//! two multiplies per sample.

/// Smallest rate the sync engine will ever ask for.
pub const MIN_RATE: f64 = 0.995;
/// Largest rate the sync engine will ever ask for.
pub const MAX_RATE: f64 = 1.005;

/// Channels this resampler handles (the mixer is stereo throughout).
pub const CHANNELS: usize = 2;

/// Linear-interpolating fractional resampler for interleaved stereo.
pub struct FractionalResampler {
    rate: f64,
    /// Position inside the current chunk, in frames, `0.0..1.0` relative to the
    /// frame pair `(previous, current)`.
    phase: f64,
    /// Last input frame of the previous chunk, so chunks join without a click.
    previous: [f32; CHANNELS],
    has_previous: bool,
    /// Total frames in, frames out, for the drift report in the UI.
    frames_in: u64,
    frames_out: u64,
}

impl Default for FractionalResampler {
    fn default() -> Self {
        Self::new()
    }
}

impl FractionalResampler {
    pub fn new() -> Self {
        Self {
            rate: 1.0,
            phase: 0.0,
            previous: [0.0; CHANNELS],
            has_previous: false,
            frames_in: 0,
            frames_out: 0,
        }
    }

    /// Set the rate factor, clamped to the nudge band.
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = if rate.is_finite() {
            rate.clamp(MIN_RATE, MAX_RATE)
        } else {
            1.0
        };
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn is_passthrough(&self) -> bool {
        (self.rate - 1.0).abs() < f64::EPSILON
    }

    /// Forget the interpolation state. Called on every seek so the new material
    /// does not interpolate with the old.
    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.previous = [0.0; CHANNELS];
        self.has_previous = false;
    }

    pub fn counters(&self) -> (u64, u64) {
        (self.frames_in, self.frames_out)
    }

    /// Resample `input` (interleaved stereo, any length) and append the result
    /// to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let frames = input.len() / CHANNELS;
        if frames == 0 {
            return;
        }
        self.frames_in += frames as u64;

        if self.is_passthrough() {
            out.extend_from_slice(&input[..frames * CHANNELS]);
            self.previous = [input[(frames - 1) * CHANNELS], input[(frames - 1) * CHANNELS + 1]];
            self.has_previous = true;
            self.phase = 0.0;
            self.frames_out += frames as u64;
            return;
        }

        // The virtual array for this call is
        //   v[0] = previous frame (or the first input frame if there is none)
        //   v[k] = input[k - 1]
        // with a length of `frames + 1`. `phase` is the read position in that
        // array: 0 means "exactly at v[0]", 1 means "exactly at v[1]".
        let first = if self.has_previous {
            self.previous
        } else {
            [input[0], input[1]]
        };
        let sample = |index: usize| -> [f32; CHANNELS] {
            if index == 0 {
                first
            } else {
                let base = (index - 1) * CHANNELS;
                [input[base], input[base + 1]]
            }
        };

        let last_index = frames; // v index of the newest input frame
        let mut position = self.phase;
        while position + 1.0 <= last_index as f64 {
            let left_index = position.floor() as usize;
            let frac = (position - left_index as f64) as f32;
            let a = sample(left_index);
            let b = sample(left_index + 1);
            out.push(a[0] + (b[0] - a[0]) * frac);
            out.push(a[1] + (b[1] - a[1]) * frac);
            position += self.rate;
            self.frames_out += 1;
        }

        // Carry the tail into the next chunk. The newest input frame becomes the
        // next call's v[0], so shift the residual position by `last_index`.
        self.previous = sample(last_index);
        self.has_previous = true;
        self.phase = (position - last_index as f64).clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(frames: usize, start: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let x = start + i as f32;
            v.push(x);
            v.push(-x);
        }
        v
    }

    #[test]
    fn passthrough_is_bit_exact() {
        let mut r = FractionalResampler::new();
        let input = ramp(64, 0.0);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert_eq!(out, input);
        assert_eq!(r.counters(), (64, 64));
    }

    #[test]
    fn rate_is_clamped_to_the_nudge_band() {
        let mut r = FractionalResampler::new();
        r.set_rate(2.0);
        assert_eq!(r.rate(), MAX_RATE);
        r.set_rate(0.1);
        assert_eq!(r.rate(), MIN_RATE);
        r.set_rate(f64::NAN);
        assert_eq!(r.rate(), 1.0);
        assert!(r.is_passthrough());
    }

    #[test]
    fn a_faster_rate_plays_the_same_input_in_less_time() {
        // `rate` is a speed factor: at 1.005 the resampler walks through 1.005
        // input frames for every output frame it emits, so the same chunk comes
        // out shorter, which is what closes a drift where the track is behind.
        let mut r = FractionalResampler::new();
        r.set_rate(MAX_RATE);
        let input = ramp(10_000, 0.0);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        let produced = out.len() / 2;
        let expected = (10_000.0 / MAX_RATE) as usize;
        assert!(
            produced.abs_diff(expected) <= 2,
            "produced {produced}, expected about {expected}"
        );
        assert!(produced < 10_000);
        assert_eq!(r.counters(), (10_000, produced as u64));
    }

    #[test]
    fn a_slower_rate_stretches_the_same_input() {
        let mut r = FractionalResampler::new();
        r.set_rate(MIN_RATE);
        let input = ramp(10_000, 0.0);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        let produced = out.len() / 2;
        let expected = (10_000.0 / MIN_RATE) as usize;
        assert!(
            produced.abs_diff(expected) <= 2,
            "produced {produced}, expected about {expected}"
        );
        assert!(produced > 10_000);
    }

    #[test]
    fn chunks_join_without_a_discontinuity() {
        let mut r = FractionalResampler::new();
        r.set_rate(1.002);
        let mut out = Vec::new();
        for chunk in 0..4 {
            r.process(&ramp(100, chunk as f32 * 100.0), &mut out);
        }
        // A straight ramp stays monotonic across the chunk boundaries.
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        for pair in left.windows(2) {
            assert!(
                pair[1] > pair[0],
                "ramp went backwards: {} -> {}",
                pair[0],
                pair[1]
            );
            assert!(pair[1] - pair[0] < 2.0);
        }
    }

    #[test]
    fn reset_forgets_the_tail() {
        let mut r = FractionalResampler::new();
        r.set_rate(1.001);
        let mut out = Vec::new();
        r.process(&ramp(50, 0.0), &mut out);
        r.reset();
        out.clear();
        // A constant DC signal must start exactly at its value after a reset.
        let dc = vec![0.25f32; 64];
        r.process(&dc, &mut out);
        assert!((out[0] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn odd_input_lengths_are_ignored_safely() {
        let mut r = FractionalResampler::new();
        r.set_rate(1.003);
        let mut out = Vec::new();
        r.process(&[0.1, 0.2, 0.3], &mut out); // one and a half frames
        assert_eq!(out.len() % 2, 0);
    }
}
