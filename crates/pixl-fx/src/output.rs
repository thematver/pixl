//! Output stage (SPEC §5.5): smoothed output gain followed by a safety clipper.
//!
//! The clipper is bit-transparent below −1 dBFS (`|x| ≤ 0.891`), then bends smoothly
//! (C¹-continuous tanh knee, slope 1 at the threshold) towards an absolute ceiling of
//! −0.01 dBFS that is never exceeded, whatever the input level.

use crate::util::{db_to_gain, Smoother};

/// Clipper threshold: −1 dBFS.
pub const CLIP_THRESHOLD_DB: f32 = -1.0;
/// Clipper ceiling: −0.01 dBFS.
pub const CLIP_CEILING_DB: f32 = -0.01;
/// Output gain smoothing (seconds to 99 %).
pub const GAIN_SMOOTH_S: f32 = 0.020;

/// Linear clipper threshold (`10^(−1/20)`).
pub const CLIP_THRESHOLD: f32 = 0.891_250_9;
/// Linear clipper ceiling (`10^(−0.01/20)`).
pub const CLIP_CEILING: f32 = 0.998_849_3;

/// Safety clipper: identity for `|x| ≤ CLIP_THRESHOLD`, smooth saturation above, output
/// strictly bounded by `CLIP_CEILING`. NaN maps to 0.
#[inline]
pub fn safety_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= CLIP_THRESHOLD {
        return x;
    }
    if a.is_nan() {
        return 0.0;
    }
    let room = CLIP_CEILING - CLIP_THRESHOLD;
    let y = (CLIP_THRESHOLD + room * ((a - CLIP_THRESHOLD) / room).tanh()).min(CLIP_CEILING);
    y.copysign(x)
}

/// Smoothed output gain + safety clipper.
#[derive(Clone, Debug)]
pub struct OutputStage {
    gain: Smoother,
}

impl OutputStage {
    /// Creates the stage at 0 dB for `fs`.
    pub fn new(fs: f32) -> Self {
        Self { gain: Smoother::new(1.0, GAIN_SMOOTH_S, fs) }
    }

    /// Updates the sample rate.
    pub fn set_sample_rate(&mut self, fs: f32) {
        self.gain.set_sample_rate(fs);
    }

    /// Sets the output gain in dB (smoothed).
    pub fn set_gain_db(&mut self, db: f32) {
        if db.is_finite() {
            self.gain.set_target(db_to_gain(db.clamp(-120.0, 24.0)));
        }
    }

    /// Jumps the gain to its target.
    pub fn reset(&mut self) {
        self.gain.snap();
    }

    /// Applies gain and clipper to a stereo block in place.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        if self.gain.is_settled() {
            let g = self.gain.value();
            if g == 1.0 {
                for i in 0..n {
                    l[i] = safety_clip(l[i]);
                    r[i] = safety_clip(r[i]);
                }
            } else {
                for i in 0..n {
                    l[i] = safety_clip(l[i] * g);
                    r[i] = safety_clip(r[i] * g);
                }
            }
        } else {
            for i in 0..n {
                let g = self.gain.next_value();
                l[i] = safety_clip(l[i] * g);
                r[i] = safety_clip(r[i] * g);
            }
        }
    }

    /// Current linear gain.
    pub fn gain(&self) -> f32 {
        self.gain.value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_their_db_definitions() {
        assert!((CLIP_THRESHOLD - db_to_gain(CLIP_THRESHOLD_DB)).abs() < 1e-6);
        assert!((CLIP_CEILING - db_to_gain(CLIP_CEILING_DB)).abs() < 1e-6);
    }

    #[test]
    fn transparent_below_minus_one_dbfs() {
        let n = 100_000;
        for i in 0..=n {
            let x = CLIP_THRESHOLD * i as f32 / n as f32;
            assert_eq!(safety_clip(x), x);
            assert_eq!(safety_clip(-x), -x);
        }
    }

    #[test]
    fn never_exceeds_the_ceiling_and_is_monotonic_and_smooth() {
        let mut prev = 0f32;
        let mut max_slope = 0f32;
        for i in 0..=200_000 {
            let x = i as f32 * 1e-4; // up to 20.0 (+26 dBFS)
            let y = safety_clip(x);
            assert!(y <= CLIP_CEILING, "{x} → {y}");
            assert!(y >= prev);
            if i > 0 {
                max_slope = max_slope.max((y - prev) / 1e-4);
            }
            prev = y;
        }
        assert!(max_slope <= 1.0 + 1e-2, "slope {max_slope}");
        for x in [1e3f32, 1e30, f32::MAX, f32::INFINITY] {
            assert!(safety_clip(x) <= CLIP_CEILING && safety_clip(x) > 0.99);
            assert!(safety_clip(-x) >= -CLIP_CEILING);
        }
        assert_eq!(safety_clip(f32::NAN), 0.0);
        // Gentle onset: 0.5 dB over the threshold is reduced by well under 0.5 dB.
        let x = db_to_gain(-0.5);
        assert!(safety_clip(x) > db_to_gain(-1.0) && safety_clip(x) < x);
    }

    #[test]
    fn gain_is_smoothed() {
        let fs = 48_000.0;
        let mut o = OutputStage::new(fs);
        o.set_gain_db(-6.0206);
        let mut l = vec![0.5f32; 960]; // 20 ms
        let mut r = l.clone();
        o.process(&mut l, &mut r);
        assert!(l[0] > 0.49, "no jump: {}", l[0]);
        assert!((l[959] - 0.25).abs() < 0.0035, "{}", l[959]);
        let mut l = vec![0.5f32; 4800];
        let mut r = l.clone();
        o.process(&mut l, &mut r);
        assert!((l[4799] - 0.25).abs() < 1e-5);
    }
}
