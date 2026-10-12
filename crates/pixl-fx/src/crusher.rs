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
//! (2 bits = {−1, −½, 0, ½}, …).
//!
//! **Relative to the signal's tracked level.** NES/GB with `analog` on feed unipolar DAC
//! levels (several channels reach +2…+4) and the crusher runs before the DC-removing chip
//! filter, so a converter range fixed at `[−1, 1)` would clip them even at 16 bits. Each
//! channel therefore tracks its input range `[lo, hi]` (instant attack, release towards the
//! input with a 5 Hz one-pole, [`TRACK_HZ`]) and the converter's `±1` window is centred on the
//! grid level nearest to the range centre `m = (hi + lo)/2`. The levels themselves stay on the
//! fixed grid `code · Δ`, so silence is still exactly zero and a moving reference never adds
//! steps of its own; only signals whose swing around their centre exceeds `±1` are clamped
//! (like any signal in FREE). Crush at 16 bits is transparent for every chip signal.
//!
//! **Below 2 bits** the mid-tread quantizer is crossfaded (weight `2 − bits`) into a symmetric
//! mid-rise *sign* quantizer around the range centre, `m ± Δ/2`, so 1 bit is a square of
//! `±Δ/2` — the harshest setting — and never silence (a 1-bit mid-tread converter has only
//! the levels {−1, 0} and would swallow every signal that stays above −½). Two refinements
//! keep it clean: the square never exceeds the input's own range (`±min(Δ/2, half-range)`),
//! so a unipolar note ends exactly at its rest level instead of being pushed below it (no
//! thump), and within 1/16 of the half-range around the centre ([`SIGN_SOFT`]) the sign is
//! linear, so decaying tails and rounding noise fade out instead of turning into
//! full-scale noise.
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
/// Release of the per-channel range tracker that centres the quantizer (Hz, one-pole).
pub const TRACK_HZ: f32 = 5.0;
/// Width of the linear zone of the sign quantizer, as a fraction of the half-range.
pub const SIGN_SOFT: f32 = 1.0 / 16.0;

/// The plain mid-tread converter curve with `2^bits` levels around a zero reference (see the
/// module docs): what the crusher applies to a signal centred on zero at 2 bits or more.
/// `bits` is clamped to `1..=16`; non-finite input yields 0.
#[inline]
pub fn quantize(x: f32, bits: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    let q = Levels::new(bits);
    // `+ 0.0` turns a rounded −0.0 into +0.0 so silence is bit-exact zero.
    (x * q.s).round().clamp(q.lo, q.hi) * q.inv_s + 0.0
}

/// A bit depth prepared for quantization (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Levels {
    /// `S = 2^(bits−1)`.
    s: f32,
    /// `Δ = 1/S`.
    inv_s: f32,
    /// Code range around the reference code: `−floor(S) ..= ceil(S) − 1`.
    lo: f32,
    hi: f32,
    /// Weight of the sign quantizer (`2 − bits`, clamped to `0..=1`).
    sign: f32,
}

impl Levels {
    #[inline]
    fn new(bits: f32) -> Self {
        let bits = if bits.is_finite() { bits.clamp(MIN_BITS, MAX_BITS) } else { MAX_BITS };
        let s = (bits - 1.0).exp2();
        Self { s, inv_s: 1.0 / s, lo: -s.floor(), hi: s.ceil() - 1.0, sign: (2.0 - bits).clamp(0.0, 1.0) }
    }
}

/// Quantizes `x` relative to an input range `[lo, hi]` (see the module docs): mid-tread on
/// the fixed grid with its `±1` window centred on the grid level nearest to the range centre,
/// crossfaded below 2 bits into a sign quantizer around the centre. Non-finite input yields
/// the centre.
#[inline]
fn quantize_tracked(x: f32, q: &Levels, lo: f32, hi: f32) -> f32 {
    let m = 0.5 * (hi + lo);
    if !x.is_finite() {
        return m;
    }
    let centre = (m * q.s).round();
    let code = (x * q.s).round().clamp(centre + q.lo, centre + q.hi);
    // `+ 0.0` turns a rounded −0.0 into +0.0 so silence is bit-exact zero.
    let tread = code * q.inv_s + 0.0;
    if q.sign <= 0.0 {
        return tread;
    }
    let half = 0.5 * (hi - lo);
    let amp = (0.5 * q.inv_s).min(half);
    let sign = if half > 0.0 { ((x - m) / (SIGN_SOFT * half)).clamp(-1.0, 1.0) } else { 0.0 };
    let rise = m + sign * amp;
    tread + q.sign * (rise - tread)
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
    /// Tracked input range (instant attack, [`TRACK_HZ`] release towards the input).
    lo: f32,
    hi: f32,
}

impl Default for HoldState {
    fn default() -> Self {
        Self { hist: [0.0; 3], held: 0.0, ring: [0.0; RING_LEN], lo: 0.0, hi: 0.0 }
    }
}

impl HoldState {
    /// Shifts a new input sample into the history.
    #[inline]
    fn push(&mut self, x: f32) {
        self.hist = [self.hist[1], self.hist[2], x];
    }

    /// Updates the range tracker with a new input sample (`k` = release coefficient).
    #[inline]
    fn track(&mut self, x: f32, k: f32) {
        if !x.is_finite() {
            return;
        }
        self.hi = if x >= self.hi { x } else { self.hi + k * (x - self.hi) };
        self.lo = if x <= self.lo { x } else { self.lo + k * (x - self.lo) };
    }

    /// Restarts the range tracker from the input history (waking from bypass).
    fn restart_tracker(&mut self) {
        let [a, b, c] = self.hist;
        self.hi = a.max(b).max(c);
        self.lo = a.min(b).min(c);
    }

    #[inline]
    fn quantize(&self, x: f32, q: &Levels) -> f32 {
        quantize_tracked(x, q, self.lo, self.hi)
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
    /// Release coefficient of the range trackers (per sample).
    track_k: f32,
    /// Number of hold clock ticks since reset (diagnostics/tests).
    ticks: u64,
}

/// Per-sample release coefficient of a [`TRACK_HZ`] one-pole at `fs`.
fn track_coefficient(fs: f32) -> f32 {
    let fs = if fs.is_finite() && fs > 0.0 { fs } else { 48_000.0 };
    1.0 - (-2.0 * std::f32::consts::PI * TRACK_HZ / fs).exp()
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
            track_k: track_coefficient(fs),
            ticks: 0,
        }
    }

    /// Updates the sample rate (keeps state).
    pub fn set_sample_rate(&mut self, fs: f32) {
        self.fs = fs;
        self.track_k = track_coefficient(fs);
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
            let q = Levels::new(bits);
            for c in &mut self.ch {
                c.ring = [0.0; RING_LEN];
                c.restart_tracker();
                c.held = c.quantize(c.hist[2], &q);
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
        let k = self.track_k;
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
                let q = Levels::new(bits);
                for (c, st) in self.ch.iter_mut().enumerate() {
                    st.held = st.quantize(st.hist[2], &q);
                    st.track(x[c], k);
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
                let q = if tick {
                    self.ticks = self.ticks.wrapping_add(1);
                    Some(Levels::new(bits))
                } else {
                    None
                };
                for (c, st) in self.ch.iter_mut().enumerate() {
                    st.track(x[c], k);
                    if let Some(q) = &q {
                        let [a, b, d] = st.hist;
                        let sampled = lagrange4(a, b, d, x[c], u);
                        let new = st.quantize(sampled, q);
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

    /// A unipolar "chip" signal: two DAC pulses (0..0.8 each, 220 / 331 Hz) summed, like a
    /// two-channel NES/GB patch with `analog` on (range 0..1.6).
    fn unipolar_chord(n: usize, fs: f64) -> Vec<f32> {
        let pulse = |f: f64, i: usize, duty: f64| if (f * i as f64 / fs).fract() < duty { 0.8f32 } else { 0.0 };
        (0..n).map(|i| pulse(220.0, i, 0.5) + pulse(331.0, i, 0.25)).collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    #[test]
    fn sixteen_bits_are_transparent_for_unipolar_chip_signals() {
        // SPEC §5.2 (FX-01): NES/GB with `analog` on feed unipolar DAC levels well above +1
        // into the crusher (it runs before the DC-removing chip filter). Quantization is
        // relative to the tracked signal level, so 16 bits never clip them — from the very
        // first sample of a note after silence, with or without the sample-and-hold.
        let fs = 48_000.0;
        let n = 24_000;
        let mut x = vec![0.0f32; 480];
        x.extend(unipolar_chord(n, fs));
        for rate in [48_000.0f32, 11_025.0] {
            let mut c = settled(fs as f32, 16.0, rate);
            let mut l = x.clone();
            let mut r = x.clone();
            c.process(&mut l, &mut r);
            let (dry, wet) = (rms(&x[480..]), rms(&l[481..]));
            let db = 20.0 * (wet / dry).log10();
            assert!(db.abs() < 0.1, "rate {rate}: crush at 16 bits changes the level by {db:.2} dB");
            let peak = l.iter().fold(0.0f32, |m, v| m.max(*v));
            assert!(peak > 1.55, "rate {rate}: the 1.6 peaks are clipped ({peak})");
            if rate >= 48_000.0 {
                // Quantize only: the input delayed by one sample, to 16-bit accuracy.
                let err = l[1..].iter().zip(&x).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
                assert!(err <= 1.0 / 65_536.0 + 1e-6, "max error {err}");
            }
        }
        // Switching crush on and off while the chord plays: during the 10 ms crossfades the
        // output only blends the dry sample with the one-sample-late wet one (no clipped
        // level, no click).
        let mut c = Crusher::new(fs as f32);
        let mut prev = 0.0f32;
        for (b, chunk) in x.chunks(64).enumerate() {
            c.set_params((100..250).contains(&b), 16.0, 48_000.0);
            let mut l = chunk.to_vec();
            let mut r = chunk.to_vec();
            c.process(&mut l, &mut r);
            for (i, y) in l.iter().enumerate() {
                let before = if i == 0 { prev } else { chunk[i - 1] };
                let (lo, hi) = (before.min(chunk[i]), before.max(chunk[i]));
                assert!(*y >= lo - 2e-5 && *y <= hi + 2e-5, "block {b} sample {i}: {y} outside [{lo}, {hi}]");
            }
            prev = chunk[chunk.len() - 1];
        }
        // Silence stays exactly silent, before and after the notes.
        let mut c = settled(fs as f32, 4.0, 48_000.0);
        let mut l = x.clone();
        l.extend(vec![0.0f32; 9_600]);
        let mut r = l.clone();
        c.process(&mut l, &mut r);
        assert!(l[..480].iter().all(|v| *v == 0.0));
        assert!(l[l.len() - 4_800..].iter().all(|v| *v == 0.0), "quantization residue in the silence after the notes");
    }

    #[test]
    fn one_bit_is_a_square_never_silence() {
        // SPEC §5.2 (FX-02): below 2 bits the mid-tread quantizer (1 bit = {−1, 0}, which turns
        // anything that stays above −½ into silence) crossfades into a sign quantizer, so 1 bit
        // is a ±Δ/2 square — for bipolar and unipolar signals alike.
        let fs = 48_000.0f64;
        let n = 48_000;
        let sine = |a: f64| (0..n).map(|i| (a * (2.0 * PI * 440.0 * i as f64 / fs).sin()) as f32).collect::<Vec<_>>();
        let crush = |x: &[f32], bits: f32| {
            let mut c = settled(fs as f32, bits, 48_000.0);
            let mut l = x.to_vec();
            let mut r = x.to_vec();
            c.process(&mut l, &mut r);
            rms(&l[4_800..])
        };
        let s = sine(0.4);
        let one = crush(&s, 1.0);
        // A square at the sine's peak level (±Δ/2 = ±½ is capped to the input's own range).
        assert!(one > 0.37, "±0.4 sine at 1 bit: RMS {one}");
        // Louder at lower depths over whole bits; between 1 and 2 bits the crossfade never
        // falls more than 3 dB below the 2-bit level (before: silence from 1.32 bits down).
        let levels: Vec<f64> = (0..=16).map(|k| crush(&s, 1.0 + k as f32 / 8.0)).collect();
        println!("±0.4 sine RMS over 1..3 bits in 1/8 steps: {levels:.3?}");
        assert!(levels[0] > levels[8] && levels[8] > levels[16], "{levels:?}");
        assert!(levels[..=8].iter().all(|v| *v > 0.7 * levels[8]), "{levels:?}");
        // A unipolar chip signal (and a quiet one) at 1 bit: a square around its centre.
        let u = unipolar_chord(n, fs);
        let du = crush(&u, 1.0);
        assert!(du > 0.3, "unipolar chord at 1 bit: AC RMS {du}");
        let quiet = sine(0.05);
        assert!(crush(&quiet, 1.0) > 0.04);
    }

    #[test]
    fn low_bit_tails_end_without_thumps_or_noise() {
        // A unipolar note that stops (the DAC returns to 0), and a bipolar note whose release
        // decays into rounding noise: at 1–2 bits the output must settle smoothly to silence,
        // never jump to a held offset or turn the residue into a full-scale buzz.
        let fs = 48_000.0f64;
        let n = 19_200;
        let mut uni = unipolar_chord(n, fs);
        uni.extend(vec![0.0f32; 24_000]);
        let mut bip: Vec<f32> = (0..n)
            .map(|i| {
                let env = 1.0 - i as f64 / n as f64;
                (0.6 * env * (2.0 * PI * 330.0 * i as f64 / fs).sin()) as f32
            })
            .collect();
        bip.extend((0..24_000).map(|i| if i % 2 == 0 { 1e-7 } else { -1e-7 }));
        for bits in [1.0f32, 1.5, 2.0] {
            for (name, x) in [("unipolar", &uni), ("bipolar", &bip)] {
                let mut c = settled(fs as f32, bits, 48_000.0);
                let mut l = x.clone();
                let mut r = x.clone();
                c.process(&mut l, &mut r);
                let tail = &l[n + 2..];
                let jump = tail.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
                assert!(jump < 0.02, "{name} at {bits} bits: step of {jump} after the note");
                let last = tail[tail.len() - 4_800..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
                assert!(last < 1e-4, "{name} at {bits} bits: {last} left after 0.4 s of silence");
            }
        }
    }
}
