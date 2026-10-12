//! First-order filters: the chip output filters (SPEC §5.3) and the always-on DC blocker.
//!
//! - High-passes use the bilinear transform with frequency prewarping (identical to a TPT /
//!   zero-delay-feedback one-pole): exact −3 dB at the cutoff, exact zero at DC.
//! - The 14 kHz NES low-pass is close to Nyquist at 44.1/48 kHz, where the bilinear transform
//!   would squash the response (−13 dB at 20 kHz instead of the analog −4.8 dB). It is
//!   therefore *magnitude-matched*: the pole is chosen so the digital magnitude equals the
//!   analog RC prototype at DC, at Nyquist and at the cutoff (or at `fs/4` when the cutoff
//!   lies above it).
//!
//! All state is `f64` (cheap for first-order sections, and keeps 5 Hz poles at 192 kHz
//! accurate). Coefficients are recomputed by [`ChipFilter::set_sample_rate`].
//!
//! Mode switches crossfade the paths over 10 ms. Leaving NES/GB for no filter additionally
//! releases the old mode's first high-pass gradually (its cutoff glides down to DC), because
//! the unipolar DC level the high-pass removed only settles slowly after such a switch (the
//! engine bridges it to avoid a thump) and must not be exposed all at once.

use std::f64::consts::PI;

/// Chip output filter cutoffs (NESdev APU mixer model; DMG output capacitor).
pub mod cutoffs {
    /// NES first high-pass (Hz).
    pub const NES_HPF1: f64 = 90.0;
    /// NES second high-pass (Hz).
    pub const NES_HPF2: f64 = 440.0;
    /// NES low-pass (Hz).
    pub const NES_LPF: f64 = 14_000.0;
    /// Game Boy (DMG) output high-pass (Hz).
    pub const GB_HPF: f64 = 28.0;
    /// DC blocker after the chip filter (Hz).
    pub const DC_BLOCK: f64 = 5.0;
}

/// Duration of the crossfade when the chip filter mode changes (seconds).
pub const MODE_XFADE_S: f64 = 0.010;
/// Leaving a high-passed mode for `None`: time constant (seconds) of the glide of the
/// transitional high-pass from the old mode's first cutoff down to DC.
pub const RELEASE_GLIDE_S: f64 = 0.25;
/// Cutoff (Hz) at which the transitional high-pass stops gliding and lets go of the low end it
/// still removes.
pub const RELEASE_END_HZ: f64 = 0.5;
/// Time constant (seconds) of that final let-go.
pub const RELEASE_FADE_S: f64 = 0.1;

/// A first-order IIR section `H(z) = (b0 + b1 z⁻¹) / (1 + a1 z⁻¹)` in transposed direct
/// form II.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OnePole {
    b0: f64,
    b1: f64,
    a1: f64,
    s: f64,
}

impl Default for OnePole {
    fn default() -> Self {
        Self::identity()
    }
}

impl OnePole {
    /// Pass-through section.
    pub const fn identity() -> Self {
        Self { b0: 1.0, b1: 0.0, a1: 0.0, s: 0.0 }
    }

    /// First-order high-pass `s / (s + ωc)` via the prewarped bilinear transform.
    pub fn highpass(fc: f64, fs: f64) -> Self {
        let k = prewarp(fc, fs);
        let n = 1.0 / (1.0 + k);
        Self { b0: n, b1: -n, a1: (k - 1.0) * n, s: 0.0 }
    }

    /// First-order low-pass `ωc / (s + ωc)` via the prewarped bilinear transform.
    pub fn lowpass_bilinear(fc: f64, fs: f64) -> Self {
        let k = prewarp(fc, fs);
        let n = 1.0 / (1.0 + k);
        Self { b0: k * n, b1: k * n, a1: (k - 1.0) * n, s: 0.0 }
    }

    /// First-order low-pass whose magnitude matches the analog RC prototype at DC, Nyquist
    /// and at `min(fc, fs/4)`.
    pub fn lowpass_matched(fc: f64, fs: f64) -> Self {
        let fc = fc.max(1e-3);
        let analog = |f: f64| 1.0 / (1.0 + (f / fc).powi(2)).sqrt();
        let hn = analog(fs * 0.5);
        let fm = fc.min(fs * 0.25);
        let target = analog(fm);
        let make = |p: f64| {
            let b0 = 0.5 * ((1.0 - p) + (1.0 + p) * hn);
            let b1 = 0.5 * ((1.0 - p) - (1.0 + p) * hn);
            Self { b0, b1, a1: -p, s: 0.0 }
        };
        // |H(fm)| decreases monotonically with the pole p; bisect for the match.
        let err = |p: f64| make(p).magnitude(fm, fs) - target;
        let (mut lo, mut hi) = (-0.95f64, 0.999_999f64);
        if err(lo) >= 0.0 && err(hi) <= 0.0 {
            for _ in 0..80 {
                let mid = 0.5 * (lo + hi);
                if err(mid) > 0.0 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            make(0.5 * (lo + hi))
        } else {
            // Not bracketed (degenerate cutoff): impulse-invariant pole, still DC/Nyquist matched.
            make((-2.0 * PI * fc / fs).exp())
        }
    }

    /// Processes one sample.
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.s;
        self.s = self.b1 * x - self.a1 * y;
        y
    }

    /// Sets the internal state so that a constant input `x` produces a constant output
    /// (steady state), avoiding a start-up transient.
    pub fn settle_to(&mut self, x: f64) {
        // Steady state: y = H(1)·x, s = b1·x − a1·y.
        let dc = (self.b0 + self.b1) / (1.0 + self.a1);
        let y = dc * x;
        self.s = y - self.b0 * x;
    }

    /// Clears the state.
    #[inline]
    pub fn reset(&mut self) {
        self.s = 0.0;
    }

    /// Flushes a negligible state to exact zero (keeps silent tails from lingering in the
    /// subnormal range).
    #[inline]
    pub fn flush_tiny(&mut self) {
        if self.s.abs() < 1e-30 {
            self.s = 0.0;
        }
    }

    /// Magnitude response at `f` Hz for sample rate `fs`.
    pub fn magnitude(&self, f: f64, fs: f64) -> f64 {
        let w = 2.0 * PI * f / fs;
        let (c, s) = (w.cos(), w.sin());
        let nr = self.b0 + self.b1 * c;
        let ni = -self.b1 * s;
        let dr = 1.0 + self.a1 * c;
        let di = -self.a1 * s;
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }

    /// Copies the coefficients of `other`, keeping this section's state.
    #[inline]
    pub fn set_coeffs_from(&mut self, other: &Self) {
        self.b0 = other.b0;
        self.b1 = other.b1;
        self.a1 = other.a1;
    }
}

/// Bilinear prewarp `tan(π fc / fs)`, with the cutoff clamped just below Nyquist.
fn prewarp(fc: f64, fs: f64) -> f64 {
    let fc = fc.clamp(1e-3, fs * 0.4999);
    (PI * fc / fs).tan()
}

/// Which chip output filter is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipFilterMode {
    /// No filter (FREE mode, or analog output off).
    None,
    /// NES: HPF 90 Hz → HPF 440 Hz → LPF 14 kHz.
    Nes,
    /// Game Boy: HPF 28 Hz.
    Gb,
}

impl ChipFilterMode {
    fn index(self) -> usize {
        match self {
            ChipFilterMode::None => 0,
            ChipFilterMode::Nes => 1,
            ChipFilterMode::Gb => 2,
        }
    }
}

/// Per-channel chip filter paths. All paths run continuously (four first-order sections per
/// channel — negligible), so mode switches are a pure output crossfade without warm-up
/// transients.
#[derive(Clone, Copy, Debug, Default)]
struct ChannelPaths {
    nes: [OnePole; 3],
    gb: OnePole,
}

/// Transitional high-pass of the `None` path after leaving a high-passed mode.
///
/// NES/GB with `analog` on render unipolar DAC levels whose DC the chip filter removes; FREE
/// and `analog` off render without DC and without a chip filter. The engine bridges a voice's
/// DC level across such a switch and lets it settle slowly (no thump), but a plain crossfade
/// to the unfiltered path would expose that settling DC at once. So the `None` path starts as
/// the old mode's first high-pass (same output at the switch) and its cutoff glides down to DC
/// ([`RELEASE_GLIDE_S`]); below [`RELEASE_END_HZ`] the little low end it still removes fades
/// out ([`RELEASE_FADE_S`]) and the path is an exact bypass again.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Release {
    /// Exact bypass.
    Off,
    /// Starts with the next sample, from the high-pass of this cutoff (Hz) in the old path.
    Pending(ChipFilterMode),
    /// Gliding: the removed low end is tracked at cutoff `fc` (Hz).
    Glide { fc: f64 },
    /// Letting go of the removed low end.
    Fade,
}

/// Stereo chip output filter with click-free mode switching (SPEC §5.3).
#[derive(Clone, Debug)]
pub struct ChipFilter {
    fs: f64,
    ch: [ChannelPaths; 2],
    mode: ChipFilterMode,
    /// Linear crossfade weights of the None/NES/GB paths (smoothstep-shaped and
    /// renormalized on output).
    weight: [f64; 3],
    step: f64,
    /// Transitional high-pass of the `None` path and the low end it removes per channel.
    release: Release,
    low: [f64; 2],
    /// No sample has been filtered since construction or [`Self::reset`]: a mode change has
    /// nothing to crossfade or release and takes effect at once (e.g. the first parameters
    /// after a state load).
    fresh: bool,
}

impl ChipFilter {
    /// Creates the filter for `fs`, starting in `ChipFilterMode::None`.
    pub fn new(fs: f64) -> Self {
        let mut f = Self {
            fs,
            ch: [ChannelPaths::default(); 2],
            mode: ChipFilterMode::None,
            weight: [1.0, 0.0, 0.0],
            step: 1.0,
            release: Release::Off,
            low: [0.0; 2],
            fresh: true,
        };
        f.set_sample_rate(fs);
        f
    }

    /// Recomputes coefficients for a new sample rate (keeps filter state).
    pub fn set_sample_rate(&mut self, fs: f64) {
        self.fs = fs;
        let hp1 = OnePole::highpass(cutoffs::NES_HPF1, fs);
        let hp2 = OnePole::highpass(cutoffs::NES_HPF2, fs);
        let lp = OnePole::lowpass_matched(cutoffs::NES_LPF, fs);
        let gb = OnePole::highpass(cutoffs::GB_HPF, fs);
        for c in &mut self.ch {
            c.nes[0].set_coeffs_from(&hp1);
            c.nes[1].set_coeffs_from(&hp2);
            c.nes[2].set_coeffs_from(&lp);
            c.gb.set_coeffs_from(&gb);
        }
        self.step = 1.0 / (MODE_XFADE_S * fs).max(1.0);
    }

    /// Selects the active mode; the change is crossfaded over [`MODE_XFADE_S`]. Leaving a
    /// high-passed mode for `None` releases its low end gradually (see [`Release`]).
    pub fn set_mode(&mut self, mode: ChipFilterMode) {
        if self.fresh {
            self.mode = mode;
            self.weight = [0.0; 3];
            self.weight[mode.index()] = 1.0;
            self.release = Release::Off;
            return;
        }
        if mode == ChipFilterMode::None && self.mode != ChipFilterMode::None {
            self.release = Release::Pending(self.mode);
        }
        self.mode = mode;
    }

    /// Current (target) mode.
    pub fn mode(&self) -> ChipFilterMode {
        self.mode
    }

    /// Jumps to the target mode without crossfade and clears the state.
    pub fn reset(&mut self) {
        for c in &mut self.ch {
            for f in &mut c.nes {
                f.reset();
            }
            c.gb.reset();
        }
        self.weight = [0.0; 3];
        self.weight[self.mode.index()] = 1.0;
        self.release = Release::Off;
        self.low = [0.0; 2];
        self.fresh = true;
    }

    /// Output of the `None` path for channel `c` (input `x`, the old paths' first high-pass
    /// outputs `h_nes`, `h_gb`), advancing the transitional high-pass by one sample.
    #[inline]
    fn none_path(&mut self, c: usize, x: f64, h_nes: f64, h_gb: f64) -> f64 {
        match self.release {
            Release::Off => x,
            Release::Pending(from) => {
                // Same output as the old path's first high-pass: it removed `x − h`.
                self.low[c] = x - if from == ChipFilterMode::Nes { h_nes } else { h_gb };
                x - self.low[c]
            }
            Release::Glide { fc } => {
                let y = x - self.low[c];
                self.low[c] += (2.0 * PI * fc / self.fs).min(1.0) * y;
                y
            }
            Release::Fade => x - self.low[c],
        }
    }

    /// Advances the release state machine after both channels' samples.
    #[inline]
    fn advance_release(&mut self) {
        match self.release {
            Release::Off => {}
            Release::Pending(from) => {
                let fc = if from == ChipFilterMode::Nes { cutoffs::NES_HPF1 } else { cutoffs::GB_HPF };
                self.release = Release::Glide { fc };
            }
            Release::Glide { fc } => {
                let fc = fc * (-1.0 / (RELEASE_GLIDE_S * self.fs)).exp();
                self.release = if fc < RELEASE_END_HZ { Release::Fade } else { Release::Glide { fc } };
            }
            Release::Fade => {
                let k = (-1.0 / (RELEASE_FADE_S * self.fs)).exp();
                for v in &mut self.low {
                    *v *= k;
                }
                if self.low.iter().all(|v| v.abs() < 1e-9) {
                    self.release = Release::Off;
                    self.low = [0.0; 2];
                }
            }
        }
    }

    /// Filters a stereo block in place.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let target = self.mode.index();
        let n = l.len().min(r.len());
        if n > 0 {
            self.fresh = false;
        }
        let settled = self.weight[target] == 1.0;
        for i in 0..n {
            let mut out = [0f64; 2];
            for (c, x) in [l[i], r[i]].into_iter().enumerate() {
                let x = x as f64;
                let p = &mut self.ch[c];
                let h1 = p.nes[0].process(x);
                let h2 = p.nes[1].process(h1);
                let nes = p.nes[2].process(h2);
                let gb = p.gb.process(x);
                let x = self.none_path(c, x, h1, gb);
                out[c] = if settled {
                    [x, nes, gb][target]
                } else {
                    // Smoothstep-shaped weights: C¹-continuous crossfade (no slope kinks).
                    let w = self.weight.map(|w| w * w * (3.0 - 2.0 * w));
                    (w[0] * x + w[1] * nes + w[2] * gb) / (w[0] + w[1] + w[2]).max(1e-9)
                };
            }
            if !settled {
                for (k, w) in self.weight.iter_mut().enumerate() {
                    *w = if k == target { (*w + self.step).min(1.0) } else { (*w - self.step).max(0.0) };
                }
            }
            self.advance_release();
            l[i] = out[0] as f32;
            r[i] = out[1] as f32;
        }
        if self.weight[target] >= 1.0 {
            self.weight = [0.0; 3];
            self.weight[target] = 1.0;
        }
        for c in &mut self.ch {
            for f in &mut c.nes {
                f.flush_tiny();
            }
            c.gb.flush_tiny();
        }
    }

    /// Magnitude response of a mode's filter path at `f` Hz (for tests and documentation).
    pub fn magnitude(&self, mode: ChipFilterMode, f: f64) -> f64 {
        let p = &self.ch[0];
        match mode {
            ChipFilterMode::None => 1.0,
            ChipFilterMode::Nes => p.nes.iter().map(|s| s.magnitude(f, self.fs)).product(),
            ChipFilterMode::Gb => p.gb.magnitude(f, self.fs),
        }
    }
}

/// Stereo first-order DC blocker (5 Hz high-pass), always active after the chip filter.
#[derive(Clone, Debug)]
pub struct DcBlocker {
    f: [OnePole; 2],
}

impl DcBlocker {
    /// Creates the blocker for `fs`.
    pub fn new(fs: f64) -> Self {
        let hp = OnePole::highpass(cutoffs::DC_BLOCK, fs);
        Self { f: [hp, hp] }
    }

    /// Recomputes coefficients (keeps state).
    pub fn set_sample_rate(&mut self, fs: f64) {
        let hp = OnePole::highpass(cutoffs::DC_BLOCK, fs);
        for f in &mut self.f {
            f.set_coeffs_from(&hp);
        }
    }

    /// Clears the state.
    pub fn reset(&mut self) {
        for f in &mut self.f {
            f.reset();
        }
    }

    /// Filters a stereo block in place.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (buf, f) in [l, r].into_iter().zip(self.f.iter_mut()) {
            for x in buf.iter_mut() {
                *x = f.process(*x as f64) as f32;
            }
            f.flush_tiny();
        }
    }

    /// Magnitude response at `f` Hz.
    pub fn magnitude(&self, f: f64, fs: f64) -> f64 {
        self.f[0].magnitude(f, fs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(x: f64) -> f64 {
        20.0 * x.log10()
    }

    fn analog_hp(f: f64, fc: f64) -> f64 {
        let r = f / fc;
        r / (1.0 + r * r).sqrt()
    }

    fn analog_lp(f: f64, fc: f64) -> f64 {
        1.0 / (1.0 + (f / fc).powi(2)).sqrt()
    }

    /// Measures the gain of a filter path by running a sine through it.
    fn measure(mode: ChipFilterMode, fs: f64, f: f64) -> f64 {
        let mut cf = ChipFilter::new(fs);
        cf.set_mode(mode);
        cf.reset();
        let n = (fs * 2.0) as usize;
        let mut l: Vec<f32> = (0..n).map(|i| (2.0 * PI * f * i as f64 / fs).sin() as f32).collect();
        let mut r = l.clone();
        cf.process(&mut l, &mut r);
        // Steady-state amplitude over the last half second (least-squares fit).
        let start = n - (fs * 0.5) as usize;
        let (mut ss, mut sc) = (0.0, 0.0);
        for (i, &y) in l.iter().enumerate().skip(start) {
            let ph = 2.0 * PI * f * i as f64 / fs;
            ss += y as f64 * ph.sin();
            sc += y as f64 * ph.cos();
        }
        let m = (n - start) as f64;
        2.0 * (ss * ss + sc * sc).sqrt() / m
    }

    #[test]
    fn nes_and_gb_magnitudes_match_analog_prototypes() {
        let freqs = [20.0, 90.0, 440.0, 1000.0, 5000.0, 10_000.0, 14_000.0, 18_000.0];
        for fs in [44_100.0, 48_000.0, 96_000.0, 192_000.0] {
            let cf = ChipFilter::new(fs);
            println!("fs {fs}: NES path (dB) digital vs analog");
            for &f in &freqs {
                if f >= fs * 0.45 {
                    continue;
                }
                let d = db(cf.magnitude(ChipFilterMode::Nes, f));
                let a = db(analog_hp(f, 90.0) * analog_hp(f, 440.0) * analog_lp(f, 14_000.0));
                println!("  {f:>7} Hz: {d:>8.3} vs {a:>8.3}");
                // HPFs are bilinear (exact at low f); the matched LPF stays within 0.5 dB.
                assert!((d - a).abs() < 0.5, "fs {fs} f {f}: {d} vs {a}");
            }
            let g = db(cf.magnitude(ChipFilterMode::Gb, 28.0));
            assert!((g + 3.0103).abs() < 0.01, "GB −3 dB at 28 Hz, got {g}");
            assert!(db(cf.magnitude(ChipFilterMode::Gb, 1000.0)).abs() < 0.01);
            // Exact −3 dB points of the NES high-passes (prewarped bilinear).
            let hp = OnePole::highpass(440.0, fs);
            assert!((db(hp.magnitude(440.0, fs)) + 3.0103).abs() < 1e-3);
            let lp = OnePole::lowpass_matched(14_000.0, fs);
            if fs * 0.25 >= 14_000.0 {
                assert!((db(lp.magnitude(14_000.0, fs)) + 3.0103).abs() < 1e-3);
            }
            assert!((lp.magnitude(0.0, fs) - 1.0).abs() < 1e-9);
            assert!((lp.magnitude(fs * 0.5, fs) - analog_lp(fs * 0.5, 14_000.0)).abs() < 1e-9);
        }
    }

    #[test]
    fn measured_sine_gains_match_the_design() {
        let fs = 48_000.0;
        for (f, mode) in [(1000.0, ChipFilterMode::Nes), (100.0, ChipFilterMode::Nes), (40.0, ChipFilterMode::Gb)] {
            let cf = ChipFilter::new(fs);
            let want = cf.magnitude(mode, f);
            let got = measure(mode, fs, f);
            assert!((db(got) - db(want)).abs() < 0.05, "{mode:?} {f} Hz: {got} vs {want}");
        }
        assert!((measure(ChipFilterMode::None, fs, 1000.0) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn dc_blocker_removes_dc() {
        let fs = 48_000.0;
        let mut dc = DcBlocker::new(fs);
        assert!((db(dc.magnitude(5.0, fs)) + 3.0103).abs() < 1e-3);
        assert!(db(dc.magnitude(1000.0, fs)).abs() < 1e-3);
        let mut l = vec![0.5f32; (fs * 2.0) as usize];
        let mut r = l.clone();
        dc.process(&mut l, &mut r);
        assert!(l.last().unwrap().abs() < 1e-4, "{}", l.last().unwrap());
    }

    #[test]
    fn mode_switch_is_click_free() {
        // A unipolar square-ish chip signal with DC; switching NES → GB → None must not
        // produce sample-to-sample jumps larger than the signal's own.
        let fs = 48_000.0;
        let mut cf = ChipFilter::new(fs);
        cf.set_mode(ChipFilterMode::Nes);
        cf.reset();
        let f = 220.0;
        let block = 64;
        let mut max_jump = 0f32;
        let mut prev = 0f32;
        let mut t = 0usize;
        for b in 0..3000 {
            if b == 1000 {
                cf.set_mode(ChipFilterMode::Gb);
            }
            if b == 2000 {
                cf.set_mode(ChipFilterMode::None);
            }
            let mut l: Vec<f32> =
                (0..block).map(|i| 0.4 + 0.3 * (2.0 * PI * f * (t + i) as f64 / fs).sin() as f32).collect();
            let mut r = l.clone();
            cf.process(&mut l, &mut r);
            for &y in &l {
                if t > 4800 {
                    max_jump = max_jump.max((y - prev).abs());
                }
                prev = y;
                t += 1;
            }
        }
        // The sine itself moves at most 2π·220/48000·0.3 ≈ 0.0086 per sample.
        assert!(max_jump < 0.02, "max jump {max_jump}");
    }

    #[test]
    fn mode_changes_before_the_first_sample_apply_at_once() {
        // A fresh instance starts from the defaults (GB) and receives a FREE patch's
        // parameters before any audio: nothing to crossfade or release, so the very first
        // sample is already an exact bypass.
        let mut cf = ChipFilter::new(48_000.0);
        cf.set_mode(ChipFilterMode::Gb);
        cf.reset();
        cf.set_mode(ChipFilterMode::None);
        let input: Vec<f32> = (0..2048).map(|i| if (i / 90) % 2 == 0 { 0.5 } else { -0.5 }).collect();
        let (mut l, mut r) = (input.clone(), input.clone());
        cf.process(&mut l, &mut r);
        assert_eq!(l, input);
        assert_eq!(r, input);
        // Once audio has run, the same change is crossfaded and released as usual.
        cf.set_mode(ChipFilterMode::Gb);
        let (mut l, mut r) = (input.clone(), input.clone());
        cf.process(&mut l, &mut r);
        cf.set_mode(ChipFilterMode::None);
        let (mut l, mut r) = (input.clone(), input.clone());
        cf.process(&mut l, &mut r);
        assert_ne!(l, input, "a switch after audio is not a jump");
    }

    #[test]
    fn leaving_a_high_passed_mode_releases_the_low_end_gradually() {
        // FX-03: GB (`analog` on) → FREE. The engine holds the unipolar DC level across its
        // voice crossfade and lets it settle with 100 ms; the chip filter must not expose that
        // settling DC at once (a 10 ms crossfade to the unfiltered path would: a thump). After
        // the release the None path is an exact bypass again.
        let fs = 48_000.0;
        let run = |release: bool| {
            let mut cf = ChipFilter::new(fs);
            cf.set_mode(ChipFilterMode::Gb);
            cf.reset();
            let n = (fs * 3.0) as usize;
            let switch = (fs * 0.5) as usize;
            let mut out = Vec::with_capacity(n);
            let mut input = Vec::with_capacity(n);
            for start in (0..n).step_by(64) {
                if start == switch {
                    cf.set_mode(ChipFilterMode::None);
                    if !release {
                        cf.release = Release::Off;
                    }
                }
                let mut l: Vec<f32> = (start..start + 64)
                    .map(|i| {
                        let t = i as f64 / fs;
                        let dc = if i < switch { 0.33 } else { 0.33 * (-(t - 0.5) / 0.1).exp() };
                        (dc + 0.3 * (2.0 * PI * 523.0 * t).sin()) as f32
                    })
                    .collect();
                input.extend_from_slice(&l);
                let mut r = l.clone();
                cf.process(&mut l, &mut r);
                out.extend_from_slice(&l);
            }
            // Low end of the output: mean over four periods of the tone.
            let w = 367;
            let low: Vec<f64> =
                out.chunks_exact(w).map(|c| c.iter().map(|v| *v as f64).sum::<f64>() / w as f64).collect();
            let worst = low.iter().skip(4).fold(0.0f64, |m, v| m.max(v.abs()));
            let bypass = out[n - 4800..] == input[n - 4800..];
            (worst, bypass)
        };
        let (released, bypass) = run(true);
        let (plain, _) = run(false);
        println!("low end after GB → None: {released:.4} with the release, {plain:.4} with a plain crossfade");
        assert!(released < 0.03, "{released}");
        assert!(plain > 0.2, "the plain crossfade exposes the DC ({plain})");
        assert!(bypass, "exact bypass after the release");
    }

    #[test]
    fn settle_to_removes_startup_transient() {
        let mut hp = OnePole::highpass(90.0, 48_000.0);
        hp.settle_to(0.7);
        assert!(hp.process(0.7).abs() < 1e-12);
        let mut lp = OnePole::lowpass_matched(14_000.0, 48_000.0);
        lp.settle_to(0.7);
        assert!((lp.process(0.7) - 0.7).abs() < 1e-12);
    }
}
