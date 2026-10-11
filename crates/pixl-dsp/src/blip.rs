//! Zero-latency band-limited step synthesis (minimum-phase BLEP), after blargg's Blip_Buffer.
//!
//! Every chip channel produces a piecewise-constant signal. Instead of sampling it (which
//! aliases badly), each amplitude change of size `delta` at fractional time `t` adds
//! `delta × kernel(t)` into an *impulse buffer*; the output is the running integral of that
//! buffer. The kernel row for a given sub-sample phase is the first difference of a
//! band-limited step response sampled at the output rate, so the integral reproduces the
//! band-limited step exactly and settles to the full `delta` (each row sums to 1.0).
//!
//! The band-limited impulse is designed once per process: a Kaiser-windowed sinc low-pass
//! (cutoff 0.45·fs) is converted to **minimum phase** with the cepstral method, so steps start
//! immediately (no look-ahead latency) while keeping the stop-band of the linear-phase
//! prototype. [`TAPS`] output samples × [`PHASES`] sub-sample phases are tabulated; the phase
//! is linearly interpolated between adjacent rows.
//!
//! **Drift-free integration.** The integrator is an `f64` accumulator. Instead of a leak (which
//! would high-pass the signal and turn every unipolar note end into a slow DC tail), the
//! buffer keeps the exact `f64` sum of every delta it was given. In exact arithmetic,
//! *integrated output + not-yet-integrated kernel tail = sum of all deltas*; after every block
//! the accumulator is re-anchored to make that identity hold, so the `f32` kernel and buffer
//! rounding can never accumulate (the per-block correction is ~1e-7 of a step: inaudible).

use crate::analysis::fft;
use std::sync::OnceLock;

/// Kernel length in output samples.
pub const TAPS: usize = 32;
/// Number of tabulated sub-sample phases (rows are interpolated linearly).
pub const PHASES: usize = 256;
/// −6 dB cutoff of the band-limited impulse, as a fraction of the sample rate.
pub const CUTOFF: f64 = 0.45;
/// Kaiser window β of the linear-phase prototype (≈ 105 dB stop-band).
pub const KAISER_BETA: f64 = 10.6;

/// The tabulated minimum-phase band-limited step kernel. Obtain it with [`Kernel::get`].
pub struct Kernel {
    /// `PHASES + 1` rows of `TAPS` step-response differences; row `p` is for a step at
    /// sub-sample offset `p / PHASES`. Row `PHASES` equals row 0 delayed by one sample.
    rows: Box<[[f32; TAPS]]>,
    /// The minimum-phase impulse response at `PHASES`× oversampling (unit DC gain), kept for
    /// analysis and tests.
    impulse: Vec<f64>,
}

static KERNEL: OnceLock<Kernel> = OnceLock::new();

impl Kernel {
    /// The process-wide kernel, designed on first use. The first call performs the design
    /// (a few milliseconds of FFT work) and must happen off the audio thread; the engine calls
    /// it from [`crate::Engine::new`].
    pub fn get() -> &'static Kernel {
        KERNEL.get_or_init(design)
    }

    /// Kernel row for phase index `p` (`0..=PHASES`).
    #[inline]
    pub fn row(&self, p: usize) -> &[f32; TAPS] {
        &self.rows[p.min(PHASES)]
    }

    /// The oversampled minimum-phase impulse response (unit DC gain, `PHASES` samples per
    /// output sample).
    pub fn impulse(&self) -> &[f64] {
        &self.impulse
    }
}

/// Zeroth-order modified Bessel function of the first kind (series expansion).
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..200 {
        term *= q / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Designs the kernel. Allocates; never called on the audio thread.
fn design() -> Kernel {
    use std::f64::consts::PI;
    let m = (TAPS - 1) * PHASES; // prototype length: every phase row fits in TAPS samples
    let fc = CUTOFF / PHASES as f64; // cycles per oversampled sample
    let centre = (m - 1) as f64 / 2.0;
    let i0b = bessel_i0(KAISER_BETA);
    let mut proto = vec![0.0f64; m];
    for (i, h) in proto.iter_mut().enumerate() {
        let x = i as f64 - centre;
        let sinc = if x.abs() < 1e-12 { 2.0 * fc } else { (2.0 * PI * fc * x).sin() / (PI * x) };
        let r = x / (centre + 0.5);
        let w = bessel_i0(KAISER_BETA * (1.0 - r * r).max(0.0).sqrt()) / i0b;
        *h = sinc * w;
    }

    // Minimum-phase conversion via the real cepstrum (homomorphic folding).
    let nfft = 1usize << 17;
    let mut re = vec![0.0f64; nfft];
    let mut im = vec![0.0f64; nfft];
    re[..m].copy_from_slice(&proto);
    fft(&mut re, &mut im, false);
    let peak = re.iter().zip(im.iter()).fold(0.0f64, |a, (r, i)| a.max((r * r + i * i).sqrt()));
    let floor = peak * 1e-11;
    for k in 0..nfft {
        let mag = (re[k] * re[k] + im[k] * im[k]).sqrt().max(floor);
        re[k] = mag.ln();
        im[k] = 0.0;
    }
    fft(&mut re, &mut im, true); // real cepstrum in `re`
    for v in re.iter_mut().take(nfft / 2).skip(1) {
        *v *= 2.0;
    }
    for v in re.iter_mut().skip(nfft / 2 + 1) {
        *v = 0.0;
    }
    im.iter_mut().for_each(|v| *v = 0.0);
    fft(&mut re, &mut im, false);
    for k in 0..nfft {
        let a = re[k].exp();
        let (s, c) = im[k].sin_cos();
        re[k] = a * c;
        im[k] = a * s;
    }
    fft(&mut re, &mut im, true);
    let mut impulse: Vec<f64> = re[..m].to_vec();
    let sum: f64 = impulse.iter().sum();
    if sum.abs() > 1e-12 {
        impulse.iter_mut().for_each(|v| *v /= sum);
    }

    // Step response at oversampled resolution: s[k] = Σ_{i<k} h[i], s[0] = 0, s[m] = 1.
    let mut step = vec![0.0f64; m + 1];
    for i in 0..m {
        step[i + 1] = step[i] + impulse[i];
    }
    let total = step[m];
    step.iter_mut().for_each(|v| *v /= total);
    let s_at = |k: isize| -> f64 {
        if k <= 0 {
            0.0
        } else if k as usize >= m {
            1.0
        } else {
            step[k as usize]
        }
    };

    let mut rows = vec![[0.0f32; TAPS]; PHASES + 1].into_boxed_slice();
    for (p, row) in rows.iter_mut().enumerate() {
        let mut exact = [0.0f64; TAPS];
        for (j, e) in exact.iter_mut().enumerate() {
            let a = ((j + 1) * PHASES) as isize - p as isize;
            let b = (j * PHASES) as isize - p as isize;
            *e = s_at(a) - s_at(b);
        }
        // Exact sum is 1 by construction; make the f32 row sum to 1 as closely as possible
        // by folding the rounding residual into the largest tap.
        let s: f64 = exact.iter().sum();
        let mut biggest = 0;
        for j in 0..TAPS {
            row[j] = (exact[j] / s) as f32;
            if row[j].abs() > row[biggest].abs() {
                biggest = j;
            }
        }
        let fsum: f64 = row.iter().map(|v| *v as f64).sum();
        row[biggest] = (row[biggest] as f64 + (1.0 - fsum)) as f32;
    }
    Kernel { rows, impulse }
}

/// Stereo band-limited step accumulator.
///
/// Times are in samples relative to the start of the block that the next [`BlipBuffer::read`]
/// will output; they must lie in `0..capacity` (out-of-range times are clamped, never panic).
pub struct BlipBuffer {
    kernel: &'static Kernel,
    l: Vec<f32>,
    r: Vec<f32>,
    acc_l: f64,
    acc_r: f64,
    /// Exact sum of all deltas ever added (the value the output settles to).
    sum_l: f64,
    sum_r: f64,
    capacity: usize,
}

impl BlipBuffer {
    /// Allocates a buffer for blocks of up to `capacity` samples.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            kernel: Kernel::get(),
            l: vec![0.0; capacity + TAPS + 2],
            r: vec![0.0; capacity + TAPS + 2],
            acc_l: 0.0,
            acc_r: 0.0,
            sum_l: 0.0,
            sum_r: 0.0,
            capacity,
        }
    }

    /// Maximum block length.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Clears all pending impulses and the integrators.
    pub fn clear(&mut self) {
        self.l.fill(0.0);
        self.r.fill(0.0);
        self.acc_l = 0.0;
        self.acc_r = 0.0;
        self.sum_l = 0.0;
        self.sum_r = 0.0;
    }

    /// Adds a band-limited step of `dl`/`dr` at time `t` (samples, may be fractional).
    #[inline]
    pub fn add_step(&mut self, t: f64, dl: f32, dr: f32) {
        if !(dl.is_finite() && dr.is_finite()) {
            return;
        }
        let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
        let whole = t.floor();
        let mut idx = whole as usize;
        let mut frac = t - whole;
        if idx >= self.capacity {
            idx = self.capacity - 1;
            frac = 0.0;
        }
        let pos = frac * PHASES as f64;
        let p = (pos as usize).min(PHASES - 1);
        let f = (pos - p as f64) as f32;
        let r0 = self.kernel.row(p);
        let r1 = self.kernel.row(p + 1);
        let start = idx + 1;
        let (Some(dst_l), Some(dst_r)) = (self.l.get_mut(start..start + TAPS), self.r.get_mut(start..start + TAPS))
        else {
            return;
        };
        let (Ok(dst_l), Ok(dst_r)) = (<&mut [f32; TAPS]>::try_from(dst_l), <&mut [f32; TAPS]>::try_from(dst_r)) else {
            return;
        };
        for j in 0..TAPS {
            let k = r0[j] + f * (r1[j] - r0[j]);
            dst_l[j] += dl * k;
            dst_r[j] += dr * k;
        }
        self.sum_l += dl as f64;
        self.sum_r += dr as f64;
    }

    /// Adds an unfiltered step at integer sample `n` (used by integrate-and-dump channels and
    /// smooth DC discharges, whose output is already a discrete-time signal). `n` may equal the
    /// block length (the step then lands on the first sample of the next block).
    #[inline]
    pub fn add_raw(&mut self, n: usize, dl: f32, dr: f32) {
        if !(dl.is_finite() && dr.is_finite()) {
            return;
        }
        let n = n.min(self.capacity);
        if let (Some(a), Some(b)) = (self.l.get_mut(n), self.r.get_mut(n)) {
            *a += dl;
            *b += dr;
            self.sum_l += dl as f64;
            self.sum_r += dr as f64;
        }
    }

    /// Integrates `min(out_l.len(), out_r.len(), capacity)` samples into the outputs
    /// (overwriting them), carries the kernel tails into the next block and re-anchors the
    /// integrator (see the module docs).
    pub fn read(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let len = out_l.len().min(out_r.len()).min(self.capacity);
        let (mut al, mut ar) = (self.acc_l, self.acc_r);
        for i in 0..len {
            al += self.l[i] as f64;
            ar += self.r[i] as f64;
            out_l[i] = al as f32;
            out_r[i] = ar as f32;
        }
        self.l.copy_within(len..len + TAPS + 1, 0);
        self.l[TAPS + 1..len + TAPS + 1].fill(0.0);
        self.r.copy_within(len..len + TAPS + 1, 0);
        self.r[TAPS + 1..len + TAPS + 1].fill(0.0);
        // Re-anchor: accumulator + pending tail must equal the exact sum of all deltas.
        let tail_l: f64 = self.l[..TAPS + 1].iter().map(|v| *v as f64).sum();
        let tail_r: f64 = self.r[..TAPS + 1].iter().map(|v| *v as f64).sum();
        al = self.sum_l - tail_l;
        ar = self.sum_r - tail_r;
        if !(al.is_finite() && ar.is_finite() && self.sum_l.is_finite() && self.sum_r.is_finite()) {
            self.clear();
            return;
        }
        self.acc_l = al;
        self.acc_r = ar;
    }

    /// True when the output has settled (no pending kernel tail) at a level below
    /// `threshold`.
    pub fn is_quiet(&self, threshold: f32) -> bool {
        let t = threshold as f64;
        self.acc_l.abs() < t
            && self.acc_r.abs() < t
            && self.l[..TAPS + 1].iter().all(|v| v.abs() < threshold * 1e-3)
            && self.r[..TAPS + 1].iter().all(|v| v.abs() < threshold * 1e-3)
    }

    /// Forces the output to exactly zero (used once the engine is silent so that silence is
    /// bit-exact). Only call it when nothing is supposed to be sounding.
    pub fn flush(&mut self) {
        self.clear();
    }

    /// Current integrator values (left, right).
    pub fn level(&self) -> (f64, f64) {
        (self.acc_l, self.acc_r)
    }

    /// The level the output settles to: the exact sum of all deltas (left, right).
    pub fn settled_level(&self) -> (f64, f64) {
        (self.sum_l, self.sum_r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::db;

    /// Magnitude response (dB) of the oversampled impulse at `f` (fraction of the output rate).
    fn response_db(h: &[f64], f: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * f / PHASES as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (k, v) in h.iter().enumerate() {
            re += v * (w * k as f64).cos();
            im -= v * (w * k as f64).sin();
        }
        db((re * re + im * im).sqrt())
    }

    #[test]
    fn rows_sum_to_one_and_rows_are_continuous() {
        let k = Kernel::get();
        for p in 0..=PHASES {
            let s: f64 = k.row(p).iter().map(|v| *v as f64).sum();
            assert!((s - 1.0).abs() < 1e-6, "row {p} sums to {s}");
        }
        // Row PHASES is row 0 delayed by one sample.
        let r0 = k.row(0);
        let rp = k.row(PHASES);
        assert!(rp[0].abs() < 1e-7);
        for j in 1..TAPS {
            assert!((rp[j] - r0[j - 1]).abs() < 1e-6);
        }
        // Adjacent rows differ smoothly (linear interpolation is accurate).
        let mut max_step = 0.0f32;
        for p in 0..PHASES {
            for j in 0..TAPS {
                max_step = max_step.max((k.row(p)[j] - k.row(p + 1)[j]).abs());
            }
        }
        assert!(max_step < 0.02, "{max_step}");
    }

    #[test]
    fn kernel_is_minimum_phase_and_band_limited() {
        let k = Kernel::get();
        let h = k.impulse();
        // Minimum phase: the response peaks within a few samples (a linear-phase design of the
        // same length peaks at its centre, 15.5 samples) and the energy is front-loaded.
        let peak = h
            .iter()
            .enumerate()
            .fold((0, 0.0f64), |(bi, bv), (i, v)| if v.abs() > bv { (i, v.abs()) } else { (bi, bv) })
            .0;
        assert!(peak < 4 * PHASES, "peak at {} samples", peak as f64 / PHASES as f64);
        let total: f64 = h.iter().map(|v| v * v).sum();
        let first: f64 = h[..4 * PHASES].iter().map(|v| v * v).sum();
        let first12: f64 = h[..12 * PHASES].iter().map(|v| v * v).sum();
        assert!(first / total > 0.6, "{}", first / total);
        assert!(first12 / total > 0.99, "{}", first12 / total);
        // Group delay at DC (centre of mass) is a few samples at most.
        let s: f64 = h.iter().sum();
        let gd: f64 = h.iter().enumerate().map(|(i, v)| i as f64 * v).sum::<f64>() / s / PHASES as f64;
        assert!(gd < 4.0, "{gd}");
        // Pass band, cutoff and stop band.
        assert!(response_db(h, 0.0).abs() < 1e-6);
        for f in [0.05, 0.1, 0.2, 0.3] {
            assert!(response_db(h, f).abs() < 0.1, "pass band ripple at {f}: {}", response_db(h, f));
        }
        let c = response_db(h, CUTOFF);
        assert!((-7.5..=-4.5).contains(&c), "cutoff {c}");
        for i in 0..200 {
            let f = 0.57 + i as f64 * 0.02;
            let r = response_db(h, f);
            assert!(r < -95.0, "stop band at {f}: {r}");
        }
    }

    #[test]
    fn step_settles_exactly() {
        let mut b = BlipBuffer::new(64);
        b.add_step(3.37, 1.0, -0.5);
        let mut l = [0.0f32; 64];
        let mut r = [0.0f32; 64];
        b.read(&mut l, &mut r);
        assert_eq!(l[3], 0.0, "causal: nothing before the step time");
        assert!((l[63] - 1.0).abs() < 1e-6 && (r[63] + 0.5).abs() < 1e-6);
        b.read(&mut l, &mut r);
        assert!(l.iter().all(|v| (v - 1.0).abs() < 1e-6));
    }

    #[test]
    fn print_kernel_response() {
        let h = Kernel::get().impulse();
        let mut line = String::new();
        for f in [0.3, 0.35, 0.4, 0.42, 0.45, 0.48, 0.5, 0.52, 0.55, 0.6, 0.7] {
            line.push_str(&format!("{f:.2}:{:.1} ", response_db(h, f)));
        }
        println!("kernel response dB {line}");
    }

    #[test]
    fn integration_is_drift_free() {
        // Millions of random steps at random phases that sum to zero must return the output to
        // (essentially) exactly zero: the f32 kernel rounding cannot accumulate.
        let mut b = BlipBuffer::new(64);
        let mut l = [0.0f32; 64];
        let mut r = [0.0f32; 64];
        let mut seed = 0x1234_5678u64;
        let mut level = 0.0f32;
        for _ in 0..20_000 {
            for _ in 0..100 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let t = (seed >> 11) as f64 / (1u64 << 53) as f64 * 63.0;
                let target = if (seed >> 7) & 1 == 1 { 0.7 } else { -0.3 };
                b.add_step(t, target - level, (target - level) * 0.5);
                level = target;
            }
            b.read(&mut l, &mut r);
        }
        b.add_step(0.0, -level, -level * 0.5);
        for _ in 0..4 {
            b.read(&mut l, &mut r);
        }
        assert!(l[63].abs() < 1e-9 && r[63].abs() < 1e-9, "{} {}", l[63], r[63]);
        assert!(b.is_quiet(1e-6));
    }

    #[test]
    fn raw_steps_and_block_carry() {
        let mut b = BlipBuffer::new(16);
        b.add_raw(16, 0.25, 0.5); // lands on the first sample of the next block
        b.add_step(15.5, 1.0, 1.0);
        let mut l = [0.0f32; 8];
        let mut r = [0.0f32; 8];
        let mut big_l = [0.0f32; 16];
        let mut big_r = [0.0f32; 16];
        b.read(&mut big_l, &mut big_r);
        assert!(big_l.iter().all(|v| *v == 0.0));
        b.read(&mut l, &mut r);
        assert!(l[0] > 0.25 && l[0] < 1.25, "{}", l[0]);
        b.read(&mut l, &mut r);
        b.read(&mut l, &mut r);
        b.read(&mut l, &mut r);
        assert!((l[7] - 1.25).abs() < 1e-6 && (r[7] - 1.5).abs() < 1e-6);
        assert!(!b.is_quiet(1e-6));
        b.flush();
        assert!(b.is_quiet(1e-9));
    }
}
