//! Small DSP helpers shared by the effect modules: level conversions, input sanitizing,
//! parameter smoothers and crossfade ramps.
//!
//! Everything here is allocation-free and safe to call on the audio thread.

/// Largest absolute sample value accepted into the chain (+36 dBFS). Anything louder is
/// clamped by [`sanitize_sample`] so that later gain stages cannot overflow to infinity.
pub const MAX_INPUT: f32 = 64.0;

/// Converts decibels to a linear gain factor.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Converts a linear magnitude to decibels, clamped below at `floor_db` (also used for
/// zero, negative and non-finite input).
#[inline]
pub fn gain_to_db_floor(gain: f32, floor_db: f32) -> f32 {
    if gain.is_finite() && gain > 0.0 {
        (20.0 * gain.log10()).max(floor_db)
    } else {
        floor_db
    }
}

/// Replaces NaN/±Inf with silence and clamps to ±[`MAX_INPUT`].
#[inline]
pub fn sanitize_sample(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(-MAX_INPUT, MAX_INPUT)
    } else {
        0.0
    }
}

/// Returns `x` if it is finite, otherwise `fallback`.
#[inline]
pub fn finite_or(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        fallback
    }
}

/// Exponential (one-pole) parameter smoother.
///
/// `time_s` is the time needed to cover 99 % of a step (time constant `time_s / ln 100`).
/// The value snaps exactly onto the target once it is within a tiny epsilon, so a settled
/// smoother is bit-stable and `is_settled()` becomes true.
#[derive(Clone, Copy, Debug)]
pub struct Smoother {
    value: f32,
    target: f32,
    coeff: f32,
    time_s: f32,
}

impl Smoother {
    /// Relative snap threshold.
    const EPS: f32 = 1e-6;

    /// A smoother settled at `value`, configured for `time_s` at `sample_rate`.
    pub fn new(value: f32, time_s: f32, sample_rate: f32) -> Self {
        let mut s = Self { value, target: value, coeff: 1.0, time_s };
        s.set_sample_rate(sample_rate);
        s
    }

    /// Recomputes the coefficient for a new sample rate (keeps the current value).
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        let samples = (self.time_s * sample_rate).max(1.0);
        // 1 - exp(-ln(100) / samples): 99 % of a step after `samples` samples.
        self.coeff = 1.0 - (-(100f32.ln()) / samples).exp();
    }

    /// Sets a new target (non-finite targets are ignored).
    #[inline]
    pub fn set_target(&mut self, target: f32) {
        if target.is_finite() {
            self.target = target;
        }
    }

    /// Jumps to `value` immediately (also sets the target).
    #[inline]
    pub fn reset_to(&mut self, value: f32) {
        if value.is_finite() {
            self.value = value;
            self.target = value;
        }
    }

    /// Jumps to the current target immediately.
    #[inline]
    pub fn snap(&mut self) {
        self.value = self.target;
    }

    /// Advances one sample and returns the new value.
    ///
    /// Snaps onto the target when the remaining distance is negligible *or* when the step
    /// no longer changes the `f32` value (the increment fell below half an ULP), so the
    /// smoother always settles exactly instead of stalling a few ULPs away.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        let d = self.target - self.value;
        let v = self.value + d * self.coeff;
        if v == self.value || d.abs() <= Self::EPS * (1.0 + self.target.abs()) {
            self.value = self.target;
        } else {
            self.value = v;
        }
        self.value
    }

    /// Current value without advancing.
    #[inline]
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Current target.
    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Whether the value has reached the target.
    #[inline]
    pub fn is_settled(&self) -> bool {
        self.value == self.target
    }
}

/// Linear ramp between 0 and 1 used for click-free on/off crossfades.
#[derive(Clone, Copy, Debug)]
pub struct Ramp {
    value: f32,
    target: f32,
    step: f32,
    time_s: f32,
}

impl Ramp {
    /// A ramp settled at `value` (0 or 1) covering a full 0→1 transition in `time_s`.
    pub fn new(value: f32, time_s: f32, sample_rate: f32) -> Self {
        let mut r = Self { value, target: value, step: 1.0, time_s };
        r.set_sample_rate(sample_rate);
        r
    }

    /// Recomputes the per-sample step for a new sample rate.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.step = 1.0 / (self.time_s * sample_rate).max(1.0);
    }

    /// Ramps towards 1 (`on`) or 0.
    #[inline]
    pub fn set_on(&mut self, on: bool) {
        self.target = if on { 1.0 } else { 0.0 };
    }

    /// Jumps to the target.
    #[inline]
    pub fn snap(&mut self) {
        self.value = self.target;
    }

    /// Advances one sample and returns the new value in `0..=1`.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        if self.value < self.target {
            self.value = (self.value + self.step).min(self.target);
        } else if self.value > self.target {
            self.value = (self.value - self.step).max(self.target);
        }
        self.value
    }

    /// Current value.
    #[inline]
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Whether the ramp is moving.
    #[inline]
    pub fn is_moving(&self) -> bool {
        self.value != self.target
    }

    /// Whether the ramp rests at 0.
    #[inline]
    pub fn is_off(&self) -> bool {
        self.value == 0.0 && self.target == 0.0
    }

    /// Whether the ramp rests at 1.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.value == 1.0 && self.target == 1.0
    }
}

/// Deterministic xorshift64* generator for tests, fuzzing and the render example.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// Creates a generator; a zero seed is replaced by a fixed non-zero constant.
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    /// Next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in `[-1, 1)`.
    pub fn bipolar(&mut self) -> f32 {
        self.next_f32() * 2.0 - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoother_settles_in_time() {
        let fs = 48_000.0;
        let mut s = Smoother::new(0.0, 0.02, fs);
        s.set_target(1.0);
        let n = (0.02 * fs) as usize;
        for _ in 0..n {
            s.next_value();
        }
        assert!((s.value() - 0.99).abs() < 2e-3, "{}", s.value());
        for _ in 0..(n * 4) {
            s.next_value();
        }
        assert!(s.is_settled());
        assert_eq!(s.value(), 1.0);
    }

    #[test]
    fn ramp_is_linear_and_exact() {
        let mut r = Ramp::new(0.0, 0.01, 1000.0);
        r.set_on(true);
        for i in 1..=10 {
            assert!((r.next_value() - i as f32 / 10.0).abs() < 1e-6);
        }
        assert!(r.is_full());
        r.set_on(false);
        for _ in 0..10 {
            r.next_value();
        }
        assert!(r.is_off());
    }

    #[test]
    fn sanitize_and_db() {
        assert_eq!(sanitize_sample(f32::NAN), 0.0);
        assert_eq!(sanitize_sample(f32::INFINITY), 0.0);
        assert_eq!(sanitize_sample(1e9), MAX_INPUT);
        assert_eq!(gain_to_db_floor(0.0, -100.0), -100.0);
        assert!((gain_to_db_floor(0.5, -100.0) + 6.0206).abs() < 1e-3);
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-5);
    }
}
