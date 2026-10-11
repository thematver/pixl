//! Minimum-phase band-limited step (minBLEP) residuals for the bitcrusher's sample-and-hold.
//!
//! A sample-and-hold produces a staircase whose steps fall at arbitrary (fractional) times.
//! Rendering each step as a *band-limited* step instead of a sample-grid jump removes the
//! timing jitter of non-integer hold periods and most of the aliasing. The step shape is the
//! running integral of a **minimum-phase** low-pass impulse, so it is causal: a step at time
//! `t` only changes output samples at or after `t`, the crusher adds **no latency**, and the
//! pass band stays flat (unlike a 2-point polyBLEP, whose triangle kernel attenuates
//! −1.8 dB at fs/4 and −5 dB at 0.4·fs).
//!
//! ## Kernel
//! - Prototype: Kaiser-windowed sinc (β = [`KAISER_BETA`]), cutoff [`CUTOFF`]·fs,
//!   [`BLEP_TAPS`] samples long, oversampled ×[`BLEP_PHASES`].
//! - Measured (tests): flat within 0.02 dB to 0.40·fs, −2.4 dB at 0.45·fs, ≤ −72 dB above
//!   0.55·fs (so aliases folding below 0.45·fs are ≥ 72 dB down); a C8 square wave at
//!   44.1 kHz has its worst alias at −93 dB. Step overshoot ≈ 21 % (inherent to minimum
//!   phase; it is ultrasonic ringing).
//! - Converted to minimum phase with the real-cepstrum (homomorphic) method using a
//!   2¹⁶-point FFT (16× the kernel length; a 4× larger FFT changes the measured responses
//!   by < 0.003 dB), faded out over its last quarter, integrated, and normalized so the step
//!   settles to exactly 1.
//! - Stored as the residual `S(x) − 1` (band-limited minus ideal step) for
//!   `x = 0 ..= BLEP_TAPS` samples after the step, phase-major so that the
//!   [`BLEP_TAPS`] values of one step are two contiguous rows (linear interpolation between
//!   neighbouring phases).
//!
//! The table is computed once per process ([`MinBlep::get`], `OnceLock`) — callers fetch it
//! in their constructors, never on the audio thread.

use std::f64::consts::PI;
use std::sync::OnceLock;

/// Output samples touched by one step (residual length).
pub const BLEP_TAPS: usize = 32;
/// Fractional step positions per sample (linear interpolation in between).
pub const BLEP_PHASES: usize = 128;
/// Prototype low-pass cutoff as a fraction of the sample rate.
pub const CUTOFF: f64 = 0.47;
/// Kaiser window β of the prototype (≈ 80 dB side lobes).
pub const KAISER_BETA: f64 = 8.0;

/// Ring-buffer length that comfortably holds one step's residual (power of two ≥ taps).
pub const RING_LEN: usize = 64;
/// Mask for [`RING_LEN`].
pub const RING_MASK: usize = RING_LEN - 1;
const _: () = assert!(RING_LEN >= BLEP_TAPS && RING_LEN.is_power_of_two());

/// The minBLEP residual table.
#[derive(Debug)]
pub struct MinBlep {
    /// `(BLEP_PHASES + 1)` rows × `BLEP_TAPS`: `rows[p][j] = S(j + p/P) − 1`.
    rows: Vec<f32>,
}

static TABLE: OnceLock<MinBlep> = OnceLock::new();

impl MinBlep {
    /// The process-wide table (computed on first use — a few ms in release builds; call it
    /// off the audio thread, e.g. from a constructor).
    pub fn get() -> &'static MinBlep {
        TABLE.get_or_init(MinBlep::build)
    }

    fn build() -> MinBlep {
        Self::build_with(CUTOFF, KAISER_BETA)
    }

    fn build_with(cutoff: f64, beta: f64) -> MinBlep {
        let p = BLEP_PHASES;
        let len = BLEP_TAPS * p;
        // Linear-phase prototype at the oversampled rate.
        let fc = cutoff / p as f64; // cycles per oversampled sample
        let centre = (len - 1) as f64 / 2.0;
        let i0b = bessel_i0(beta);
        let proto: Vec<f64> = (0..len)
            .map(|k| {
                let t = k as f64 - centre;
                let u = t / (centre + 1.0);
                let w = bessel_i0(beta * (1.0 - u * u).max(0.0).sqrt()) / i0b;
                2.0 * fc * sinc(2.0 * fc * t) * w
            })
            .collect();
        let mut h = minimum_phase(&proto, 1 << 16);
        h.truncate(len);
        // Fade out the last quarter so the truncated tail ends smoothly.
        let fade = len / 4;
        for (k, v) in h.iter_mut().enumerate().skip(len - fade) {
            let x = (k - (len - fade)) as f64 / fade as f64;
            *v *= 0.5 + 0.5 * (PI * x).cos();
        }
        // Integrate into the step and normalize to settle at exactly 1.
        let total: f64 = h.iter().sum();
        let mut step = Vec::with_capacity(len + 1);
        let mut acc = 0.0;
        for v in &h {
            acc += v / total;
            step.push(acc);
        }
        step.push(1.0);
        // Phase-major residual rows; row p = P holds S(j + 1) − 1 (the next sample).
        let mut rows = vec![0f32; (p + 1) * BLEP_TAPS];
        for ph in 0..=p {
            for j in 0..BLEP_TAPS {
                let k = j * p + ph;
                let s = if k >= len { 1.0 } else { step[k] };
                rows[ph * BLEP_TAPS + j] = (s - 1.0) as f32;
            }
        }
        MinBlep { rows }
    }

    /// Residual `S(x) − 1` at `x ≥ 0` samples after a unit step (0 for `x ≥ BLEP_TAPS`).
    pub fn residual(&self, x: f64) -> f32 {
        if x.is_nan() || x < 0.0 || x >= BLEP_TAPS as f64 {
            return 0.0;
        }
        let j = x.floor() as usize;
        let pos = (x - j as f64) * BLEP_PHASES as f64;
        let ip = (pos.floor() as usize).min(BLEP_PHASES - 1);
        let fp = (pos - ip as f64) as f32;
        let a = self.rows[ip * BLEP_TAPS + j];
        let b = self.rows[(ip + 1) * BLEP_TAPS + j];
        a + fp * (b - a)
    }

    /// Adds the residual of a step of height `h` into `ring`.
    ///
    /// `x0 ∈ [0, 1)` is the distance (samples) from the step to the output sample at ring
    /// index `start`; samples `start + j` receive `h · (S(x0 + j) − 1)` for
    /// `j < BLEP_TAPS`.
    #[inline]
    pub fn add_step(&self, ring: &mut [f32; RING_LEN], start: usize, x0: f32, h: f32) {
        let pos = x0.clamp(0.0, 1.0) * BLEP_PHASES as f32;
        let ip = (pos as usize).min(BLEP_PHASES - 1);
        let fp = pos - ip as f32;
        let ra = &self.rows[ip * BLEP_TAPS..(ip + 1) * BLEP_TAPS];
        let rb = &self.rows[(ip + 1) * BLEP_TAPS..(ip + 2) * BLEP_TAPS];
        let (wa, wb) = (h * (1.0 - fp), h * fp);
        for (j, (a, b)) in ra.iter().zip(rb).enumerate() {
            ring[(start + j) & RING_MASK] += wa * a + wb * b;
        }
    }
}

/// Minimum-phase version of an FIR (real cepstrum folding), evaluated with an `n`-point FFT
/// (`n` a power of two, ≥ 4 × the input length). Returns `n` samples (use the first
/// `x.len()`).
pub fn minimum_phase(x: &[f64], n: usize) -> Vec<f64> {
    debug_assert!(n.is_power_of_two() && n >= x.len());
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    re[..x.len()].copy_from_slice(x);
    fft(&mut re, &mut im, false);
    // Real cepstrum of the magnitude (floored at −300 dB).
    for k in 0..n {
        let mag = (re[k] * re[k] + im[k] * im[k]).sqrt().max(1e-15);
        re[k] = mag.ln();
        im[k] = 0.0;
    }
    fft(&mut re, &mut im, true);
    // Fold the anti-causal part onto the causal part.
    for k in 1..n / 2 {
        re[k] *= 2.0;
        im[k] *= 2.0;
    }
    for k in n / 2 + 1..n {
        re[k] = 0.0;
        im[k] = 0.0;
    }
    fft(&mut re, &mut im, false);
    // Complex exponential back to the spectrum.
    for k in 0..n {
        let (m, ph) = (re[k].exp(), im[k]);
        re[k] = m * ph.cos();
        im[k] = m * ph.sin();
    }
    fft(&mut re, &mut im, true);
    re
}

/// In-place iterative radix-2 complex FFT (`inverse` includes the 1/n scaling).
pub fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    // Bit reversal.
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let ang = sign * 2.0 * PI / len as f64;
        let half = len / 2;
        // Twiddles computed directly per index (accurate; this runs once per table build).
        for k in 0..half {
            let (s, c) = (ang * k as f64).sin_cos();
            let mut i = k;
            while i < n {
                let (ar, ai) = (re[i], im[i]);
                let (br, bi) = (re[i + half], im[i + half]);
                let tr = br * c - bi * s;
                let ti = br * s + bi * c;
                re[i] = ar + tr;
                im[i] = ai + ti;
                re[i + half] = ar - tr;
                im[i + half] = ai - ti;
                i += len;
            }
        }
        len <<= 1;
    }
    if inverse {
        let s = 1.0 / n as f64;
        re.iter_mut().for_each(|v| *v *= s);
        im.iter_mut().for_each(|v| *v *= s);
    }
}

/// Zeroth-order modified Bessel function of the first kind (power series).
pub(crate) fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..300 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Normalized sinc `sin(πx)/(πx)`.
pub(crate) fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Magnitude response (dB) of the step's derivative (the impulse) at `f` (fraction of fs).
    fn kernel_db(b: &MinBlep, f: f64) -> f64 {
        let n = BLEP_TAPS * BLEP_PHASES;
        let (mut re, mut im) = (0.0, 0.0);
        let mut prev = -1.0f64;
        for k in 0..=n {
            let r = b.residual(k as f64 / BLEP_PHASES as f64) as f64;
            let v = r - prev; // impulse sample (oversampled)
            prev = r;
            let ph = 2.0 * PI * f * k as f64 / BLEP_PHASES as f64;
            re += v * ph.cos();
            im -= v * ph.sin();
        }
        20.0 * (re * re + im * im).sqrt().log10()
    }

    #[test]
    fn kernel_meets_its_design() {
        let b = MinBlep::get();
        let pass: Vec<f64> = [0.1, 0.25, 0.35, 0.40].iter().map(|&f| kernel_db(b, f)).collect();
        let edge = kernel_db(b, 0.45);
        let mut stop = -300.0f64;
        let mut f = 0.55;
        while f < 4.0 {
            stop = stop.max(kernel_db(b, f));
            f += 0.001;
        }
        println!("minBLEP kernel: pass band {pass:.3?} dB at 0.1/0.25/0.35/0.40·fs, {edge:.2} dB at 0.45·fs, stop band ≤ {stop:.1} dB above 0.55·fs");
        assert!(pass.iter().all(|d| d.abs() < 0.05), "{pass:?}");
        assert!(stop < -70.0, "{stop}");
    }

    #[test]
    fn fft_roundtrip_and_known_transform() {
        let n = 64;
        let mut re: Vec<f64> = (0..n).map(|i| (2.0 * PI * 5.0 * i as f64 / n as f64).cos()).collect();
        let mut im = vec![0.0; n];
        let orig = re.clone();
        fft(&mut re, &mut im, false);
        assert!((re[5] - 32.0).abs() < 1e-9 && (re[59] - 32.0).abs() < 1e-9);
        assert!(re.iter().enumerate().filter(|(k, _)| *k != 5 && *k != 59).all(|(_, v)| v.abs() < 1e-9));
        fft(&mut re, &mut im, true);
        for (a, b) in re.iter().zip(&orig) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    #[test]
    fn residual_is_causal_settles_and_starts_at_minus_one() {
        let b = MinBlep::get();
        assert!((b.residual(0.0) + 1.0).abs() < 0.01, "{}", b.residual(0.0));
        assert_eq!(b.residual(BLEP_TAPS as f64), 0.0);
        assert_eq!(b.residual(-0.5), 0.0);
        // Tail is tiny and the step has settled.
        for k in 0..BLEP_PHASES {
            let x = (BLEP_TAPS - 1) as f64 + k as f64 / BLEP_PHASES as f64;
            assert!(b.residual(x).abs() < 1e-4, "{x}: {}", b.residual(x));
        }
        // Gibbs overshoot of a minimum-phase step is bounded.
        let mut max = -1f32;
        for k in 0..BLEP_TAPS * BLEP_PHASES {
            max = max.max(b.residual(k as f64 / BLEP_PHASES as f64));
        }
        // Minimum-phase steps overshoot more than linear-phase ones (all energy up front):
        // ≈ 21 % for every cutoff/window combination around this design.
        println!("minBLEP overshoot {:.2} %", max * 100.0);
        assert!(max < 0.25);
    }

    /// Renders a band-limited square wave with the table and checks that everything between
    /// the harmonics (aliases) is far down (the same test SPEC §4.2 applies to the engine).
    #[test]
    fn square_wave_alias_rejection() {
        let fs = 44_100.0f64;
        let f0 = 4186.009; // C8: harmonics alias heavily with naive steps
        let n = 1 << 15;
        let b = MinBlep::get();
        let mut ring = [0f32; RING_LEN];
        let mut out = vec![0f64; n];
        let period = fs / f0;
        let mut next_edge = 0.37f64; // arbitrary start phase
        let mut level = -0.5f32;
        for (i, y) in out.iter_mut().enumerate() {
            // Edges falling in (i − 1, i].
            while next_edge <= i as f64 {
                let x0 = (i as f64 - next_edge) as f32;
                b.add_step(&mut ring, i, x0, -2.0 * level);
                level = -level;
                next_edge += period / 2.0;
            }
            let slot = i & RING_MASK;
            *y = (level + ring[slot]) as f64;
            ring[slot] = 0.0;
        }
        // Blackman-Harris spectrum.
        let mut re: Vec<f64> = (0..n)
            .map(|i| {
                let a = 2.0 * PI * i as f64 / n as f64;
                let w = 0.35875 - 0.48829 * a.cos() + 0.14128 * (2.0 * a).cos() - 0.01168 * (3.0 * a).cos();
                out[i] * w
            })
            .collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im, false);
        let mag: Vec<f64> = (0..n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        let bin = |f: f64| f * n as f64 / fs;
        let fund = mag[bin(f0).round() as usize - 3..bin(f0).round() as usize + 4].iter().cloned().fold(0.0, f64::max);
        let mut worst = 0f64;
        let mut worst_f = 0.0;
        for (k, &m) in mag.iter().enumerate().skip(8) {
            let f = k as f64 * fs / n as f64;
            if f > 0.40 * fs {
                break; // transition band of the kernel
            }
            let h = (f / f0).round();
            if h >= 1.0 && (f - h * f0).abs() * n as f64 / fs < 10.0 {
                continue; // a harmonic (main lobe)
            }
            if m > worst {
                worst = m;
                worst_f = f;
            }
        }
        let db = 20.0 * (worst / fund).log10();
        println!("minBLEP square C8 @ 44.1 kHz: worst alias {db:.1} dB at {worst_f:.0} Hz");
        assert!(db < -75.0, "{db}");
    }
}
