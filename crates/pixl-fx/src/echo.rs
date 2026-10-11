//! SNES SPC700-style echo (SPEC §5.4).
//!
//! The echo runs at the SPC700's 32 kHz: the host signal is resampled down
//! ([`crate::resampler`]), processed, and resampled back. Signal flow per channel (32 kHz):
//!
//! ```text
//!   in ──►(input routing, on/off ramp)──►(+)──► soft clip ──► 16-bit ──► delay line (≤ 2 s)
//!                                         ▲                                   │
//!                                         │ fb · (width matrix)               ▼ read at T
//!                                         └───────────── FIR (8 taps, Q7) ◄───┤
//!                                                                             │ read at T − Z
//!   wet = FIR(read at T − Z) · mix ◄──────────────────────────────────────────┘
//! ```
//!
//! - **Exact timing.** The resampler round trip has a fixed, exact latency of `Z` internal
//!   samples. The loop itself uses the full delay `T`, so repeats are spaced exactly `T`
//!   apart, while the wet output is read through a second FIR from `T − Z` (the same content
//!   `Z` samples earlier). After the up-sampler adds `Z` back, the first repeat lands exactly
//!   `T` after the dry signal, and every further repeat at `k·T`.
//! - **FIR presets** are Q7 (`/128`) like the hardware; see [`FIR_PRESETS`] for the designed
//!   responses. Every preset has `|DC gain| ≤ 1` *and* a peak magnitude ≤ 1 at every
//!   frequency, so with feedback ≤ 95 % the loop gain stays ≤ 0.95 everywhere. Preset changes
//!   glide the coefficients (a convex combination of two stable sets is stable).
//! - **Feedback path** goes through a soft clipper (unity slope below 0.6, asymptote 1.0) and
//!   16-bit quantization with truncation toward zero. Truncation (unlike the SPC700's floor,
//!   which drifts towards −1 LSB) only ever removes energy; the tests verify that every
//!   preset and width decays from 60 s of noise at 95 % feedback to exact digital silence
//!   (no limit cycles).
//! - **Width** interpolates the input routing and the feedback matrix from independent L/R
//!   echoes (0) to full ping-pong (1: mono input into the left line, feedback crosses L↔R).
//!   The matrix `[[1−w, w], [w, 1−w]]` has spectral norm 1, so width never adds loop gain.
//! - **Time changes** crossfade between two read heads over 30 ms (smoothstep, equal gain).
//! - **Off** stops feeding the line (10 ms ramp) but keeps the tail running. Once the lines
//!   are silent — and the echo is off, or on but receiving silence — the whole echo
//!   (including resampling) goes to sleep at zero cost and reports no tail; an enabled echo
//!   wakes up with the first non-silent input block. Sleeping is output-identical to
//!   processing the silence.

use crate::resampler::RoundTrip;
use crate::util::{Ramp, Smoother};
use pixl_core::EchoFir;
use std::f64::consts::PI;

/// Internal processing rate of the echo (SPC700 DSP rate).
pub const INTERNAL_RATE: u32 = 32_000;
/// Longest echo time (seconds).
pub const MAX_TIME_S: f64 = 2.0;
/// Shortest echo time (seconds).
pub const MIN_TIME_S: f64 = 0.010;
/// Read-head crossfade duration on time changes (seconds).
pub const TIME_XFADE_S: f64 = 0.030;
/// Smoothing time of feedback, mix and width (seconds to 99 %).
pub const PARAM_SMOOTH_S: f32 = 0.020;
/// Smoothing time of FIR coefficient changes (seconds to 99 %).
pub const FIR_SMOOTH_S: f32 = 0.020;
/// On/off input ramp (seconds).
pub const ONOFF_RAMP_S: f32 = 0.010;
/// Highest feedback amount (fraction).
pub const MAX_FEEDBACK: f32 = 0.95;
/// Soft-clip knee of the feedback path (linear below this level).
pub const CLIP_KNEE: f32 = 0.6;

/// Delay-line length per channel (power of two ≥ 2 s at 32 kHz + FIR span + margin).
const LINE_LEN: usize = 1 << 16;
const LINE_MASK: usize = LINE_LEN - 1;
const _: () = assert!(LINE_LEN > (MAX_TIME_S as usize) * INTERNAL_RATE as usize + 16);

/// 8-tap FIR presets in Q7 (coefficient `k` multiplies the read sample delayed by `k`;
/// response = `Σ c_k e^{−jωk} / 128` at 32 kHz). Indexed by [`EchoFir::index`].
///
/// Designed for PIXL (weighted magnitude fits with integer coefficient search); magnitude in
/// dB at 32 kHz (repeat `n` of the echo sees this response `n` times):
///
/// | preset | coefficients | 0 Hz | 500 | 1 k | 2 k | 3 k | 4 k | 6 k | 8 k | 12 k | 16 k | character |
/// |---|---|---|---|---|---|---|---|---|---|---|---|---|
/// | FLAT   | `127 0 0 0 0 0 0 0`           | −0.07 | −0.07 | −0.07 | −0.07 | −0.07 | −0.07 | −0.07 | −0.07 | −0.07 | −0.07 | clean repeats |
/// | WARM   | `8 23 37 49 26 −18 −8 10`     | −0.07 | −0.12 | −0.27 | −0.78 | −1.34 | −1.89 | −3.89 | −7.46 | −15.06 | −42.14 | gentle low-pass, −3 dB ≈ 5.4 kHz |
/// | DARK   | `3 12 22 27 27 22 12 3`       | 0.00 | −0.11 | −0.44 | −1.80 | −4.18 | −7.82 | −22.64 | −27.09 | −61.41 | −∞ | strong low-pass, −3 dB ≈ 2.5 kHz, linear phase |
/// | CAVE   | `24 44 30 13 3 −16 −10 −12`   | −4.53 | −3.98 | −2.79 | −0.70 | −0.15 | −1.31 | −9.48 | −13.23 | −15.93 | −17.04 | resonant band, peak −0.12 dB at 2.8 kHz |
/// | BRIGHT | `91 −55 −31 −17 −8 −4 −2 −1`  | −13.52 | −11.74 | −8.85 | −4.78 | −2.66 | −1.60 | −0.66 | −0.34 | −0.20 | −0.07 | high-pass shelf, thin repeats |
/// | LOFI   | `11 24 26 22 2 −22 −25 −13`   | −14.19 | −10.64 | −6.61 | −2.29 | −0.86 | −1.35 | −7.77 | −19.29 | −26.03 | −32.60 | telephone band-pass, peak −0.82 dB at 3.2 kHz |
///
/// The table is verified by the tests (`fir_responses_match_documentation`).
pub const FIR_PRESETS: [[i8; 8]; 6] = [
    [127, 0, 0, 0, 0, 0, 0, 0],
    [8, 23, 37, 49, 26, -18, -8, 10],
    [3, 12, 22, 27, 27, 22, 12, 3],
    [24, 44, 30, 13, 3, -16, -10, -12],
    [91, -55, -31, -17, -8, -4, -2, -1],
    [11, 24, 26, 22, 2, -22, -25, -13],
];

/// Q7 coefficients of a preset.
pub fn fir_coeffs(fir: EchoFir) -> [i8; 8] {
    FIR_PRESETS[fir.index().min(FIR_PRESETS.len() - 1)]
}

/// Magnitude (linear) of a preset at `f` Hz (32 kHz domain).
pub fn fir_magnitude(fir: EchoFir, f: f64) -> f64 {
    let c = fir_coeffs(fir);
    let w = 2.0 * PI * f / INTERNAL_RATE as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (k, &ck) in c.iter().enumerate() {
        re += ck as f64 * (w * k as f64).cos();
        im -= ck as f64 * (w * k as f64).sin();
    }
    (re * re + im * im).sqrt() / 128.0
}

/// DC gain of a preset (signed).
pub fn fir_dc_gain(fir: EchoFir) -> f64 {
    fir_coeffs(fir).iter().map(|&c| c as f64).sum::<f64>() / 128.0
}

/// Peak magnitude of a preset over 0..16 kHz (1 Hz grid).
pub fn fir_peak_gain(fir: EchoFir) -> f64 {
    (0..=16_000).map(|f| fir_magnitude(fir, f as f64)).fold(0.0, f64::max)
}

/// Peak gains of all presets, computed once per process (first call happens in
/// [`EchoCore::new`], off the audio thread).
fn fir_peak_table() -> &'static [f32; 6] {
    static TABLE: std::sync::OnceLock<[f32; 6]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| fir_peak_gain(EchoFir::from_index(i)) as f32))
}

/// Soft clipper of the feedback path: identity below [`CLIP_KNEE`], tanh knee above,
/// asymptote ±1. Slope ≤ 1 and `|y| ≤ |x|` everywhere (never adds loop gain).
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= CLIP_KNEE {
        x
    } else {
        let room = 1.0 - CLIP_KNEE;
        let y = CLIP_KNEE + room * ((a - CLIP_KNEE) / room).tanh();
        y.copysign(x)
    }
}

/// 16-bit quantization with truncation toward zero (as stored in the delay line).
#[inline]
pub fn quantize16(x: f32) -> i16 {
    // `as` saturates and maps NaN to 0.
    ((x * 32_768.0).trunc() as i32).clamp(-32_768, 32_767) as i16
}

const I16_SCALE: f32 = 1.0 / 32_768.0;

/// 8-sample FIR history with a mirrored buffer so the newest-first window is contiguous.
#[derive(Clone, Copy, Debug, Default)]
struct FirHist {
    buf: [f32; 16],
    idx: usize,
}

impl FirHist {
    #[inline]
    fn push(&mut self, x: f32) {
        self.idx = (self.idx + 7) & 7;
        self.buf[self.idx] = x;
        self.buf[self.idx + 8] = x;
    }

    #[inline]
    fn apply(&self, c: &[f32; 8]) -> f32 {
        let w = &self.buf[self.idx..self.idx + 8];
        (c[0] * w[0] + c[1] * w[1])
            + (c[2] * w[2] + c[3] * w[3])
            + (c[4] * w[4] + c[5] * w[5])
            + (c[6] * w[6] + c[7] * w[7])
    }
}

/// The echo running at [`INTERNAL_RATE`] (no resampling).
#[derive(Clone, Debug)]
pub struct EchoCore {
    lines: [Vec<i16>; 2],
    write: usize,
    /// Current loop delay (samples) and crossfade target.
    delay_a: usize,
    delay_b: usize,
    target_delay: usize,
    xfade: f32,
    xfade_step: f32,
    xfading: bool,
    /// Output-tap lookahead (round-trip latency compensation).
    lookahead: usize,
    fir_fb: [FirHist; 2],
    fir_out: [FirHist; 2],
    coeff: [f32; 8],
    coeff_target: [f32; 8],
    coeff_alpha: f32,
    coeff_moving: bool,
    fir_peak: f32,
    feedback: Smoother,
    mix: Smoother,
    width: Smoother,
    input: Ramp,
    /// Consecutive samples in which both lines were written with zeros.
    zero_run: usize,
    /// Peak |write| of the current and previous delay-length window.
    win_peak: [f32; 2],
    win_count: usize,
}

impl EchoCore {
    /// Creates an empty echo (allocates the 2 s lines). Not real-time safe.
    pub fn new(lookahead: usize) -> Self {
        // Build the shared preset table now (never on the audio thread).
        let _ = fir_peak_table();
        let fs = INTERNAL_RATE as f32;
        let d = (0.25 * fs) as usize;
        let mut core = Self {
            lines: [vec![0; LINE_LEN], vec![0; LINE_LEN]],
            write: 0,
            delay_a: d,
            delay_b: d,
            target_delay: d,
            xfade: 0.0,
            xfade_step: 1.0 / (TIME_XFADE_S * INTERNAL_RATE as f64) as f32,
            xfading: false,
            lookahead,
            fir_fb: [FirHist::default(); 2],
            fir_out: [FirHist::default(); 2],
            coeff: [0.0; 8],
            coeff_target: [0.0; 8],
            coeff_alpha: 1.0 - (-(100f32.ln()) / (FIR_SMOOTH_S * fs)).exp(),
            coeff_moving: false,
            fir_peak: 1.0,
            feedback: Smoother::new(0.4, PARAM_SMOOTH_S, fs),
            mix: Smoother::new(0.25, PARAM_SMOOTH_S, fs),
            width: Smoother::new(0.5, PARAM_SMOOTH_S, fs),
            input: Ramp::new(0.0, ONOFF_RAMP_S, fs),
            zero_run: 0,
            win_peak: [0.0; 2],
            win_count: 0,
        };
        core.set_fir(EchoFir::Warm);
        core.coeff = core.coeff_target;
        core.coeff_moving = false;
        core
    }

    /// Sets the round-trip latency compensation (internal samples).
    pub fn set_lookahead(&mut self, lookahead: usize) {
        self.lookahead = lookahead;
    }

    /// Clears the lines and filter histories, jumps smoothers to their targets and the delay
    /// to its target (no crossfade). The input ramp keeps its target.
    pub fn reset(&mut self) {
        for l in &mut self.lines {
            l.iter_mut().for_each(|x| *x = 0);
        }
        self.write = 0;
        self.restart();
    }

    /// Like [`reset`](Self::reset) but leaves the delay lines untouched: for waking up when
    /// the lines are already known to be all zero (cheap, no 256 kB clear at note-on).
    pub fn restart(&mut self) {
        self.delay_a = self.target_delay;
        self.delay_b = self.target_delay;
        self.xfading = false;
        self.xfade = 0.0;
        self.fir_fb = [FirHist::default(); 2];
        self.fir_out = [FirHist::default(); 2];
        self.coeff = self.coeff_target;
        self.coeff_moving = false;
        self.feedback.snap();
        self.mix.snap();
        self.width.snap();
        self.zero_run = 0;
        self.win_peak = [0.0; 2];
        self.win_count = 0;
    }

    /// Sets the loop delay in internal samples (clamped to the valid range).
    pub fn set_delay(&mut self, samples: usize) {
        let min = ((MIN_TIME_S * INTERNAL_RATE as f64) as usize).max(self.lookahead + 1);
        let max = (MAX_TIME_S * INTERNAL_RATE as f64) as usize;
        self.target_delay = samples.clamp(min, max);
    }

    /// Current target loop delay in internal samples.
    pub fn target_delay(&self) -> usize {
        self.target_delay
    }

    /// Selects a FIR preset (coefficients glide over [`FIR_SMOOTH_S`]).
    pub fn set_fir(&mut self, fir: EchoFir) {
        let c = fir_coeffs(fir);
        let mut t = [0f32; 8];
        for (d, &s) in t.iter_mut().zip(&c) {
            *d = s as f32 / 128.0;
        }
        if t != self.coeff_target {
            self.coeff_target = t;
            self.coeff_moving = true;
            self.fir_peak = fir_peak_table()[fir.index().min(FIR_PRESETS.len() - 1)];
        }
    }

    /// Sets feedback (0..0.95), mix (0..1) and width (0..1) targets.
    pub fn set_levels(&mut self, feedback: f32, mix: f32, width: f32) {
        self.feedback.set_target(feedback.clamp(0.0, MAX_FEEDBACK));
        self.mix.set_target(mix.clamp(0.0, 1.0));
        self.width.set_target(width.clamp(0.0, 1.0));
    }

    /// Opens (`true`) or closes the echo input (10 ms ramp). Closed input lets the tail decay.
    pub fn set_input_on(&mut self, on: bool) {
        self.input.set_on(on);
    }

    /// Jumps the input ramp to its target.
    pub fn snap_input(&mut self) {
        self.input.snap();
    }

    /// Whether the lines have been written with zeros for long enough that the output
    /// (including `extra` samples of downstream filter history) is exact silence and stays
    /// silent while the input is silent.
    pub fn is_silent(&self, extra: usize) -> bool {
        let reach = self.delay_a.max(self.delay_b).max(self.target_delay) + 8 + extra;
        self.zero_run > reach
    }

    /// Whether the input ramp is fully closed (echo switched off).
    pub fn input_closed(&self) -> bool {
        self.input.is_off()
    }

    /// Estimated samples (internal rate) until the tail falls below one 16-bit LSB.
    pub fn tail_estimate(&self) -> u64 {
        let level = self.win_peak[0].max(self.win_peak[1]);
        if level <= I16_SCALE {
            // Nothing (or at most one LSB) in flight: at most one more pass.
            return if level > 0.0 { (self.delay_a + 8 + self.lookahead) as u64 } else { 0 };
        }
        let g = (self.feedback.target() * self.fir_peak.min(1.0)) as f64;
        let repeats =
            if g <= 1e-3 { 1.0 } else { ((I16_SCALE as f64 / level as f64).ln() / g.ln()).ceil().max(1.0) + 1.0 };
        let repeats = repeats.min(4096.0);
        (repeats * self.delay_a.max(self.delay_b) as f64) as u64 + 8 + self.lookahead as u64
    }

    #[inline]
    fn read(line: &[i16], write: usize, dist: usize) -> f32 {
        // `line.len() == LINE_LEN`; the mask keeps the index in range.
        line[(write.wrapping_sub(dist)) & LINE_MASK] as f32 * I16_SCALE
    }

    /// Processes internal-rate samples in place: input = dry signal, output = `wet · mix`.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        for i in 0..n {
            // Start a new head crossfade when the target moved and none is running.
            if !self.xfading && self.target_delay != self.delay_a {
                self.delay_b = self.target_delay;
                self.xfade = 0.0;
                self.xfading = true;
            }
            let a = if self.xfading {
                let t = self.xfade;
                t * t * (3.0 - 2.0 * t)
            } else {
                0.0
            };
            if self.coeff_moving {
                let mut done = true;
                for (c, t) in self.coeff.iter_mut().zip(&self.coeff_target) {
                    let d = t - *c;
                    if d.abs() < 1e-7 {
                        *c = *t;
                    } else {
                        *c += d * self.coeff_alpha;
                        done = false;
                    }
                }
                self.coeff_moving = !done;
            }
            let la = self.lookahead;
            let mut y_fb = [0f32; 2];
            let mut y_out = [0f32; 2];
            for c in 0..2 {
                let line = &self.lines[c];
                let (fb_in, out_in) = if self.xfading {
                    let fa = Self::read(line, self.write, self.delay_a);
                    let fb = Self::read(line, self.write, self.delay_b);
                    let oa = Self::read(line, self.write, self.delay_a.saturating_sub(la));
                    let ob = Self::read(line, self.write, self.delay_b.saturating_sub(la));
                    (fa + a * (fb - fa), oa + a * (ob - oa))
                } else {
                    (
                        Self::read(line, self.write, self.delay_a),
                        Self::read(line, self.write, self.delay_a.saturating_sub(la)),
                    )
                };
                self.fir_fb[c].push(fb_in);
                self.fir_out[c].push(out_in);
                y_fb[c] = self.fir_fb[c].apply(&self.coeff);
                y_out[c] = self.fir_out[c].apply(&self.coeff);
            }
            let w = self.width.next_value();
            let fb = self.feedback.next_value();
            let mix = self.mix.next_value();
            let g_in = self.input.next_value();
            let (xl, xr) = (l[i], r[i]);
            let in_l = g_in * ((1.0 - w) * xl + w * 0.5 * (xl + xr));
            let in_r = g_in * (1.0 - w) * xr;
            let fb_l = (1.0 - w) * y_fb[0] + w * y_fb[1];
            let fb_r = (1.0 - w) * y_fb[1] + w * y_fb[0];
            let wl = quantize16(soft_clip(in_l + fb * fb_l));
            let wr = quantize16(soft_clip(in_r + fb * fb_r));
            self.lines[0][self.write & LINE_MASK] = wl;
            self.lines[1][self.write & LINE_MASK] = wr;
            self.write = (self.write + 1) & LINE_MASK;
            if wl == 0 && wr == 0 {
                self.zero_run = self.zero_run.saturating_add(1);
            } else {
                self.zero_run = 0;
            }
            let peak = (wl as f32).abs().max((wr as f32).abs()) * I16_SCALE;
            self.win_peak[0] = self.win_peak[0].max(peak);
            self.win_count += 1;
            if self.win_count >= self.delay_a {
                self.win_peak[1] = self.win_peak[0];
                self.win_peak[0] = 0.0;
                self.win_count = 0;
            }
            if self.xfading {
                self.xfade += self.xfade_step;
                if self.xfade >= 1.0 {
                    self.delay_a = self.delay_b;
                    self.xfading = false;
                    self.xfade = 0.0;
                }
            }
            l[i] = y_out[0] * mix;
            r[i] = y_out[1] * mix;
        }
    }
}

/// Input level below which a block counts as silence for the echo's idle detection
/// (−102 dBFS). Even after the down-sampler's worst-case gain such input stays below one
/// 16-bit LSB, which the line's truncation maps to exact zero.
pub const SILENCE_THRESHOLD: f32 = 1.0 / 131_072.0;

/// Whether every sample of `x` is below [`SILENCE_THRESHOLD`] in magnitude.
#[inline]
fn is_quiet(x: &[f32]) -> bool {
    x.iter().fold(0f32, |m, v| m.max(v.abs())) < SILENCE_THRESHOLD
}

/// Echo time from parameters: synced (`div` beats at `bpm`) or free (`time_ms`), clamped
/// to 10 ms ..= 2 s. Returns seconds.
pub fn echo_time_s(sync: bool, div: pixl_core::EchoDiv, bpm: f64, time_ms: f32) -> f64 {
    let t = if sync {
        let bpm = if bpm.is_finite() && bpm > 0.0 { bpm } else { 120.0 };
        div.beats() * 60.0 / bpm
    } else if time_ms.is_finite() {
        time_ms as f64 / 1000.0
    } else {
        0.25
    };
    t.clamp(MIN_TIME_S, MAX_TIME_S)
}

/// Host-rate wrapper: resampling, on/off state machine and idle bypass.
#[derive(Clone, Debug)]
pub struct Echo {
    host_rate: u32,
    resample: bool,
    rt: [RoundTrip; 2],
    core: EchoCore,
    /// Internal-rate scratch per channel.
    mid: [Vec<f32>; 2],
    /// Host-rate wet scratch per channel.
    wet: [Vec<f32>; 2],
    max_block: usize,
    on: bool,
    active: bool,
    /// Whether the previous input block was silent (wake-up fade decision).
    last_quiet: bool,
    time_s: f64,
}

impl Echo {
    /// Creates the echo for a host rate and maximum block size. Allocates.
    pub fn new(host_rate: f32, max_block: usize) -> Self {
        let host = host_rate.round().clamp(1000.0, 1_000_000.0) as u32;
        let max_block = max_block.max(1);
        let rt = RoundTrip::new(host, INTERNAL_RATE, max_block);
        let resample = host != INTERNAL_RATE;
        let lookahead = if resample { rt.latency_internal() } else { 0 };
        let max_mid = (max_block as f64 * INTERNAL_RATE as f64 / host as f64).ceil() as usize + 8;
        let mut e = Self {
            host_rate: host,
            resample,
            rt: [rt.clone(), rt],
            core: EchoCore::new(lookahead),
            mid: [vec![0.0; max_mid], vec![0.0; max_mid]],
            wet: [vec![0.0; max_block], vec![0.0; max_block]],
            max_block,
            on: false,
            active: false,
            last_quiet: true,
            time_s: 0.25,
        };
        e.core.set_delay(e.delay_samples());
        e.reset();
        e
    }

    /// Host rate this echo was built for.
    pub fn host_rate(&self) -> u32 {
        self.host_rate
    }

    /// Round-trip latency compensation in internal samples (0 at 32 kHz hosts).
    pub fn latency_internal(&self) -> usize {
        if self.resample {
            self.rt[0].latency_internal()
        } else {
            0
        }
    }

    fn delay_samples(&self) -> usize {
        (self.time_s * INTERNAL_RATE as f64).round() as usize
    }

    /// Sets all echo parameters. `time_s` is the resolved echo time (see [`echo_time_s`]).
    ///
    /// Switching on does not start processing by itself: the echo wakes up as soon as it
    /// receives a non-silent input block (see [`process`](Self::process)).
    pub fn set_params(&mut self, on: bool, time_s: f64, feedback: f32, mix: f32, fir: EchoFir, width: f32) {
        if time_s.is_finite() {
            self.time_s = time_s.clamp(MIN_TIME_S, MAX_TIME_S);
        }
        self.core.set_delay(self.delay_samples());
        self.core.set_levels(feedback, mix, width);
        self.core.set_fir(fir);
        if on && !self.on && !self.active {
            // Turned on while asleep: fade the input in once processing starts.
            self.core.set_input_on(false);
            self.core.snap_input();
        }
        self.on = on;
        self.core.set_input_on(on);
    }

    /// Clears all state (the echo goes to sleep with empty lines; an enabled echo wakes up
    /// with the next non-silent input).
    pub fn reset(&mut self) {
        self.core.reset();
        self.core.snap_input();
        for rt in &mut self.rt {
            rt.reset();
        }
        self.active = false;
        self.last_quiet = true;
    }

    /// Starts processing from the sleeping state (lines are already all zero).
    fn wake(&mut self) {
        self.core.restart();
        for rt in &mut self.rt {
            rt.reset();
        }
        self.active = true;
    }

    /// Stops processing: clears the lines (they only hold inaudible residue at this point,
    /// but a later time increase must not read it back) and the resamplers.
    fn sleep(&mut self) {
        self.core.reset();
        for rt in &mut self.rt {
            rt.reset();
        }
        self.active = false;
    }

    /// Whether the echo is processing (on with signal, or decaying after being switched off).
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Whether the echo is switched on (it may still be asleep while there is no signal).
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Remaining tail in host samples.
    pub fn tail_samples(&self) -> u64 {
        if !self.active {
            return 0;
        }
        let t = self.core.tail_estimate();
        (t as f64 * self.host_rate as f64 / INTERNAL_RATE as f64).ceil() as u64
    }

    /// Adds the echo to a stereo block in place (`out = dry + wet · mix`).
    ///
    /// Idle handling: when the lines have been silent for longer than the echo reaches and
    /// either the echo is off or the input is silent (below [`SILENCE_THRESHOLD`], which the
    /// 16-bit line truncates to zero anyway), the echo goes to sleep and costs nothing; an
    /// enabled echo wakes up with the first non-silent block. The output is identical to
    /// processing the silence.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        let (l, r) = (&mut l[..n], &mut r[..n]);
        let quiet = is_quiet(l) && is_quiet(r);
        let was_quiet = std::mem::replace(&mut self.last_quiet, quiet);
        if !self.active {
            if !self.on || quiet {
                return;
            }
            self.wake();
            if was_quiet {
                // The signal starts in this block: nothing to fade in against.
                self.core.snap_input();
            }
        }
        let mut start = 0;
        while start < n {
            let len = (n - start).min(self.max_block);
            self.process_chunk(&mut l[start..start + len], &mut r[start..start + len]);
            start += len;
        }
        let extra = if self.resample { self.rt[0].up.taps() + self.rt[0].latency_internal() } else { 0 };
        if self.core.is_silent(extra) && ((!self.on && self.core.input_closed()) || (self.on && quiet)) {
            self.sleep();
        }
    }

    fn process_chunk(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len();
        if !self.resample {
            let [ml, mr] = &mut self.mid;
            ml[..n].copy_from_slice(l);
            mr[..n].copy_from_slice(r);
            self.core.process(&mut ml[..n], &mut mr[..n]);
            for i in 0..n {
                l[i] += self.mid[0][i];
                r[i] += self.mid[1][i];
            }
            return;
        }
        self.rt[0].down.push(l);
        self.rt[1].down.push(r);
        let m0 = self.rt[0].down.pull(&mut self.mid[0]);
        let m1 = self.rt[1].down.pull(&mut self.mid[1]);
        debug_assert_eq!(m0, m1);
        let m = m0.min(m1);
        let [ml, mr] = &mut self.mid;
        self.core.process(&mut ml[..m], &mut mr[..m]);
        for c in 0..2 {
            self.rt[c].up.push(&self.mid[c][..m]);
            self.rt[c].up.pull_exact(&mut self.wet[c][..n]);
        }
        for i in 0..n {
            l[i] += self.wet[0][i];
            r[i] += self.wet[1][i];
        }
    }

    /// Up-sampler starvation count (must stay 0; tests).
    pub fn starved(&self) -> u64 {
        self.rt[0].up.starved() + self.rt[1].up.starved()
    }

    /// The internal-rate core (tests and diagnostics).
    pub fn core(&self) -> &EchoCore {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;
    use pixl_core::EchoDiv;

    fn db(x: f64) -> f64 {
        if x <= 0.0 {
            f64::NEG_INFINITY
        } else {
            20.0 * x.log10()
        }
    }

    const DOC_FREQS: [f64; 10] = [0.0, 500.0, 1000.0, 2000.0, 3000.0, 4000.0, 6000.0, 8000.0, 12_000.0, 16_000.0];
    /// The table in the `FIR_PRESETS` docs (dB); `-inf` written as -999.
    const DOC_TABLE: [[f64; 10]; 6] = [
        [-0.07, -0.07, -0.07, -0.07, -0.07, -0.07, -0.07, -0.07, -0.07, -0.07],
        [-0.07, -0.12, -0.27, -0.78, -1.34, -1.89, -3.89, -7.46, -15.06, -42.14],
        [0.00, -0.11, -0.44, -1.80, -4.18, -7.82, -22.64, -27.09, -61.41, -999.0],
        [-4.53, -3.98, -2.79, -0.70, -0.15, -1.31, -9.48, -13.23, -15.93, -17.04],
        [-13.52, -11.74, -8.85, -4.78, -2.66, -1.60, -0.66, -0.34, -0.20, -0.07],
        [-14.19, -10.64, -6.61, -2.29, -0.86, -1.35, -7.77, -19.29, -26.03, -32.60],
    ];

    #[test]
    fn fir_dc_and_peak_gains_are_at_most_unity() {
        for &fir in EchoFir::ALL {
            let dc = fir_dc_gain(fir);
            let peak = fir_peak_gain(fir);
            println!(
                "{:>6}: DC gain {:+.4} ({:+.2} dB), peak {:.4} ({:+.3} dB)",
                fir.label(),
                dc,
                db(dc.abs()),
                peak,
                db(peak)
            );
            assert!(dc.abs() <= 1.0, "{fir:?} DC {dc}");
            assert!(peak <= 1.0 + 1e-12, "{fir:?} peak {peak}");
        }
    }

    #[test]
    fn fir_responses_match_documentation() {
        for (p, &fir) in EchoFir::ALL.iter().enumerate() {
            let mut line = format!("{:>6}:", fir.label());
            for (k, &f) in DOC_FREQS.iter().enumerate() {
                let got = db(fir_magnitude(fir, f));
                line += &format!(" {f:.0}:{got:.2}");
                let want = DOC_TABLE[p][k];
                if want <= -999.0 {
                    assert!(got < -90.0, "{fir:?} at {f}: {got}");
                } else {
                    assert!((got - want).abs() < 0.006, "{fir:?} at {f}: {got} vs doc {want}");
                }
            }
            println!("{line}");
        }
    }

    #[test]
    fn soft_clip_and_quantizer_properties() {
        let mut prev = soft_clip(-10.0);
        for i in -10_000..=10_000 {
            let x = i as f32 / 1000.0;
            let y = soft_clip(x);
            assert!(y.abs() <= x.abs() + 1e-7 && y.abs() <= 1.0);
            assert!(y >= prev - 1e-7, "monotonic");
            prev = y;
        }
        assert_eq!(soft_clip(0.5), 0.5);
        assert_eq!(quantize16(1.0 / 32_768.0 * 0.99), 0);
        assert_eq!(quantize16(-1.0 / 32_768.0 * 0.99), 0);
        assert_eq!(quantize16(2.0), 32_767);
        assert_eq!(quantize16(-2.0), -32_768);
        assert_eq!(quantize16(f32::NAN), 0);
    }

    /// Runs the core on an impulse with given parameters; returns the left/right output.
    fn core_impulse(delay: usize, fb: f32, width: f32, fir: EchoFir, n: usize, right: bool) -> (Vec<f32>, Vec<f32>) {
        let mut core = EchoCore::new(0);
        core.set_delay(delay);
        core.set_levels(fb, 1.0, width);
        core.set_fir(fir);
        core.set_input_on(true);
        core.reset();
        core.snap_input();
        let mut l = vec![0f32; n];
        let mut r = vec![0f32; n];
        if right {
            r[0] = 0.5;
        } else {
            l[0] = 0.5;
        }
        core.process(&mut l, &mut r);
        (l, r)
    }

    #[test]
    fn repeats_are_spaced_by_the_delay_and_decay_by_feedback() {
        let d = 3200;
        let (l, r) = core_impulse(d, 0.5, 0.0, EchoFir::Flat, 4 * d, false);
        assert!(l[..d].iter().all(|&v| v == 0.0));
        let q = |x: f32| quantize16(x) as f32 / 32_768.0;
        let first = q(0.5) * 127.0 / 128.0;
        assert!((l[d] - first).abs() < 1e-6, "{} vs {first}", l[d]);
        let second = q(0.5 * first) * 127.0 / 128.0;
        assert!((l[2 * d] - second).abs() < 1e-4, "{} vs {second}", l[2 * d]);
        assert!(r.iter().all(|&v| v == 0.0), "width 0: channels independent");
    }

    #[test]
    fn ping_pong_alternates_sides() {
        let d = 1600;
        let (l, r) = core_impulse(d, 0.8, 1.0, EchoFir::Flat, 4 * d, false);
        assert!(l[d].abs() > 0.2 && r[d] == 0.0, "first repeat left");
        assert!(r[2 * d].abs() > 0.1 && l[2 * d] == 0.0, "second repeat right");
        assert!(l[3 * d].abs() > 0.05 && r[3 * d] == 0.0, "third repeat left");
        // A right-only input is summed into the left line too.
        let (l, _) = core_impulse(d, 0.0, 1.0, EchoFir::Flat, 2 * d, true);
        assert!(l[d].abs() > 0.2);
    }

    #[test]
    fn stable_for_60_s_of_noise_at_max_feedback_all_presets_and_decays_to_silence() {
        let fs = INTERNAL_RATE as usize;
        for &fir in EchoFir::ALL {
            for width in [0.0f32, 0.5, 1.0] {
                let mut core = EchoCore::new(0);
                core.set_delay(fs / 10);
                core.set_levels(MAX_FEEDBACK, 1.0, width);
                core.set_fir(fir);
                core.set_input_on(true);
                core.reset();
                core.snap_input();
                let mut rng = Rng::new(0xEC40 + fir.index() as u64);
                let block = 256;
                let mut l = vec![0f32; block];
                let mut r = vec![0f32; block];
                let mut peak = 0f32;
                for _ in 0..(60 * fs / block) {
                    for i in 0..block {
                        l[i] = rng.bipolar() * 0.7;
                        r[i] = rng.bipolar() * 0.7;
                    }
                    core.process(&mut l, &mut r);
                    for (&a, &b) in l.iter().zip(&r) {
                        assert!(a.is_finite() && b.is_finite());
                        peak = peak.max(a.abs()).max(b.abs());
                    }
                }
                // Lines are bounded by the soft clipper; the wet output by the FIR's ℓ1 norm.
                let l1: f32 = fir_coeffs(fir).iter().map(|&c| (c as f32).abs()).sum::<f32>() / 128.0;
                assert!(peak <= l1 + 1e-3, "{fir:?} w {width}: peak {peak}");
                // Switch the input off: the tail must reach exact digital silence (no limit cycle).
                core.set_input_on(false);
                let mut silent_at = None;
                for b in 0..(120 * fs / block) {
                    l.iter_mut().for_each(|x| *x = 0.0);
                    r.iter_mut().for_each(|x| *x = 0.0);
                    core.process(&mut l, &mut r);
                    if core.is_silent(0) {
                        silent_at = Some(b * block);
                        break;
                    }
                }
                let t = silent_at.unwrap_or_else(|| panic!("{fir:?} w {width}: tail never became silent"));
                // Silent means exact digital zero from now on (no limit cycle, no residue).
                for _ in 0..(2 * fs / 10 / block + 2) {
                    l.iter_mut().for_each(|x| *x = 0.0);
                    r.iter_mut().for_each(|x| *x = 0.0);
                    core.process(&mut l, &mut r);
                    assert!(l.iter().chain(&r).all(|&v| v == 0.0), "{fir:?} w {width}: residue after silence");
                }
                println!(
                    "{:>6} width {width:.1}: 60 s noise @ fb 95 %: wet peak {peak:.3}; silent {:.2} s after input off",
                    fir.label(),
                    t as f64 / fs as f64
                );
            }
        }
    }

    #[test]
    fn time_change_crossfades_without_clicks() {
        // A steady sine through the echo; change the delay and check the wet output has no
        // discontinuity larger than the sine's own slope allows.
        let fs = INTERNAL_RATE as f64;
        let mut core = EchoCore::new(0);
        core.set_delay(4000);
        core.set_levels(0.0, 1.0, 0.0);
        core.set_fir(EchoFir::Flat);
        core.set_input_on(true);
        core.reset();
        core.snap_input();
        let f = 440.0;
        let mut prev = 0f32;
        let mut max_jump = 0f32;
        let mut t = 0usize;
        for b in 0..400 {
            if b == 200 {
                core.set_delay(4000 + 777);
            }
            let mut l: Vec<f32> = (0..64).map(|i| (0.5 * (2.0 * PI * f * (t + i) as f64 / fs).sin()) as f32).collect();
            let mut r = l.clone();
            core.process(&mut l, &mut r);
            for &y in &l {
                if t > 5000 {
                    max_jump = max_jump.max((y - prev).abs());
                }
                prev = y;
                t += 1;
            }
        }
        let slope = (0.5 * 2.0 * PI * f / fs) as f32;
        assert!(max_jump < slope * 1.5, "jump {max_jump} vs sine slope {slope}");
        assert_eq!(core.target_delay(), 4777);
    }

    #[test]
    fn synced_time_follows_tempo() {
        assert!((echo_time_s(true, EchoDiv::D8D, 120.0, 250.0) - 0.375).abs() < 1e-12);
        assert!((echo_time_s(true, EchoDiv::D4, 60.0, 250.0) - 1.0).abs() < 1e-12);
        assert_eq!(echo_time_s(true, EchoDiv::D4, 20.0, 250.0), MAX_TIME_S);
        assert_eq!(echo_time_s(true, EchoDiv::D32, 999.0, 250.0), 0.0075f64.max(MIN_TIME_S));
        assert!((echo_time_s(false, EchoDiv::D4, 120.0, 330.0) - 0.33).abs() < 1e-6);
        assert!((echo_time_s(true, EchoDiv::D4, f64::NAN, 0.0) - 0.5).abs() < 1e-12);
    }

    /// Host-rate impulse response of the full echo (resampled path).
    fn host_impulse(fs: f32, time_s: f64, fb: f32, n: usize) -> Vec<f32> {
        let mut e = Echo::new(fs, 256);
        e.set_params(true, time_s, fb, 1.0, EchoFir::Flat, 0.0);
        e.reset();
        let mut out = vec![0f32; n];
        let mut i = 0;
        while i < n {
            let len = 256.min(n - i);
            let mut l = vec![0f32; len];
            let mut r = vec![0f32; len];
            if i == 0 {
                l[0] = 0.5;
            }
            e.process(&mut l, &mut r);
            // Subtract the dry impulse to keep the wet only.
            if i == 0 {
                l[0] -= 0.5;
            }
            out[i..i + len].copy_from_slice(&l);
            i += len;
        }
        assert_eq!(e.starved(), 0);
        out
    }

    /// Sub-sample position of the energy peak around `approx` (parabolic interpolation).
    fn peak_pos(x: &[f32], approx: usize, radius: usize) -> f64 {
        let lo = approx.saturating_sub(radius);
        let hi = (approx + radius).min(x.len() - 2);
        let mut best = lo;
        for i in lo..hi {
            if x[i].abs() > x[best].abs() {
                best = i;
            }
        }
        let (a, b, c) = (x[best - 1] as f64, x[best] as f64, x[best + 1] as f64);
        let den = a - 2.0 * b + c;
        best as f64 + if den.abs() > 1e-12 { 0.5 * (a - c) / den } else { 0.0 }
    }

    #[test]
    fn host_repeats_land_exactly_on_multiples_of_the_echo_time() {
        for fs in [44_100.0f32, 48_000.0, 96_000.0, 32_000.0] {
            let t = 0.125;
            let x = host_impulse(fs, t, 0.6, (fs as f64 * 0.45) as usize);
            for k in 1..=3 {
                let want = k as f64 * t * fs as f64;
                let got = peak_pos(&x, want.round() as usize, 40);
                println!("fs {fs}: repeat {k} at {got:.3} samples (ideal {want:.3})");
                assert!((got - want).abs() < 0.6, "fs {fs} repeat {k}: {got} vs {want}");
            }
        }
    }

    #[test]
    fn switching_off_lets_the_tail_ring_then_bypasses() {
        let fs = 48_000.0;
        let mut e = Echo::new(fs, 128);
        e.set_params(true, 0.05, 0.7, 0.5, EchoFir::Warm, 0.3);
        e.reset();
        let mut rng = Rng::new(7);
        for _ in 0..100 {
            let mut l: Vec<f32> = (0..128).map(|_| rng.bipolar() * 0.5).collect();
            let mut r = l.clone();
            e.process(&mut l, &mut r);
        }
        assert!(e.tail_samples() > 0);
        e.set_params(false, 0.05, 0.7, 0.5, EchoFir::Warm, 0.3);
        // Right after switching off, the wet tail continues.
        let mut l = vec![0f32; 128 * 40];
        let mut r = l.clone();
        e.process(&mut l, &mut r);
        assert!(l.iter().any(|v| v.abs() > 1e-3), "tail keeps ringing");
        assert!(e.is_active());
        let mut blocks = 0;
        while e.is_active() && blocks < 10_000 {
            let mut l = vec![0f32; 128];
            let mut r = l.clone();
            e.process(&mut l, &mut r);
            blocks += 1;
        }
        assert!(!e.is_active(), "echo must bypass once silent");
        assert_eq!(e.tail_samples(), 0);
        println!("tail decayed and bypassed after {:.2} s", blocks as f64 * 128.0 / fs as f64);
        // Bypassed: exact pass-through.
        let mut l: Vec<f32> = (0..128).map(|i| i as f32 * 0.001).collect();
        let src = l.clone();
        let mut r = l.clone();
        e.process(&mut l, &mut r);
        assert_eq!(l, src);
    }

    #[test]
    fn fir_preset_change_is_smooth() {
        let mut core = EchoCore::new(0);
        core.set_delay(800);
        core.set_levels(0.5, 1.0, 0.0);
        core.set_fir(EchoFir::Flat);
        core.set_input_on(true);
        core.reset();
        core.snap_input();
        let fs = INTERNAL_RATE as f64;
        // Click detector: the second difference of the wet output. A 200 Hz sine of
        // amplitude ≤ 0.6 has |Δ²| ≤ 0.6·(2π·200/32000)² ≈ 9e-4; a discontinuity of size J
        // shows up as |Δ²| ≈ J.
        let mut hist = [0f32; 2];
        let mut before = 0f32;
        let mut after = 0f32;
        let mut t = 0usize;
        for b in 0..300 {
            if b == 150 {
                core.set_fir(EchoFir::Bright);
            }
            let mut l: Vec<f32> =
                (0..64).map(|i| (0.3 * (2.0 * PI * 200.0 * (t + i) as f64 / fs).sin()) as f32).collect();
            let mut r = l.clone();
            core.process(&mut l, &mut r);
            for &y in &l {
                let d2 = (y - 2.0 * hist[0] + hist[1]).abs();
                if t > 4000 {
                    if b < 150 {
                        before = before.max(d2);
                    } else {
                        after = after.max(d2);
                    }
                }
                hist = [y, hist[0]];
                t += 1;
            }
        }
        println!("FIR preset change: max |Δ²| before {before:.2e}, during/after {after:.2e}");
        assert!(after < 2e-3, "{after}");
    }
}
