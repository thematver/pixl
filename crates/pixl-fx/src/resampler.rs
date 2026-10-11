//! Streaming arbitrary-ratio polyphase resampler (Kaiser-windowed sinc).
//!
//! Used by the echo to move between the host rate (22.05–192 kHz) and the SPC700's
//! 32 kHz. The rate ratio is reduced to an exact fraction `P/Q` (input samples per output
//! sample), so the polyphase table has exactly `Q` rows and timing never drifts. Non-standard
//! host rates whose exact fraction would need more than [`MAX_PHASES`] rows use the best
//! rational approximation instead (the internal rate is then off by at most 0.05 %, which
//! only scales the echo time by the same amount, and both directions use the exact inverse
//! ratio, so the round trip has no pitch error and never drifts).
//!
//! Filter design (relative to the lower of the two rates, `fl`):
//! passband edge `0.4375·fl` (14 kHz at 32 kHz), stopband edge `0.5·fl` (16 kHz),
//! cutoff in the middle, Kaiser β = [`KAISER_BETA`], [`TAPS_LOW`] taps measured at `fl`
//! (more input taps when decimating). Every polyphase row is normalized to unity DC gain.
//! Measured (see tests): passband ripple < ±0.001 dB to 14 kHz, ≥ 80 dB rejection at and
//! above 16 kHz, round-trip THD+N ≈ −100 dB.
//!
//! Output `m` is centred exactly on input position `m·P/Q` (linear phase, no fractional
//! offset); an optional extra delay of whole input samples can be added at construction.

use crate::blep::{bessel_i0, sinc};
use std::f64::consts::PI;
use std::sync::Arc;

/// Taps per output at the lower of the two rates.
pub const TAPS_LOW: usize = 96;
/// Kaiser window β (≈ 94 dB sidelobe attenuation).
pub const KAISER_BETA: f64 = 9.4;
/// Passband edge as a fraction of the lower rate.
pub const PASS_EDGE: f64 = 0.4375;
/// Stopband edge as a fraction of the lower rate.
pub const STOP_EDGE: f64 = 0.5;
/// Maximum number of polyphase rows (exact ratios of all standard rates need ≤ 640; odd
/// rates use the best approximation with ≤ 1024 rows, ≤ 0.05 % off — this bounds a table
/// to ≤ 2.4 MB even for a 190 kHz down-sampler).
pub const MAX_PHASES: u64 = 1024;

/// A reduced rate ratio: `num / den` input samples per output sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ratio {
    /// Numerator (input samples).
    pub num: u64,
    /// Denominator (output samples) = number of polyphase rows.
    pub den: u64,
}

impl Ratio {
    /// The ratio `in_rate / out_rate`, exact when both terms fit in [`MAX_PHASES`],
    /// otherwise the best rational approximation with both terms ≤ `MAX_PHASES`.
    pub fn new(in_rate: u64, out_rate: u64) -> Self {
        let (a, b) = (in_rate.max(1), out_rate.max(1));
        let g = gcd(a, b);
        let (n, d) = (a / g, b / g);
        if n <= MAX_PHASES && d <= MAX_PHASES {
            return Self { num: n, den: d };
        }
        best_rational(n, d, MAX_PHASES)
    }

    /// The inverse ratio.
    pub fn inverse(self) -> Self {
        Self { num: self.den, den: self.num }
    }

    /// Ratio as a float.
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.max(1)
}

/// Best rational approximation of `n/d` with numerator and denominator ≤ `limit`
/// (continued-fraction convergents and semiconvergents).
fn best_rational(n: u64, d: u64, limit: u64) -> Ratio {
    let target = n as f64 / d.max(1) as f64;
    let err = |p: u64, q: u64| ((p as f64 / q as f64) / target - 1.0).abs();
    let mut best = Ratio { num: (target.round() as u64).clamp(1, limit), den: 1 };
    let mut best_err = err(best.num, best.den);
    let (mut p0, mut q0, mut p1, mut q1) = (0u64, 1u64, 1u64, 0u64);
    let (mut a, mut b) = (n, d);
    while b != 0 {
        let k = a / b;
        // Largest multiplier j ≤ k keeping both terms within the limit.
        let jp = limit.saturating_sub(p0).checked_div(p1).unwrap_or(k);
        let jq = limit.saturating_sub(q0).checked_div(q1).unwrap_or(k);
        let j = k.min(jp).min(jq);
        if j >= 1 {
            let (p, q) = (j * p1 + p0, j * q1 + q0);
            if p >= 1 && q >= 1 {
                let e = err(p, q);
                if e < best_err {
                    best = Ratio { num: p, den: q };
                    best_err = e;
                }
            }
        }
        if j < k {
            break;
        }
        (p0, q0, p1, q1) = (p1, q1, k * p1 + p0, k * q1 + q0);
        let r = a % b;
        a = b;
        b = r;
    }
    best
}

/// Streaming polyphase resampler (one channel; clone it for more channels — the coefficient
/// table is shared).
#[derive(Clone, Debug)]
pub struct Resampler {
    ratio: Ratio,
    /// Whole input samples advanced per output.
    step_int: usize,
    /// Fractional advance per output, in rows (`num mod den`).
    step_frac: u64,
    taps: usize,
    /// `den` rows × `taps` coefficients (shared between channels: cloning a resampler only
    /// duplicates its history, never the table).
    table: Arc<[f32]>,
    /// Input history; `hist[pos..pos + taps]` is the window of the next output.
    hist: Vec<f32>,
    len: usize,
    pos: usize,
    row: u64,
    /// Zeros pre-filled on reset (`taps/2 − 1` centring + extra delay).
    prefill: usize,
    /// Outputs that could not be computed because input was missing (should stay 0).
    starved: u64,
}

impl Resampler {
    /// Creates a resampler from `in_rate` to `out_rate` with the given ratio (use
    /// [`Ratio::new`] or the inverse of a partner's ratio), accepting up to `max_push`
    /// input samples per [`push`](Self::push) and adding `extra_delay` input samples of delay.
    /// Allocates.
    pub fn new(ratio: Ratio, in_rate: f64, out_rate: f64, max_push: usize, extra_delay: usize) -> Self {
        let low = in_rate.min(out_rate);
        let decim = (in_rate / out_rate).max(1.0);
        // Taps in input samples, rounded up to a multiple of 8 (vector-friendly, even).
        let taps = (((TAPS_LOW as f64 * decim) / 8.0).ceil() as usize * 8).max(8);
        let fc = 0.5 * (PASS_EDGE + STOP_EDGE) * low / in_rate; // cycles per input sample
        let den = ratio.den.max(1);
        let half = taps as f64 / 2.0;
        let i0b = bessel_i0(KAISER_BETA);
        let mut table = vec![0f32; den as usize * taps];
        for row in 0..den as usize {
            let frac = row as f64 / den as f64;
            let mut coeffs = vec![0f64; taps];
            let mut sum = 0.0;
            for (j, c) in coeffs.iter_mut().enumerate() {
                // Distance of tap j from the output position, in input samples.
                let t = frac + half - 1.0 - j as f64;
                let u = t / half;
                let w = if u.abs() >= 1.0 { 0.0 } else { bessel_i0(KAISER_BETA * (1.0 - u * u).sqrt()) / i0b };
                *c = 2.0 * fc * sinc(2.0 * fc * t) * w;
                sum += *c;
            }
            let norm = if sum.abs() > 1e-12 { 1.0 / sum } else { 1.0 };
            for (dst, c) in table[row * taps..(row + 1) * taps].iter_mut().zip(&coeffs) {
                *dst = (c * norm) as f32;
            }
        }
        let prefill = taps / 2 - 1 + extra_delay;
        let cap = taps + prefill + max_push + 64;
        let mut r = Self {
            ratio,
            step_int: (ratio.num / den) as usize,
            step_frac: ratio.num % den,
            taps,
            table: table.into(),
            hist: vec![0.0; cap],
            len: 0,
            pos: 0,
            row: 0,
            prefill,
            starved: 0,
        };
        r.reset();
        r
    }

    /// Clears the history (re-applies the centring/extra-delay prefill).
    pub fn reset(&mut self) {
        self.hist.iter_mut().for_each(|x| *x = 0.0);
        self.len = self.prefill.min(self.hist.len());
        self.pos = 0;
        self.row = 0;
        self.starved = 0;
    }

    /// Number of taps (input samples per output computation).
    pub fn taps(&self) -> usize {
        self.taps
    }

    /// The ratio in use.
    pub fn ratio(&self) -> Ratio {
        self.ratio
    }

    /// Outputs that had to be zero-filled because input was missing (0 in correct use).
    pub fn starved(&self) -> u64 {
        self.starved
    }

    /// Number of outputs that can be pulled right now.
    pub fn available(&self) -> usize {
        if self.pos + self.taps > self.len {
            return 0;
        }
        // Simulate the position advance (cheap: integer arithmetic).
        let mut n = 0;
        let (mut pos, mut row) = (self.pos, self.row);
        while pos + self.taps <= self.len {
            n += 1;
            pos += self.step_int;
            row += self.step_frac;
            if row >= self.ratio.den {
                row -= self.ratio.den;
                pos += 1;
            }
        }
        n
    }

    /// Appends input samples. Input beyond the configured capacity is dropped (never
    /// happens when `max_push` is respected).
    pub fn push(&mut self, input: &[f32]) {
        if self.len + input.len() > self.hist.len() {
            self.compact();
        }
        let room = self.hist.len().saturating_sub(self.len);
        let n = input.len().min(room);
        debug_assert!(n == input.len(), "resampler capacity exceeded");
        self.hist[self.len..self.len + n].copy_from_slice(&input[..n]);
        self.len += n;
    }

    /// Moves the live part of the history to the front.
    fn compact(&mut self) {
        let start = self.pos.min(self.len);
        if start > 0 {
            self.hist.copy_within(start..self.len, 0);
            self.len -= start;
            self.pos -= start;
        }
    }

    /// Produces up to `out.len()` outputs from the available input; returns how many.
    pub fn pull(&mut self, out: &mut [f32]) -> usize {
        let taps = self.taps;
        let mut n = 0;
        while n < out.len() && self.pos + taps <= self.len {
            let row = self.row as usize * taps;
            out[n] = dot(&self.table[row..row + taps], &self.hist[self.pos..self.pos + taps]);
            n += 1;
            self.advance();
        }
        n
    }

    /// Produces exactly `out.len()` outputs; outputs whose input is not available yet are
    /// zero (and counted in [`starved`](Self::starved)) while the timeline still advances.
    pub fn pull_exact(&mut self, out: &mut [f32]) {
        let taps = self.taps;
        for y in out.iter_mut() {
            if self.pos + taps <= self.len {
                let row = self.row as usize * taps;
                *y = dot(&self.table[row..row + taps], &self.hist[self.pos..self.pos + taps]);
            } else {
                *y = 0.0;
                self.starved = self.starved.wrapping_add(1);
            }
            self.advance();
        }
        if self.pos > self.len {
            // Starved past the end: drop the missing span so the buffer stays consistent.
            self.hist.iter_mut().for_each(|x| *x = 0.0);
            self.len = 0;
            self.pos = 0;
        }
    }

    #[inline]
    fn advance(&mut self) {
        self.pos += self.step_int;
        self.row += self.step_frac;
        if self.row >= self.ratio.den {
            self.row -= self.ratio.den;
            self.pos += 1;
        }
    }

    /// Magnitude response of the prototype (row 0) at `f` Hz given the input rate, for tests.
    pub fn magnitude(&self, f: f64, in_rate: f64) -> f64 {
        // Evaluate the full (interleaved) prototype: coefficient of tap j in row r sits at
        // time t = r/den + half − 1 − j input samples.
        let half = self.taps as f64 / 2.0;
        let (mut re, mut im) = (0.0, 0.0);
        let den = self.ratio.den as usize;
        for r in 0..den {
            for j in 0..self.taps {
                let t = r as f64 / den as f64 + half - 1.0 - j as f64;
                let c = self.table[r * self.taps + j] as f64 / den as f64;
                let ph = 2.0 * PI * f * t / in_rate;
                re += c * ph.cos();
                im += c * ph.sin();
            }
        }
        (re * re + im * im).sqrt()
    }
}

/// Dot product with eight independent accumulators (vectorizes; fixed summation order keeps
/// results deterministic).
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut acc = [0f32; 8];
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    for (x, y) in ca.iter().zip(cb) {
        for ((s, xa), yb) in acc.iter_mut().zip(x).zip(y) {
            *s += xa * yb;
        }
    }
    let mut s = ((acc[0] + acc[4]) + (acc[1] + acc[5])) + ((acc[2] + acc[6]) + (acc[3] + acc[7]));
    for (x, y) in ra.iter().zip(rb) {
        s += x * y;
    }
    s
}

/// A matched host → 32 kHz → host resampler pair for one channel, with the fixed extra delay
/// that guarantees the up-sampler never starves for any block size.
#[derive(Clone, Debug)]
pub struct RoundTrip {
    /// Host → internal.
    pub down: Resampler,
    /// Internal → host.
    pub up: Resampler,
    latency_internal: usize,
}

impl RoundTrip {
    /// Builds the pair for `host_rate` ↔ `internal_rate` handling host blocks of up to
    /// `max_block` samples. Allocates.
    pub fn new(host_rate: u32, internal_rate: u32, max_block: usize) -> Self {
        let ratio = Ratio::new(host_rate as u64, internal_rate as u64);
        let (h, i) = (host_rate as f64, internal_rate as f64);
        let max_internal = (max_block as f64 * i / h).ceil() as usize + 4;
        let down = Resampler::new(ratio, h, i, max_block, 0);
        // Required delay in internal samples: half the up-sampler span plus the
        // down-sampler half span expressed at the internal rate, plus rounding margin.
        let probe_up = Resampler::new(ratio.inverse(), i, h, max_internal, 0);
        let z = (probe_up.taps() as f64 / 2.0 + down.taps() as f64 / 2.0 * i / h).ceil() as usize + 2;
        let up = Resampler::new(ratio.inverse(), i, h, max_internal, z);
        Self { down, up, latency_internal: z }
    }

    /// Total round-trip latency in internal-rate samples (exact: the chain is linear phase).
    pub fn latency_internal(&self) -> usize {
        self.latency_internal
    }

    /// Resets both directions.
    pub fn reset(&mut self) {
        self.down.reset();
        self.up.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATES: [u32; 5] = [44_100, 48_000, 88_200, 96_000, 192_000];

    fn sine(f: f64, fs: f64, n: usize, amp: f64) -> Vec<f32> {
        (0..n).map(|i| (amp * (2.0 * PI * f * i as f64 / fs).sin()) as f32).collect()
    }

    /// Least-squares fit of a sine at known `f`: returns (amplitude, phase, residual rms).
    fn fit(x: &[f32], f: f64, fs: f64) -> (f64, f64, f64) {
        let (mut ss, mut sc, mut cc, mut sy, mut cy) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (i, &y) in x.iter().enumerate() {
            let ph = 2.0 * PI * f * i as f64 / fs;
            let (s, c) = ph.sin_cos();
            ss += s * s;
            sc += s * c;
            cc += c * c;
            sy += s * y as f64;
            cy += c * y as f64;
        }
        let det = ss * cc - sc * sc;
        let a = (sy * cc - cy * sc) / det;
        let b = (cy * ss - sy * sc) / det;
        let mut res = 0.0;
        for (i, &y) in x.iter().enumerate() {
            let ph = 2.0 * PI * f * i as f64 / fs;
            let e = y as f64 - (a * ph.sin() + b * ph.cos());
            res += e * e;
        }
        ((a * a + b * b).sqrt(), b.atan2(a), (res / x.len() as f64).sqrt())
    }

    /// Runs a signal through a round trip in blocks of varying size.
    fn round_trip(fs: u32, input: &[f32]) -> (Vec<f32>, RoundTrip) {
        let mut rt = RoundTrip::new(fs, 32_000, 1024);
        let mut out = vec![0f32; input.len()];
        let mut mid = vec![0f32; 2048];
        let sizes = [1usize, 7, 64, 333, 1024, 128, 2, 999];
        let mut i = 0;
        let mut k = 0;
        while i < input.len() {
            let n = sizes[k % sizes.len()].min(input.len() - i);
            k += 1;
            rt.down.push(&input[i..i + n]);
            let m = rt.down.pull(&mut mid);
            rt.up.push(&mid[..m]);
            rt.up.pull_exact(&mut out[i..i + n]);
            i += n;
        }
        assert_eq!(rt.up.starved(), 0, "up-sampler starved at {fs} Hz");
        (out, rt)
    }

    #[test]
    fn ratios_are_exact_for_standard_rates() {
        assert_eq!(Ratio::new(44_100, 32_000), Ratio { num: 441, den: 320 });
        assert_eq!(Ratio::new(48_000, 32_000), Ratio { num: 3, den: 2 });
        assert_eq!(Ratio::new(192_000, 32_000), Ratio { num: 6, den: 1 });
        assert_eq!(Ratio::new(22_050, 32_000), Ratio { num: 441, den: 640 });
        // Every integer host rate in the supported range gets a ratio within the table limit;
        // standard rates are exact, odd ones at most 0.05 % off (the echo time then differs by
        // the same 0.05 %; there is no pitch error because both directions are exact inverses).
        let mut worst = 0f64;
        let mut worst_rate = 0;
        for fs in 22_050u64..=192_000 {
            let r = Ratio::new(fs, 32_000);
            assert!(r.num >= 1 && r.den >= 1 && r.num <= MAX_PHASES && r.den <= MAX_PHASES, "{fs}: {r:?}");
            let ppm = (r.as_f64() / (fs as f64 / 32_000.0) - 1.0).abs() * 1e6;
            if ppm > worst {
                worst = ppm;
                worst_rate = fs;
            }
        }
        println!("worst ratio approximation over 22050..=192000 Hz: {worst:.3} ppm at {worst_rate} Hz");
        assert!(worst < 600.0, "{worst} ppm at {worst_rate}");
    }

    #[test]
    fn prototype_meets_passband_and_stopband_specs() {
        for fs in RATES.iter().copied().chain([22_050, 32_001]) {
            let ratio = Ratio::new(fs as u64, 32_000);
            for (dir, r, fin, fout) in
                [("down", ratio, fs as f64, 32_000.0), ("up", ratio.inverse(), 32_000.0, fs as f64)]
            {
                let rs = Resampler::new(r, fin, fout, 1024, 0);
                let low = fin.min(fout);
                let mut ripple: f64 = 0.0;
                let mut f = 10.0;
                while f <= PASS_EDGE * low {
                    ripple = ripple.max((20.0 * rs.magnitude(f, fin).log10()).abs());
                    f += 97.0;
                }
                let mut stop: f64 = -300.0;
                let mut f = STOP_EDGE * low;
                while f < fin.max(fout) / 2.0 {
                    stop = stop.max(20.0 * rs.magnitude(f, fin).log10());
                    f += 37.0;
                }
                println!(
                    "{fs} Hz {dir}: {} taps, {} phases, passband ripple ±{ripple:.5} dB, stopband ≤ {stop:.1} dB",
                    rs.taps(),
                    r.den
                );
                assert!(ripple < 0.01, "{fs} {dir}: ripple {ripple}");
                assert!(stop < -80.0, "{fs} {dir}: stopband {stop}");
            }
        }
    }

    #[test]
    fn worst_case_gain_is_below_4() {
        // The echo's silence detection relies on input below 2^-17 staying below one 16-bit
        // LSB (2^-15) after the down-sampler: every row's ℓ1 norm must be < 4.
        let mut worst = 0f64;
        for fs in RATES.iter().copied().chain([22_050, 176_400, 32_000]) {
            let ratio = Ratio::new(fs as u64, 32_000);
            let rs = Resampler::new(ratio, fs as f64, 32_000.0, 64, 0);
            for row in rs.table.chunks_exact(rs.taps) {
                worst = worst.max(row.iter().map(|c| c.abs() as f64).sum());
            }
        }
        println!("worst down-sampler row ℓ1 norm: {worst:.3}");
        assert!(worst < 4.0, "{worst}");
    }

    #[test]
    fn round_trip_thd_n_and_passband() {
        for fs in RATES.iter().copied().chain([22_050, 176_400]) {
            let fsf = fs as f64;
            let n = fs as usize; // one second
            let mut worst_thdn: f64 = -300.0;
            let mut worst_ripple: f64 = 0.0;
            // Test tones across the pass band of this rate pair (to 14 kHz, or 0.4375·fs).
            let edge = PASS_EDGE * fsf.min(32_000.0);
            for f in [100.0, 1000.0, 3000.0, 7000.0, 9_600.0, 10_000.0, 12_500.0, 14_000.0]
                .into_iter()
                .filter(|&f| f <= edge)
            {
                let x = sine(f, fsf, n, 0.5);
                let (y, rt) = round_trip(fs, &x);
                // Skip start-up; compare the steady part.
                let skip = (0.05 * fsf) as usize;
                let (amp, _, res) = fit(&y[skip..], f, fsf);
                let thdn = 20.0 * (res / (amp / 2f64.sqrt())).log10();
                worst_thdn = worst_thdn.max(thdn);
                worst_ripple = worst_ripple.max((20.0 * (amp / 0.5).log10()).abs());
                assert!(rt.latency_internal() < 200);
            }
            println!("round trip {fs} Hz: worst THD+N {worst_thdn:.1} dB, passband ripple ±{worst_ripple:.4} dB");
            assert!(worst_thdn < -85.0, "{fs}: THD+N {worst_thdn}");
            assert!(worst_ripple < 0.01, "{fs}: ripple {worst_ripple}");
        }
    }

    #[test]
    fn round_trip_latency_is_exact() {
        for fs in RATES {
            let fsf = fs as f64;
            let f = 1000.0;
            let x = sine(f, fsf, fs as usize / 2, 0.5);
            let (y, rt) = round_trip(fs, &x);
            let skip = (0.05 * fsf) as usize;
            let (_, ph_in, _) = fit(&x[skip..], f, fsf);
            let (_, ph_out, _) = fit(&y[skip..], f, fsf);
            let delay_s = rt.latency_internal() as f64 / 32_000.0;
            let mut dphi = ph_in - ph_out - 2.0 * PI * f * delay_s;
            dphi = (dphi + PI).rem_euclid(2.0 * PI) - PI;
            assert!(dphi.abs() < 1e-3, "{fs}: phase error {dphi}");
        }
    }

    #[test]
    fn aliasing_from_above_16k_is_rejected() {
        // Tones between 16 kHz and the host Nyquist must not fold into the 32 kHz domain.
        for fs in [48_000u32, 96_000, 192_000] {
            let fsf = fs as f64;
            for f in [16_000.0, 17_000.0, 20_000.0, 23_000.0] {
                let x = sine(f, fsf, fs as usize / 2, 1.0);
                let ratio = Ratio::new(fs as u64, 32_000);
                let mut d = Resampler::new(ratio, fsf, 32_000.0, x.len(), 0);
                d.push(&x);
                let mut y = vec![0f32; 20_000];
                let m = d.pull(&mut y);
                let y = &y[400..m];
                let rms = (y.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / y.len() as f64).sqrt();
                let db = 20.0 * (rms * 2f64.sqrt()).max(1e-12).log10();
                assert!(db < -80.0, "{fs} Hz, tone {f}: leak {db:.1} dB");
            }
        }
    }

    #[test]
    fn low_host_rate_round_trip() {
        let fs = 22_050;
        let x = sine(1000.0, fs as f64, fs as usize, 0.5);
        let (y, _) = round_trip(fs, &x);
        let (amp, _, res) = fit(&y[2000..], 1000.0, fs as f64);
        assert!((amp - 0.5).abs() < 1e-3);
        assert!(20.0 * (res / amp).log10() < -80.0);
    }
}
