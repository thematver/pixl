//! Pitch modulators (SPEC §4.6): sweep, vibrato, glide and the arpeggiator's sequencing,
//! plus the small deterministic PRNG used for unison phases and random arps.
//!
//! Everything here is a pure function of time / step index and the parameters, so the voice
//! can evaluate a modulator at any instant (e.g. half a frame ahead for the chip modes'
//! centred staircase) without mutating state.

use pixl_core::{ArpMode, Curve, SweepDir, VibShape};

/// `ln(100)`: an exponential glide reaches 99 % of its distance after the glide time.
pub const GLIDE_EXP_K: f64 = 4.605_170_185_988_091;

/// Tiny deterministic PRNG (SplitMix64). Seeded explicitly; never touches global state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rng(u64);

impl Rng {
    /// A generator from a seed.
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next 64 random bits.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mix64(self.0)
    }

    /// Uniform value in `[0, 1)`.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
}

/// SplitMix64 finalizer: a high-quality 64-bit hash.
#[inline]
pub fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Sweep offset in semitones `t` samples after the (re)trigger (SPEC §4.6): starts at `+amt`
/// (DOWN) or `−amt` (UP) and reaches 0 after `time` samples; EXP = `amt·(1 − t/T)²`,
/// LIN = `amt·(1 − t/T)`.
#[inline]
pub fn sweep_offset(t: f64, amt: f64, time: f64, dir: SweepDir, curve: Curve) -> f64 {
    if !(t.is_finite() && amt.is_finite()) || time <= 0.0 || t >= time {
        return 0.0;
    }
    let r = (1.0 - t.max(0.0) / time).clamp(0.0, 1.0);
    let shape = match curve {
        Curve::Exp => r * r,
        Curve::Lin => r,
    };
    let v = amt * shape;
    match dir {
        SweepDir::Down => v,
        SweepDir::Up => -v,
    }
}

/// Vibrato LFO waveform at `phase` (cycles), range −1..1, starting at 0 going up (sine,
/// triangle) or at +1 (square).
#[inline]
pub fn lfo(phase: f64, shape: VibShape) -> f64 {
    let p = phase - phase.floor();
    match shape {
        VibShape::Sine => (std::f64::consts::TAU * p).sin(),
        VibShape::Tri => {
            if p < 0.25 {
                4.0 * p
            } else if p < 0.75 {
                2.0 - 4.0 * p
            } else {
                4.0 * p - 4.0
            }
        }
        VibShape::Square => {
            if p < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
    }
}

/// Vibrato depth scale 0..1 `t` samples after note-on: 0 during `delay`, then a linear
/// fade-in over `fade`.
#[inline]
pub fn vibrato_fade(t: f64, delay: f64, fade: f64) -> f64 {
    if !t.is_finite() || t < delay {
        0.0
    } else if fade <= 0.0 {
        1.0
    } else {
        ((t - delay) / fade).clamp(0.0, 1.0)
    }
}

/// Glide offset (semitones) `t` samples into a glide that started `from` semitones away
/// from the target and lasts `time` samples (constant-time portamento).
#[inline]
pub fn glide_offset(from: f64, t: f64, time: f64, curve: Curve) -> f64 {
    if from == 0.0 || !from.is_finite() {
        return 0.0;
    }
    if time <= 0.0 || !t.is_finite() {
        return 0.0;
    }
    let x = t.max(0.0) / time;
    match curve {
        Curve::Lin => from * (1.0 - x).max(0.0),
        Curve::Exp => {
            let e = (-GLIDE_EXP_K * x).exp();
            if e < 1e-7 {
                0.0
            } else {
                from * e
            }
        }
    }
}

/// Whether a glide that started `t` samples ago has (practically) finished.
#[inline]
pub fn glide_done(t: f64, time: f64, curve: Curve) -> bool {
    match curve {
        Curve::Lin => t >= time,
        Curve::Exp => t >= time * 3.6,
    }
}

/// Index into an arpeggio of `len` notes at step `step` (SPEC §4.6). UPDOWN is a ping-pong
/// without repeated ends; RANDOM is a deterministic hash of `seed` and the step; ORDER walks
/// the list as given (the caller passes the notes in played order).
#[inline]
pub fn arp_index(mode: ArpMode, step: u64, len: usize, seed: u64) -> usize {
    if len <= 1 {
        return 0;
    }
    let n = len as u64;
    let i = match mode {
        ArpMode::Up | ArpMode::Order => step % n,
        ArpMode::Down => n - 1 - step % n,
        ArpMode::UpDown => {
            let period = 2 * n - 2;
            let j = step % period;
            if j < n {
                j
            } else {
                period - j
            }
        }
        ArpMode::Random => mix64(seed ^ step.wrapping_mul(0xD6E8_FEB8_6659_FD93)) % n,
    };
    i as usize
}

/// The `k`-th note of a sequence built from `base` repeated over `octaves` octaves (each
/// repetition 12 semitones higher): `base[k % len] + 12·(k / len)`.
#[inline]
pub fn arp_note(base: &[i16], k: usize) -> i16 {
    if base.is_empty() {
        return 0;
    }
    let len = base.len();
    base[k % len] + 12 * (k / len).min(16) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_and_uniform() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let mut sum = 0.0;
        for _ in 0..10_000 {
            let x = a.next_f64();
            assert_eq!(x, b.next_f64());
            assert!((0.0..1.0).contains(&x));
            sum += x;
        }
        assert!((sum / 10_000.0 - 0.5).abs() < 0.02);
    }

    #[test]
    fn sweep_shapes() {
        let t = 1000.0;
        assert_eq!(sweep_offset(0.0, 12.0, t, SweepDir::Down, Curve::Exp), 12.0);
        assert_eq!(sweep_offset(0.0, 12.0, t, SweepDir::Up, Curve::Lin), -12.0);
        assert!((sweep_offset(500.0, 12.0, t, SweepDir::Down, Curve::Exp) - 3.0).abs() < 1e-12);
        assert!((sweep_offset(500.0, 12.0, t, SweepDir::Down, Curve::Lin) - 6.0).abs() < 1e-12);
        assert_eq!(sweep_offset(1000.0, 12.0, t, SweepDir::Down, Curve::Exp), 0.0);
        assert_eq!(sweep_offset(f64::NAN, 12.0, t, SweepDir::Down, Curve::Exp), 0.0);
    }

    #[test]
    fn lfo_shapes() {
        for shape in [VibShape::Sine, VibShape::Tri] {
            assert!(lfo(0.0, shape).abs() < 1e-12);
            assert!((lfo(0.25, shape) - 1.0).abs() < 1e-12);
            assert!((lfo(0.75, shape) + 1.0).abs() < 1e-12);
        }
        assert_eq!(lfo(0.1, VibShape::Square), 1.0);
        assert_eq!(lfo(0.6, VibShape::Square), -1.0);
        assert_eq!(lfo(-0.6, VibShape::Square), 1.0);
    }

    #[test]
    fn glide_reaches_99_percent_at_glide_time() {
        let g = glide_offset(-12.0, 4410.0, 4410.0, Curve::Exp);
        assert!((g / -12.0 - 0.01).abs() < 1e-9, "{g}");
        assert!((glide_offset(-12.0, 2205.0, 4410.0, Curve::Lin) + 6.0).abs() < 1e-12);
        assert_eq!(glide_offset(-12.0, 4410.0, 4410.0, Curve::Lin), 0.0);
        assert_eq!(glide_offset(-12.0, 1e9, 4410.0, Curve::Exp), 0.0);
        assert!(glide_done(4410.0, 4410.0, Curve::Lin));
        assert!(!glide_done(4000.0, 4410.0, Curve::Exp));
    }

    #[test]
    fn arp_orders() {
        let seq = |mode| (0..8).map(|s| arp_index(mode, s, 4, 1)).collect::<Vec<_>>();
        assert_eq!(seq(ArpMode::Up), [0, 1, 2, 3, 0, 1, 2, 3]);
        assert_eq!(seq(ArpMode::Down), [3, 2, 1, 0, 3, 2, 1, 0]);
        assert_eq!(seq(ArpMode::UpDown), [0, 1, 2, 3, 2, 1, 0, 1]);
        assert_eq!(seq(ArpMode::Order), seq(ArpMode::Up));
        let r = (0..400).map(|s| arp_index(ArpMode::Random, s, 4, 99)).collect::<Vec<_>>();
        assert!(r.iter().all(|i| *i < 4));
        for v in 0..4 {
            assert!(r.iter().filter(|i| **i == v).count() > 60, "random covers all notes");
        }
        assert_eq!(r, (0..400).map(|s| arp_index(ArpMode::Random, s, 4, 99)).collect::<Vec<_>>());
        assert_eq!(arp_index(ArpMode::UpDown, 5, 1, 0), 0);
        assert_eq!(arp_index(ArpMode::Down, 5, 0, 0), 0);
        let base = [0i16, 4, 7];
        let notes: Vec<i16> = (0..6).map(|k| arp_note(&base, k)).collect();
        assert_eq!(notes, [0, 4, 7, 12, 16, 19]);
    }

    #[test]
    fn vibrato_delay_and_fade() {
        assert_eq!(vibrato_fade(50.0, 100.0, 100.0), 0.0);
        assert!((vibrato_fade(150.0, 100.0, 100.0) - 0.5).abs() < 1e-12);
        assert_eq!(vibrato_fade(500.0, 100.0, 100.0), 1.0);
        assert_eq!(vibrato_fade(100.0, 100.0, 0.0), 1.0);
    }
}
