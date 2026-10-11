//! Bitcrusher (SPEC §5.2): amplitude quantization plus band-limited fractional
//! sample-and-hold.
//!
//! ## Quantizer
//! A *mid-tread* quantizer with `2^bits` levels, i.e. the transfer curve of a signed
//! `bits`-bit converter: step `Δ = 2^(1−bits)`, codes `−floor(S) ..= ceil(S) − 1` with
//! `S = 2^(bits−1)`, output `code · Δ`. Zero is a level, so silence stays silence and quiet
//! tails gate off like on real low-resolution hardware instead of turning into a buzz.
//! Fractional bit depths are supported (`bits = 3.5` → Δ = 0.177, 11 levels) so macro sweeps
//! move continuously; for integer depths the level count is exactly `2^bits`
//! (1 bit = {−1, 0}, 2 bits = {−1, −½, 0, ½}, …). The output never leaves `[-1, 1]`.
//!
//! ## Sample-and-hold
//! A fractional phase accumulator (f64) clocks the hold at `crush_rate` Hz with sub-sample
//! timing. At each clock instant the input is sampled with a centred 4-point (cubic)
//! Lagrange interpolator, quantized, and the resulting step of the staircase is rendered as
//! a **minimum-phase band-limited step** ([`crate::blep`]) at the exact clock instant. Hold
//! periods that are not an integer number of samples (e.g. 4.5) therefore produce exact,
//! jitter-free step timing instead of alternating 4/5-sample holds, aliasing of the S&H
//! images is suppressed by the kernel's ≈ 72 dB stop band, and the pass band follows the
//! ideal zero-order-hold response `sinc(f/rate)`: the kernel is flat to 0.4·fs, and the
//! cubic interpolator adds ≤ 0.03 dB up to ≈ fs/10, 0.34 dB at 0.21·fs and 0.8 dB at fs/4
//! (measured at 48 kHz; a linear interpolator would lose 1.3 dB already at 0.21·fs). Jitter
//! sidebands of a 4.5-sample hold measure −133 dB (naive grid hold: −30 dB).
//!
//! The centred interpolator needs one sample of look-ahead, so the wet signal is delayed by
//! exactly **one sample** (the step itself is causal and adds nothing). The dry signal is
//! not delayed: off is a true bypass, and the 10 ms on/off crossfade only mixes `x[n]` with
//! `x[n−1]` momentarily.
//!
//! `crush_rate ≥ fs` disables the hold (quantize only, same one-sample timing). Rate and bit
//! depth are smoothed (20 ms) so macro sweeps are click-free; enabling/disabling crossfades
//! over 10 ms; off costs nothing.

use crate::blep::{MinBlep, RING_LEN, RING_MASK};
use crate::util::{Ramp, Smoother};

/// Smoothing time of rate and bit depth (seconds to 99 %).
pub const PARAM_SMOOTH_S: f32 = 0.020;
/// On/off crossfade time (seconds).
pub const ONOFF_XFADE_S: f32 = 0.010;
/// Lowest accepted bit depth.
pub const MIN_BITS: f32 = 1.0;
/// Highest accepted bit depth.
pub const MAX_BITS: f32 = 16.0;
/// Lowest accepted hold rate (Hz).
pub const MIN_RATE: f32 = 100.0;

/// Mid-tread quantizer with `2^bits` levels (see the module docs). `bits` is clamped to
/// `1..=16`; non-finite input yields 0.
#[inline]
pub fn quantize(x: f32, bits: f32) -> f32 {
    let bits = if bits.is_finite() { bits.clamp(MIN_BITS, MAX_BITS) } else { MAX_BITS };
    let s = (bits - 1.0).exp2();
    quantize_scaled(x, s, 1.0 / s)
}

/// Quantizer core with precomputed `s = 2^(bits−1)` and `inv_s = 1/s`.
#[inline]
fn quantize_scaled(x: f32, s: f32, inv_s: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    let lo = -s.floor();
    let hi = s.ceil() - 1.0;
    let code = (x * s).round().clamp(lo, hi);
    // `+ 0.0` turns a rounded −0.0 into +0.0 so silence is bit-exact zero.
    (code * inv_s).clamp(-1.0, 1.0) + 0.0
}

/// Number of distinct output levels of [`quantize`] for a bit depth.
pub fn level_count(bits: f32) -> u32 {
    let s = (bits.clamp(MIN_BITS, MAX_BITS) - 1.0).exp2();
    (s.floor() + s.ceil()) as u32
}

/// Per-channel sample-and-hold state.
#[derive(Clone, Copy, Debug)]
struct HoldState {
    /// The last three input samples `[x[n−3], x[n−2], x[n−1]]` (interpolator support).
    hist: [f32; 3],
    /// Currently held (quantized) value: the naive staircase.
    held: f32,
    /// Pending band-limited step residuals, indexed by absolute sample position.
    ring: [f32; RING_LEN],
}

impl Default for HoldState {
    fn default() -> Self {
        Self { hist: [0.0; 3], held: 0.0, ring: [0.0; RING_LEN] }
    }
}

impl HoldState {
    /// Shifts a new input sample into the history.
    #[inline]
    fn push(&mut self, x: f32) {
        self.hist = [self.hist[1], self.hist[2], x];
    }
}

/// Cubic Lagrange interpolation through `p[-1], p[0], p[1], p[2]` at `u ∈ [0, 1]`
/// (between `p[0]` and `p[1]`).
#[inline]
fn lagrange4(pm1: f32, p0: f32, p1: f32, p2: f32, u: f32) -> f32 {
    let (a, b, c) = (u + 1.0, u - 1.0, u - 2.0);
    let wm1 = -u * b * c * (1.0 / 6.0);
    let w0 = a * b * c * 0.5;
    let w1 = -a * u * c * 0.5;
    let w2 = a * u * b * (1.0 / 6.0);
    wm1 * pm1 + w0 * p0 + w1 * p1 + w2 * p2
}

/// Stereo bitcrusher.
#[derive(Clone, Debug)]
pub struct Crusher {
    fs: f32,
    enabled: bool,
    mix: Ramp,
    rate: Smoother,
    bits: Smoother,
    /// Hold clock phase in `[0, 1)`; shared by both channels so they stay in lockstep.
    phase: f64,
    /// Ring position of the current output sample.
    pos: usize,
    ch: [HoldState; 2],
    blep: &'static MinBlep,
    /// Number of hold clock ticks since reset (diagnostics/tests).
    ticks: u64,
}

impl Crusher {
    /// Creates a disabled crusher for `fs` with the parameter-table defaults. Builds the
    /// shared minBLEP table on first use (call off the audio thread).
    pub fn new(fs: f32) -> Self {
        Self {
            fs,
            enabled: false,
            mix: Ramp::new(0.0, ONOFF_XFADE_S, fs),
            rate: Smoother::new(11_025.0, PARAM_SMOOTH_S, fs),
            bits: Smoother::new(6.0, PARAM_SMOOTH_S, fs),
            phase: 0.0,
            pos: 0,
            ch: [HoldState::default(); 2],
            blep: MinBlep::get(),
            ticks: 0,
        }
    }

    /// Updates the sample rate (keeps state).
    pub fn set_sample_rate(&mut self, fs: f32) {
        self.fs = fs;
        self.mix.set_sample_rate(fs);
        self.rate.set_sample_rate(fs);
        self.bits.set_sample_rate(fs);
    }

    /// Sets the parameters: `on`, bit depth (1..16, fractional) and hold rate (Hz).
    /// Non-finite values keep the previous targets.
    pub fn set_params(&mut self, on: bool, bits: f32, rate_hz: f32) {
        let bits = if bits.is_finite() { bits.clamp(MIN_BITS, MAX_BITS) } else { self.bits.target() };
        let rate = if rate_hz.is_finite() { rate_hz.max(MIN_RATE) } else { self.rate.target() };
        if on && !self.enabled && self.mix.is_off() {
            // Waking from bypass: start the hold clock fresh, no smoothing from stale values.
            self.bits.reset_to(bits);
            self.rate.reset_to(rate);
            self.phase = 0.0;
            for c in &mut self.ch {
                c.ring = [0.0; RING_LEN];
                c.held = quantize(c.hist[2], bits);
            }
        }
        self.enabled = on;
        self.mix.set_on(on);
        self.bits.set_target(bits);
        self.rate.set_target(rate);
    }

    /// Clears the hold state and jumps all smoothers/crossfades to their targets.
    pub fn reset(&mut self) {
        self.mix.snap();
        self.rate.snap();
        self.bits.snap();
        self.phase = 0.0;
        self.pos = 0;
        self.ch = [HoldState::default(); 2];
        self.ticks = 0;
    }

    /// Whether the crusher currently affects the signal (on, or crossfading out).
    pub fn is_active(&self) -> bool {
        !self.mix.is_off()
    }

    /// Hold clock ticks since the last reset (each tick is one new held value).
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Processes a stereo block in place.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        if self.mix.is_off() {
            // Bypass: only track the input history so a later enable starts from it.
            for i in n.saturating_sub(3)..n {
                self.ch[0].push(l[i]);
                self.ch[1].push(r[i]);
            }
            return;
        }
        let fs = self.fs as f64;
        let blep = self.blep;
        for i in 0..n {
            let wet_gain = self.mix.next_value();
            let bits = self.bits.next_value();
            let inc = self.rate.next_value() as f64 / fs;
            let x = [l[i], r[i]];
            let pos = self.pos;
            let mut y = [0f32; 2];
            if inc >= 1.0 {
                // Quantize only: the hold clock runs at or above the sample rate.
                self.phase = 0.0;
                let s = (bits - 1.0).exp2();
                let inv_s = 1.0 / s;
                for (c, st) in self.ch.iter_mut().enumerate() {
                    st.held = quantize_scaled(st.hist[2], s, inv_s);
                    st.push(x[c]);
                    // Residuals of steps rendered before the switch still play out.
                    y[c] = st.held + st.ring[pos];
                    st.ring[pos] = 0.0;
                }
                self.ticks = self.ticks.wrapping_add(1);
            } else {
                // Does the hold clock tick within (i − 1, i]? It ticks at i − 1 + τ and
                // samples the input one sample earlier, at i − 2 + τ.
                let next = self.phase + inc;
                let tick = next >= 1.0;
                let tau = if tick { ((1.0 - self.phase) / inc).clamp(0.0, 1.0) } else { 0.0 };
                self.phase = if tick { next - 1.0 } else { next };
                // Distance from the tick to output sample i, in [0, 1).
                let x0 = (1.0 - tau) as f32;
                let u = tau as f32;
                let (s, inv_s) = if tick {
                    self.ticks = self.ticks.wrapping_add(1);
                    let s = (bits - 1.0).exp2();
                    (s, 1.0 / s)
                } else {
                    (1.0, 1.0)
                };
                for (c, st) in self.ch.iter_mut().enumerate() {
                    if tick {
                        let [a, b, d] = st.hist;
                        let sampled = lagrange4(a, b, d, x[c], u);
                        let new = quantize_scaled(sampled, s, inv_s);
                        let h = new - st.held;
                        if h != 0.0 {
                            blep.add_step(&mut st.ring, pos, x0, h);
                            st.held = new;
                        }
                    }
                    st.push(x[c]);
                    y[c] = st.held + st.ring[pos];
                    st.ring[pos] = 0.0;
                }
            }
            self.pos = (pos + 1) & RING_MASK;
            if wet_gain >= 1.0 {
                l[i] = y[0];
                r[i] = y[1];
            } else {
                l[i] = x[0] + wet_gain * (y[0] - x[0]);
                r[i] = x[1] + wet_gain * (y[1] - x[1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::f64::consts::PI;

    fn settled(fs: f32, bits: f32, rate: f32) -> Crusher {
        let mut c = Crusher::new(fs);
        c.set_params(true, bits, rate);
        c.reset();
        c
    }

    #[test]
    fn level_counts_match_bit_depth() {
        for bits in 1..=10 {
            let b = bits as f32;
            let mut set = BTreeSet::new();
            for i in 0..=200_000 {
                let x = -1.2 + 2.4 * i as f32 / 200_000.0;
                let q = quantize(x, b);
                assert!((-1.0..=1.0).contains(&q));
                set.insert(q.to_bits());
            }
            assert_eq!(set.len() as u32, 1 << bits, "bits {bits}");
            assert_eq!(level_count(b), 1 << bits);
            // Mid-tread: zero is a level and small inputs map to it.
            assert_eq!(quantize(0.0, b), 0.0);
            assert_eq!(quantize(0.2 / (1 << bits) as f32, b), 0.0);
        }
        // Fractional depths interpolate the level count.
        let mut set = BTreeSet::new();
        for i in 0..=200_000 {
            set.insert(quantize(-1.0 + 2.0 * i as f32 / 200_000.0, 3.5).to_bits());
        }
        assert_eq!(set.len() as u32, level_count(3.5));
        assert_eq!(level_count(3.5), 11);
        assert_eq!(quantize(f32::NAN, 4.0), 0.0);
        assert_eq!(quantize(0.3, f32::NAN), quantize(0.3, 16.0));
    }

    #[test]
    fn quantize_only_when_rate_reaches_fs() {
        let fs = 48_000.0;
        let mut c = settled(fs, 4.0, 48_000.0);
        let mut l: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin() * 0.9).collect();
        let src = l.clone();
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        for i in 1..1000 {
            assert_eq!(l[i], quantize(src[i - 1], 4.0), "sample {i}");
        }
    }

    #[test]
    fn hold_period_is_exact_for_fractional_ratios() {
        // fs / 4.5: one new held value every 4.5 samples on average, exactly.
        let fs = 48_000.0;
        let mut c = settled(fs, 16.0, fs / 4.5);
        let n = 90_000;
        let mut l = vec![0.25f32; n];
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        assert_eq!(c.ticks(), 20_000);
        // Steady DC input: output equals the quantized DC exactly (polyBLEP residuals vanish).
        assert!(l[n - 10..].iter().all(|&y| y == quantize(0.25, 16.0)));
    }

    /// Spectral line amplitude of `x` at `f` (single-bin DFT, Hann window).
    fn amp_at(x: &[f32], f: f64, fs: f64) -> f64 {
        let n = x.len();
        let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
        for (i, &v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos();
            let ph = 2.0 * PI * f * i as f64 / fs;
            re += w * v as f64 * ph.cos();
            im += w * v as f64 * ph.sin();
            wsum += w;
        }
        2.0 * (re * re + im * im).sqrt() / wsum
    }

    #[test]
    fn fractional_hold_has_no_jitter_spurs() {
        // Hold at R = 2fs/9 (4.5-sample period). A grid-quantized hold alternates 4/5-sample
        // holds, i.e. its step times are phase-modulated at R/2 = fs/9, which puts jitter
        // sidebands at f0 ± fs/9. For an exact (fractional-time) hold, the only energy there
        // is the aliased 4th/5th ZOH image (f0 − 4R, f0 + 5R), which the polyBLEP kernel
        // attenuates by another ~35 dB. Note: every odd multiple of fs/9 coincides with *some*
        // legitimate image for this ratio (e.g. f0 + 7fs/9 ≡ R − f0), so only ±fs/9 is a clean
        // jitter probe.
        let fs = 48_000.0f64;
        let f0 = 750.0; // bin-exact for n = 48000
        let n = 48_000;
        let input: Vec<f32> = (0..n).map(|i| (0.5 * (2.0 * PI * f0 * i as f64 / fs).sin()) as f32).collect();
        let mut c = settled(fs as f32, 16.0, (fs * 2.0 / 9.0) as f32);
        let mut l = input.clone();
        let mut r = input.clone();
        c.process(&mut l, &mut r);
        // Naive grid hold for reference.
        let mut naive = vec![0f32; n];
        let mut held = 0f32;
        let mut phase = 0.0f64;
        for i in 0..n {
            phase += 2.0 / 9.0;
            if phase >= 1.0 {
                phase -= 1.0;
                held = input[i];
            }
            naive[i] = held;
        }
        let fund = amp_at(&l, f0, fs);
        let fund_naive = amp_at(&naive, f0, fs);
        let probes = [f0 + fs / 9.0, fs / 9.0 - f0];
        let ours = probes.iter().map(|&f| amp_at(&l, f, fs) / fund).fold(0.0, f64::max);
        let naive_spur = probes.iter().map(|&f| amp_at(&naive, f, fs) / fund_naive).fold(0.0, f64::max);
        let ours_db = 20.0 * ours.log10();
        let naive_db = 20.0 * naive_spur.log10();
        println!(
            "jitter sidebands (f0 ± fs/9): fractional minBLEP hold {ours_db:.1} dB, naive grid hold {naive_db:.1} dB"
        );
        assert!(ours_db < -100.0, "{ours_db}");
        assert!(ours_db < naive_db - 60.0, "{ours_db} vs {naive_db}");
        // The legitimate first image (R − f0) is present at the ZOH level f0/(R−f0).
        let rate = fs * 2.0 / 9.0;
        let img = amp_at(&l, rate - f0, fs) / fund;
        let expect = f0 / (rate - f0);
        println!("first S&H image R − f0: {:.2} dB (ideal ZOH {:.2} dB)", 20.0 * img.log10(), 20.0 * expect.log10());
        assert!((20.0 * (img / expect).log10()).abs() < 1.5, "image {img} vs {expect}");
    }

    #[test]
    fn sample_and_hold_follows_the_ideal_zoh_response() {
        // A real S&H has the ZOH response sinc(f/R); the band-limited rendering must not add
        // anything on top (a 2-point polyBLEP would add its own sinc² droop, −3.3 dB at
        // 16 kHz / 48 kHz, and linear sampling another −1.3 dB at 10 kHz).
        let fs = 48_000.0f64;
        // (rate, tone, tolerance dB): the cubic interpolator is essentially exact up to ≈ fs/8
        // and leaves a few tenths of a dB of extra roll-off towards fs/4 and above.
        let cases = [
            (11_025.0, 2_000.0, 0.02),
            (11_025.0, 5_000.0, 0.05),
            (22_050.0, 5_000.0, 0.05),
            (fs / 3.3, 5_000.0, 0.05),
            (22_050.0, 10_000.0, 0.5),
            (0.75 * fs, 12_000.0, 1.0),
            (0.95 * fs, 16_000.0, 2.0),
        ];
        for (rate, f, tol) in cases {
            let n = 48_000;
            let x: Vec<f32> = (0..n).map(|i| (0.5 * (2.0 * PI * f * i as f64 / fs).sin()) as f32).collect();
            let mut c = settled(fs as f32, 16.0, rate as f32);
            let mut l = x.clone();
            let mut r = x.clone();
            c.process(&mut l, &mut r);
            let got = amp_at(&l[4800..], f, fs) / amp_at(&x[4800..], f, fs);
            let zoh = {
                let a = PI * f / rate;
                a.sin() / a
            };
            let err = 20.0 * (got / zoh).log10();
            println!(
                "S&H at {rate:.0} Hz, {f} Hz tone: {:.3} dB vs ideal ZOH {:.3} dB ({err:+.3} dB)",
                20.0 * got.log10(),
                20.0 * zoh.log10()
            );
            assert!(err.abs() < tol, "rate {rate} f {f}: {err} dB off the ZOH response");
        }
        // Quantize-only (rate ≥ fs) and 16 bits: output equals the input delayed by exactly
        // one sample, to 16-bit accuracy.
        let mut c = settled(fs as f32, 16.0, 48_000.0);
        let x: Vec<f32> = (0..4800).map(|i| (0.5 * (2.0 * PI * 3000.0 * i as f64 / fs).sin()) as f32).collect();
        let mut l = x.clone();
        let mut r = x.clone();
        c.process(&mut l, &mut r);
        assert!(l[1..].iter().zip(&x).all(|(a, b)| (a - b).abs() <= 1.0 / 65_536.0 + 1e-7));
    }

    #[test]
    fn off_is_exact_bypass_and_toggle_is_smooth() {
        let fs = 48_000.0;
        let mut c = Crusher::new(fs);
        c.set_params(false, 4.0, 8000.0);
        let mut l: Vec<f32> = (0..512).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let src = l.clone();
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        assert_eq!(l, src);
        assert!(!c.is_active());
        // Toggle on: the first output samples stay close to the dry signal (crossfade).
        c.set_params(true, 1.0, 2000.0);
        let mut l2: Vec<f32> = (512..1024).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let src2 = l2.clone();
        let mut r2 = l2.clone();
        c.process(&mut l2, &mut r2);
        assert!((l2[0] - src2[0]).abs() < 0.05);
        // Turn off again: after the 10 ms crossfade the output is the dry input again.
        c.set_params(false, 1.0, 2000.0);
        let mut l3: Vec<f32> = (0..2048).map(|i| (i as f32 * 0.05).cos() * 0.5).collect();
        let src3 = l3.clone();
        let mut r3 = l3.clone();
        c.process(&mut l3, &mut r3);
        assert!(!c.is_active());
        assert_eq!(l3[1000..], src3[1000..]);
    }

    #[test]
    fn rate_sweeps_stay_bounded_and_finite() {
        let fs = 44_100.0;
        let mut c = settled(fs, 3.0, 1000.0);
        let mut t = 0usize;
        for b in 0..400 {
            let rate = 1000.0 + (b as f32 * 0.05).sin().abs() * 60_000.0;
            c.set_params(true, 1.0 + (b % 16) as f32, rate);
            let mut l: Vec<f32> = (0..64).map(|i| ((t + i) as f32 * 0.03).sin()).collect();
            let mut r = l.clone();
            c.process(&mut l, &mut r);
            for &y in &l {
                assert!(y.is_finite() && y.abs() <= 1.6, "{y}");
            }
            t += 64;
        }
    }
}
