//! Voices, unison sub-voices and the chip mixers (SPEC §3, §4.3–§4.7).
//!
//! A [`Voice`] owns the per-note state (envelopes, sweep/vibrato/glide/arp timing, fades) and
//! up to [`MAX_UNISON`] [`SubVoice`]s. A sub-voice runs the four chip channels and mixes them:
//!
//! - **Linear** (GB, FREE, NES with `analog` off): every channel transition is one BLEP of
//!   `Δu · g · k` (per-unit gain × pan × unison gain), channels are independent.
//! - **NES 2A03 mixer** (NES with `analog` on): the non-linear pulse and triangle/noise group
//!   formulas are re-evaluated at every transition; channel transitions are merged in time
//!   order so each mixer step is exact. Stereo: each group's compression factor is applied to
//!   the pan-weighted DAC levels of its channels, so the mono sum is exactly the 2A03 output.
//!
//! Modulation is evaluated only at ticks (frame ticks in chip modes, every 16 samples in
//! FREE) and at note events. Each evaluation samples envelopes and modulators half a tick
//! ahead, i.e. at the centre of the coming step, so the chip modes' staircase is centred on
//! the ideal curve and a fast attack sounds at full level from the first frame.

use crate::blip::BlipBuffer;
use crate::chip::{
    edges_duty, free_noise_clock, gb_noise_clock, gb_pulse_freq, gb_pulse_reg, gb_quantize_pan, gb_wave_freq,
    gb_wave_reg, gb_wave_shift, nes_noise_clock, nes_pulse_freq, nes_pulse_timer, nes_triangle_freq,
    nes_triangle_timer, pan_gains, pitch_to_freq, GB_EDGES, NES_EDGES, NES_PULSE_MIN_TIMER, NES_TRIANGLE_MIN_TIMER,
};
use crate::env::Adsr;
use crate::modulation::{arp_index, arp_note, glide_offset, lfo, sweep_offset, vibrato_fade, Rng};
use crate::osc::{NoiseOsc, PulseOsc, WaveMode, WaveOsc, WaveTable, NEVER};
use crate::resolve::{sm, Resolved, Smoothed};
use pixl_core::{ArpChord, ArpMode, ChipMode, Curve, FrameRate};

/// Maximum unison sub-voices per voice.
pub const MAX_UNISON: usize = 4;
/// Duration of the click-free fade used for stolen voices, all-sound-off and mode switches.
pub const FADE_MS: f64 = 2.0;
/// Number of gain steps in a fade (each one a band-limited step).
pub const FADE_STEPS: u32 = 16;
/// FREE mode: largest amplitude change (fraction of full scale) between two amplitude
/// updates. Fast envelope segments get extra amplitude-only updates so they never zipper.
pub const MICRO_STEP: f64 = 0.01;
/// FREE-mode modulation interval (samples); mirrors the engine's control tick.
pub const FREE_CONTROL: f64 = 16.0;
/// Time constant of the smooth discharge of a level left behind by a finished voice (a halted
/// NES triangle), seconds.
pub const DISCHARGE_TAU: f64 = 0.16;
/// Discharged levels below this are removed in one (inaudible) step.
pub const DISCHARGE_FLOOR: f64 = 1e-6;

/// 2A03 pulse-group numerator.
const NES_P_NUM: f32 = 95.88;
/// 2A03 pulse-group denominator constant.
const NES_P_DEN: f32 = 8128.0;
/// 2A03 TND-group numerator.
const NES_T_NUM: f32 = 159.79;
/// 2A03 triangle weight divisor.
const NES_TRI_DIV: f32 = 8227.0;
/// 2A03 noise weight divisor.
const NES_NOISE_DIV: f32 = 12241.0;

/// Output scale of the 2A03 mixer: one pulse at level 15 swings exactly 1.0 (−6 dBFS peak
/// after DC removal for a 50 % duty), the same as GB and FREE (SPEC §4.3 calibration).
#[inline]
pub fn nes_k() -> f32 {
    1.0 / crate::chip::nes_pulse_mix(15.0)
}

/// Chip configuration a voice renders with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeCfg {
    /// Emulated chip.
    pub chip: ChipMode,
    /// Hardware DAC behaviour (the `analog` parameter).
    pub analog: bool,
}

impl ModeCfg {
    /// Unipolar hardware DAC levels (chip mode with `analog` on).
    pub fn unipolar(&self) -> bool {
        self.chip.is_chip() && self.analog
    }

    /// Non-linear 2A03 mixer.
    pub fn nes_mixer(&self) -> bool {
        self.chip == ChipMode::Nes && self.analog
    }
}

impl Default for ModeCfg {
    fn default() -> Self {
        Self { chip: ChipMode::Gb, analog: true }
    }
}

/// Performance controllers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Perf {
    /// Pitch bend −1..1.
    pub bend: f32,
    /// Mod wheel 0..1.
    pub mod_wheel: f32,
    /// Channel pressure 0..1.
    pub aftertouch: f32,
}

/// Global values at one modulation tick.
#[derive(Clone, Copy, Debug)]
pub struct Tick {
    /// Time in the chunk (samples).
    pub t: f64,
    /// How far ahead modulation is sampled (half the distance to the next tick).
    pub ahead: f64,
    /// Arpeggiator step counter valid from this tick.
    pub arp_count: u64,
    /// Smoothed parameters at this tick.
    pub sm: Smoothed,
}

/// Everything a voice reads while updating.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// Sample rate (Hz).
    pub fs: f64,
    /// Resolved parameters.
    pub res: &'a Resolved,
    /// Smoothed parameters at the evaluation time.
    pub sm: &'a Smoothed,
    /// Global arpeggiator step counter.
    pub arp_count: u64,
    /// Performance controllers.
    pub perf: Perf,
    /// Logically held notes, ascending.
    pub held_sorted: &'a [u8],
    /// Logically held notes, in the order they were played.
    pub held_order: &'a [u8],
    /// The wave RAM tables.
    pub wave: &'a WaveTable,
    /// The fixed NES triangle tables.
    pub tri: &'a WaveTable,
    /// The engine's current chip configuration (voices adopt it on start / mode switch).
    pub cfg: ModeCfg,
}

/// Phase/register resets applied to a sub-voice during an update (after the old output was
/// captured, so the bookkeeping stays exact).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reset {
    /// New cycle positions (0..1) of the two pulses.
    pub pulse_phase: Option<[f64; 2]>,
    /// New wave step position (0..32).
    pub wave_pos: Option<f64>,
    /// New noise register.
    pub lfsr: Option<u16>,
}

/// Per-voice values shared by its sub-voices during one update.
#[derive(Clone, Copy, Debug)]
struct Plan {
    /// Channel pitch in semitones (without unison detune).
    pitch: [f64; 4],
    /// Effective amplitude 0..1 (level × envelope × velocity).
    amp: [f32; 4],
    /// NES triangle gate.
    tri_gate: bool,
    /// Pitch modulation applied to a non-tracking noise index (semitones).
    noise_mod: f64,
    /// Unison gain 1/√n.
    uni_gain: f32,
    /// Chip configuration of the voice.
    cfg: ModeCfg,
}

// ---------------------------------------------------------------------------------------
// Sub-voice
// ---------------------------------------------------------------------------------------

/// One unison layer: the four chip channels and their mixer.
#[derive(Clone, Copy, Debug)]
pub struct SubVoice {
    /// Pulse 1 and 2.
    pub pulse: [PulseOsc; 2],
    /// Wave channel / NES triangle.
    pub wave: WaveOsc,
    /// Noise channel.
    pub noise: NoiseOsc,
    /// DAC mapping per channel: `dac = u·g + o`.
    g: [f32; 4],
    o: [f32; 4],
    /// Output gain per DAC unit (left/right), including pan, channel weight and unison gain.
    kl: [f32; 4],
    kr: [f32; 4],
    /// `g · k · fade`: gain of a unit-value change (linear mixer).
    al: [f32; 4],
    ar: [f32; 4],
    fade: f32,
    nes: bool,
    out_l: f32,
    out_r: f32,
    /// Pulse duty fractions of the last update (DAC centring in amplitude-only updates).
    duty: [f32; 2],
    /// Memo of the last pitch → frequency conversions (pitch is often constant).
    freq_key: [f64; 4],
    freq_val: [f64; 4],
    /// Memo of the last pan → gains conversions.
    pan_key: [f32; 4],
    pan_val: [(f32, f32); 4],
}

impl Default for SubVoice {
    fn default() -> Self {
        Self::new()
    }
}

impl SubVoice {
    /// A silent sub-voice.
    pub const fn new() -> Self {
        Self {
            pulse: [PulseOsc::new(), PulseOsc::new()],
            wave: WaveOsc::new(),
            noise: NoiseOsc::new(),
            g: [0.0; 4],
            o: [0.0; 4],
            kl: [0.0; 4],
            kr: [0.0; 4],
            al: [0.0; 4],
            ar: [0.0; 4],
            fade: 1.0,
            nes: false,
            out_l: 0.0,
            out_r: 0.0,
            duty: [0.5; 2],
            freq_key: [f64::NAN; 4],
            freq_val: [0.0; 4],
            pan_key: [f32::NAN; 4],
            pan_val: [(1.0, 1.0); 4],
        }
    }

    /// `f(slot) = key → value` memo for pitch → Hz.
    #[inline]
    fn freq(&mut self, slot: usize, pitch: f64) -> f64 {
        if self.freq_key[slot] != pitch {
            self.freq_key[slot] = pitch;
            self.freq_val[slot] = pitch_to_freq(pitch);
        }
        self.freq_val[slot]
    }

    /// Memo for the pan law.
    #[inline]
    fn pan(&mut self, c: usize, pan: f32) -> (f32, f32) {
        if self.pan_key[c] != pan {
            self.pan_key[c] = pan;
            self.pan_val[c] = pan_gains(pan);
        }
        self.pan_val[c]
    }

    /// Forget all output bookkeeping without emitting anything (callers make sure the
    /// integrator holds nothing of this sub-voice), and park the triangle at its zero step.
    pub fn clear(&mut self) {
        self.g = [0.0; 4];
        self.o = [0.0; 4];
        self.al = [0.0; 4];
        self.ar = [0.0; 4];
        self.fade = 1.0;
        self.out_l = 0.0;
        self.out_r = 0.0;
        self.wave.park(WaveOsc::TRIANGLE_REST_STEP);
    }

    #[inline]
    fn units(&self) -> [f32; 4] {
        [self.pulse[0].u(), self.pulse[1].u(), self.wave.u(), self.noise.value()]
    }

    /// DAC value of channel `c`.
    #[inline]
    fn dac(&self, c: usize, u: f32) -> f32 {
        u * self.g[c] + self.o[c]
    }

    /// 2A03 pulse group output (left, right) before the fade.
    #[inline]
    fn nes_pulse_group(&self) -> (f32, f32) {
        let d0 = self.dac(0, self.pulse[0].u());
        let d1 = self.dac(1, self.pulse[1].u());
        let cp = NES_P_NUM / (NES_P_DEN + 100.0 * (d0 + d1).max(0.0));
        (cp * (d0 * self.kl[0] + d1 * self.kl[1]), cp * (d0 * self.kr[0] + d1 * self.kr[1]))
    }

    /// 2A03 triangle/noise group output (left, right) before the fade.
    #[inline]
    fn nes_tnd_group(&self) -> (f32, f32) {
        let t = self.dac(2, self.wave.u()) / NES_TRI_DIV;
        let n = self.dac(3, self.noise.value()) / NES_NOISE_DIV;
        let ct = NES_T_NUM / (1.0 + 100.0 * (t + n).max(0.0));
        (ct * (t * self.kl[2] + n * self.kl[3]), ct * (t * self.kr[2] + n * self.kr[3]))
    }

    /// Current stereo output of this sub-voice (what the integrator has been told).
    #[inline]
    pub fn total(&self) -> (f32, f32) {
        let (l, r) = if self.nes {
            let (pl, pr) = self.nes_pulse_group();
            let (tl, tr) = self.nes_tnd_group();
            (pl + tl, pr + tr)
        } else {
            let mut l = 0.0;
            let mut r = 0.0;
            for (c, u) in self.units().into_iter().enumerate() {
                let d = self.dac(c, u);
                l += d * self.kl[c];
                r += d * self.kr[c];
            }
            (l, r)
        };
        (l * self.fade, r * self.fade)
    }

    /// Whether this sub-voice is (still) telling the integrator a non-zero level.
    pub fn holds_level(&self) -> bool {
        let (l, r) = self.total();
        l.abs() > 1e-9 || r.abs() > 1e-9
    }

    /// Refresh the per-unit transition gains (`g · k · fade`). In the NES path they only mark
    /// which channels are audible; the mixer itself uses `g`, `o`, `kl`, `kr` and `fade`.
    fn update_gains(&mut self) {
        for c in 0..4 {
            self.al[c] = self.g[c] * self.kl[c] * self.fade;
            self.ar[c] = self.g[c] * self.kr[c] * self.fade;
        }
    }

    #[inline]
    fn active(&self, c: usize) -> bool {
        self.al[c] != 0.0 || self.ar[c] != 0.0
    }

    /// Set the fade gain at time `t`, emitting the resulting level change.
    fn set_fade(&mut self, gain: f32, t: f64, blip: &mut BlipBuffer) {
        let (ol, or) = self.total();
        self.fade = gain.clamp(0.0, 1.0);
        self.update_gains();
        let (nl, nr) = self.total();
        emit(blip, t, nl - ol, nr - or);
        self.out_l = nl;
        self.out_r = nr;
    }

    /// Render all transitions before `t_end`.
    fn run(&mut self, t_end: f64, tab: &WaveTable, blip: &mut BlipBuffer) {
        if self.nes {
            self.run_nes(t_end, tab, blip);
            return;
        }
        for c in 0..2 {
            if self.active(c) {
                self.pulse[c].run(t_end, self.al[c], self.ar[c], blip);
            }
        }
        if self.active(2) {
            self.wave.run(t_end, self.al[2], self.ar[2], tab, blip);
        }
        if self.active(3) {
            self.noise.run(t_end, self.al[3], self.ar[3], blip);
        }
    }

    /// 2A03 mixer: merge the channels' transitions in time order and re-evaluate the
    /// non-linear mixer group that changed at each one.
    fn run_nes(&mut self, t_end: f64, tab: &WaveTable, blip: &mut BlipBuffer) {
        let act = [self.active(0), self.active(1), self.active(2), self.active(3)];
        if !act.iter().any(|a| *a) {
            return;
        }
        let mut gp = self.nes_pulse_group();
        let mut gt = self.nes_tnd_group();
        let fade = self.fade;
        loop {
            let mut t = t_end;
            let mut which = 4;
            if act[0] && self.pulse[0].next_t() < t {
                t = self.pulse[0].next_t();
                which = 0;
            }
            if act[1] && self.pulse[1].next_t() < t {
                t = self.pulse[1].next_t();
                which = 1;
            }
            if act[2] && self.wave.next_t() < t {
                t = self.wave.next_t();
                which = 2;
            }
            let id = self.noise.is_integrating();
            if act[3] {
                let tn = if id { self.noise.next_boundary() } else { self.noise.next_t() };
                if tn < t {
                    t = tn;
                    which = 3;
                }
            }
            let raw = match which {
                0 | 1 => {
                    self.pulse[which].commit();
                    gp = self.nes_pulse_group();
                    false
                }
                2 => {
                    if self.wave.commit(tab) == 0.0 {
                        continue;
                    }
                    gt = self.nes_tnd_group();
                    false
                }
                3 => {
                    let raw = if id {
                        if self.noise.boundary() == 0.0 {
                            continue;
                        }
                        true
                    } else {
                        self.noise.commit();
                        false
                    };
                    gt = self.nes_tnd_group();
                    raw
                }
                _ => break,
            };
            let l = (gp.0 + gt.0) * fade;
            let r = (gp.1 + gt.1) * fade;
            let (dl, dr) = (l - self.out_l, r - self.out_r);
            self.out_l = l;
            self.out_r = r;
            if raw {
                blip.add_raw(t.max(0.0) as usize, dl, dr);
            } else {
                emit(blip, t, dl, dr);
            }
        }
        if act[3] {
            self.noise.finish(t_end);
        }
    }

    /// Apply one update at time `at`: oscillator rates, DAC mapping, pans; emit the change.
    fn apply(&mut self, plan: &Plan, lin: f64, ctx: &Ctx<'_>, at: f64, reset: Option<&Reset>, blip: &mut BlipBuffer) {
        let cfg = plan.cfg;
        let (ol, or) = self.total();
        if let Some(r) = reset {
            if let Some(ph) = r.pulse_phase {
                self.pulse[0].set_phase(ph[0], at);
                self.pulse[1].set_phase(ph[1], at);
            }
            if let Some(p) = r.wave_pos {
                self.wave.set_position(p, at);
            }
            if let Some(s) = r.lfsr {
                self.noise.reset(s, at);
            }
        }
        let res = ctx.res;
        let fs = ctx.fs;
        let s = ctx.sm;
        let unipolar = cfg.unipolar();
        let chip = cfg.chip;
        let is_chip = chip.is_chip();
        let pal = res.frame_rate == FrameRate::Pal;
        let cpu = crate::chip::nes_cpu(pal);
        let det = s[sm::DETUNE] as f64 * lin / 100.0;
        let quant = |a: f32| if is_chip { (a * 15.0).round().clamp(0.0, 15.0) } else { (a * 15.0).clamp(0.0, 15.0) };

        // Pulses.
        for c in 0..2 {
            let f = self.freq(c, plan.pitch[c] + det);
            let duty_pct = s[sm::DUTY + c];
            let (rise, fall) = match chip {
                ChipMode::Nes => NES_EDGES[crate::chip::duty_index(duty_pct)],
                ChipMode::Gb => GB_EDGES[crate::chip::duty_index(duty_pct)],
                ChipMode::Free => (0.0, (duty_pct as f64 / 100.0).clamp(0.02, 0.98)),
            };
            let duty = edges_duty(rise, fall);
            let (fa, muted) = match chip {
                ChipMode::Nes => {
                    let t = nes_pulse_timer(f, cpu);
                    (nes_pulse_freq(t, cpu), t < NES_PULSE_MIN_TIMER)
                }
                ChipMode::Gb => (gb_pulse_freq(gb_pulse_reg(f)), false),
                ChipMode::Free => (f, false),
            };
            let stat = if muted {
                Some(0.0)
            } else if fa >= 0.5 * fs {
                Some(duty as f32)
            } else {
                None
            };
            self.pulse[c].configure(at, fa / fs, rise, fall, stat);
            self.duty[c] = duty as f32;
            let a = quant(plan.amp[c]);
            if muted {
                self.g[c] = 0.0;
                self.o[c] = 0.0;
            } else {
                self.g[c] = a;
                self.o[c] = if unipolar { 0.0 } else { -(duty as f32) * a };
            }
        }

        // Wave / NES triangle.
        let f = self.freq(2, plan.pitch[2] + det);
        match chip {
            ChipMode::Nes => {
                let t = nes_triangle_timer(f, cpu);
                let fa = nes_triangle_freq(t, cpu);
                let mode = if !plan.tri_gate || t < NES_TRIANGLE_MIN_TIMER {
                    WaveMode::Hold
                } else if fa >= 0.5 * fs {
                    WaveMode::Static(7.5)
                } else {
                    WaveMode::Run
                };
                self.wave.configure(at, 32.0 * fa / fs, 0, mode, ctx.tri);
                if res.on[2] {
                    self.g[2] = 1.0;
                    self.o[2] = if unipolar { 0.0 } else { -7.5 };
                } else {
                    // A disabled triangle contributes nothing (not even its held level).
                    self.g[2] = 0.0;
                    self.o[2] = 0.0;
                }
            }
            ChipMode::Gb => {
                let fa = gb_wave_freq(gb_wave_reg(f));
                let shift = gb_wave_shift(plan.amp[2]);
                let sh = shift.unwrap_or(0);
                let mode = if fa >= 0.5 * fs { WaveMode::Static(ctx.wave.mean(sh)) } else { WaveMode::Run };
                self.wave.configure(at, 32.0 * fa / fs, sh, mode, ctx.wave);
                match shift {
                    Some(sh) => {
                        self.g[2] = 1.0;
                        self.o[2] = if unipolar { 0.0 } else { -((15u8 >> sh) as f32) / 2.0 };
                    }
                    None => {
                        self.g[2] = 0.0;
                        self.o[2] = 0.0;
                    }
                }
            }
            ChipMode::Free => {
                let mode = if f >= 0.5 * fs { WaveMode::Static(ctx.wave.mean(0)) } else { WaveMode::Run };
                self.wave.configure(at, 32.0 * f / fs, 0, mode, ctx.wave);
                let a = plan.amp[2].clamp(0.0, 1.0);
                self.g[2] = a;
                self.o[2] = -7.5 * a;
            }
        }

        // Noise.
        let st = if res.ns_track { plan.pitch[3] + det - 60.0 } else { plan.noise_mod };
        let (clock, nes_lfsr) = match chip {
            ChipMode::Nes => {
                let idx = (res.ns_period - st).round().clamp(0.0, 15.0) as usize;
                (nes_noise_clock(idx, pal), true)
            }
            ChipMode::Gb => {
                let idx = (res.ns_period - st).round().clamp(0.0, 15.0) as usize;
                (gb_noise_clock(idx), false)
            }
            ChipMode::Free => {
                let x = (res.ns_period - st / 6.0).clamp(0.0, 15.0);
                // Memoised like the pitches: FREE noise clock = 262 144 Hz · 2^(−x/2).
                (self.freq(3, 69.0 - 6.0 * x) / 440.0 * free_noise_clock(0.0), false)
            }
        };
        self.noise.configure(at, clock / fs, nes_lfsr, res.ns_short);
        let a = quant(plan.amp[3]);
        self.g[3] = a;
        self.o[3] = if unipolar { 0.0 } else { -0.5 * a };

        // Mixer constants.
        self.nes = cfg.nes_mixer();
        let spread = s[sm::WIDTH] * lin as f32;
        for c in 0..4 {
            let mut pan = (s[sm::PAN + c] + spread).clamp(-100.0, 100.0);
            if chip == ChipMode::Gb {
                pan = gb_quantize_pan(pan);
            }
            let (pl, pr) = self.pan(c, pan);
            let w = if self.nes {
                1.0
            } else if chip == ChipMode::Nes {
                // Linearised 2A03 balance (analog off): full-scale ratios of the real mixer.
                [1.0, 1.0, crate::chip::nes_triangle_weight(), crate::chip::nes_noise_weight()][c] / 15.0
            } else {
                1.0 / 15.0
            };
            self.kl[c] = w * pl * plan.uni_gain;
            self.kr[c] = w * pr * plan.uni_gain;
        }
        if self.nes {
            // The 2A03 formulas include the output scale; fold it into k so `total` is calibrated.
            let k = nes_k();
            for c in 0..4 {
                self.kl[c] *= k;
                self.kr[c] *= k;
            }
        }
        self.update_gains();
        let (nl, nr) = self.total();
        emit(blip, at, nl - ol, nr - or);
        self.out_l = nl;
        self.out_r = nr;
    }

    /// FREE mode amplitude-only update (bipolar, linear mixer) at time `at`.
    fn set_amps(&mut self, amp: &[f32; 4], at: f64, blip: &mut BlipBuffer) {
        let (ol, or) = self.total();
        for (c, level) in amp.iter().take(2).enumerate() {
            let a = (level * 15.0).clamp(0.0, 15.0);
            self.g[c] = a;
            self.o[c] = -self.duty[c] * a;
        }
        let a = amp[2].clamp(0.0, 1.0);
        self.g[2] = a;
        self.o[2] = -7.5 * a;
        let a = (amp[3] * 15.0).clamp(0.0, 15.0);
        self.g[3] = a;
        self.o[3] = -0.5 * a;
        self.update_gains();
        let (nl, nr) = self.total();
        emit(blip, at, nl - ol, nr - or);
        self.out_l = nl;
        self.out_r = nr;
    }

    /// Shift the time origin by `dt` samples.
    fn shift_time(&mut self, dt: f64) {
        self.pulse[0].shift_time(dt);
        self.pulse[1].shift_time(dt);
        self.wave.shift_time(dt);
        self.noise.shift_time(dt);
    }
}

#[inline]
fn emit(blip: &mut BlipBuffer, t: f64, dl: f32, dr: f32) {
    if dl != 0.0 || dr != 0.0 {
        blip.add_step(t, dl, dr);
    }
}

// ---------------------------------------------------------------------------------------
// Voice
// ---------------------------------------------------------------------------------------

/// What kind of note source drives a voice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One played note (optionally arpeggiating its chord).
    Normal,
    /// The single voice of the HELD arpeggiator.
    HeldArp,
}

/// Why a voice is fading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FadeGoal {
    /// Stolen / all sound off: free the voice at gain 0.
    Kill,
    /// Chip mode change: adopt the new mode at gain 0, then fade back in.
    Switch,
    /// Fading back in after a switch.
    In,
}

#[derive(Clone, Copy, Debug)]
struct Fade {
    active: bool,
    goal: FadeGoal,
    gain: f32,
    step: f32,
    left: u32,
    interval: f64,
    next_t: f64,
}

impl Fade {
    const fn idle() -> Self {
        Self { active: false, goal: FadeGoal::Kill, gain: 1.0, step: 0.0, left: 0, interval: 1.0, next_t: NEVER }
    }
}

/// How a voice's render call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceEnd {
    /// Still sounding.
    Running,
    /// All enabled envelopes finished.
    Finished,
    /// A kill fade completed.
    Killed,
}

/// Arguments of a note start.
#[derive(Clone, Copy, Debug)]
pub struct StartArgs {
    /// MIDI note.
    pub note: u8,
    /// Velocity 0..1.
    pub velocity: f32,
    /// Start order (for "oldest" stealing and telemetry).
    pub order: u64,
    /// Glide start pitch (absolute semitones), if this note glides.
    pub glide_from: Option<f64>,
    /// Seed for unison phases and the random arp.
    pub seed: u64,
    /// Note source.
    pub kind: Kind,
    /// How far ahead the first evaluation samples modulation.
    pub ahead: f64,
}

/// One polyphonic voice.
#[derive(Clone, Copy, Debug)]
pub struct Voice {
    /// Rendering (sounding, releasing or fading).
    pub active: bool,
    /// Note source.
    pub kind: Kind,
    /// Current note (the voice's reference pitch).
    pub note: u8,
    /// Key logically held (pressed or sustained by the pedal).
    pub gate: bool,
    /// Velocity 0..1 of the note that (re)triggered the voice.
    pub velocity: f32,
    /// Start order (larger = more recent).
    pub order: u64,
    /// Free, but smoothly discharging the level it left behind (a halted NES triangle).
    pub discharging: bool,
    /// Remaining discharge level per sub-voice (left, right).
    dis: [(f64, f64); MAX_UNISON],
    /// Next FREE-mode amplitude-only update (chunk time), `NEVER` when not needed.
    t_amp: f64,
    env: [Adsr; 4],
    t_upd: f64,
    age: f64,
    vib_phase: f64,
    glide_from: f64,
    glide_t: f64,
    glide_time: f64,
    glide_curve: Curve,
    arp_c0: u64,
    arp_seed: u64,
    held_note: i16,
    /// Chip configuration the voice currently renders with.
    pub cfg: ModeCfg,
    n_sub: usize,
    sub: [SubVoice; MAX_UNISON],
    fade: Fade,
    tele_pitch: f32,
    tele_arp: Option<u8>,
}

impl Default for Voice {
    fn default() -> Self {
        Self::new()
    }
}

impl Voice {
    /// A free voice.
    pub fn new() -> Self {
        let mut v = Self {
            active: false,
            kind: Kind::Normal,
            note: 60,
            gate: false,
            velocity: 1.0,
            order: 0,
            discharging: false,
            dis: [(0.0, 0.0); MAX_UNISON],
            t_amp: NEVER,
            env: [Adsr::new(); 4],
            t_upd: 0.0,
            age: 0.0,
            vib_phase: 0.0,
            glide_from: 0.0,
            glide_t: 0.0,
            glide_time: 0.0,
            glide_curve: Curve::Exp,
            arp_c0: 0,
            arp_seed: 0,
            held_note: 60,
            cfg: ModeCfg::default(),
            n_sub: 1,
            sub: [SubVoice::new(); MAX_UNISON],
            fade: Fade::idle(),
            tele_pitch: 0.0,
            tele_arp: None,
        };
        v.clear();
        v
    }

    /// Back to a clean free state without emitting anything.
    pub fn clear(&mut self) {
        self.active = false;
        self.discharging = false;
        self.dis = [(0.0, 0.0); MAX_UNISON];
        self.t_amp = NEVER;
        self.gate = false;
        self.fade = Fade::idle();
        for s in &mut self.sub {
            s.clear();
        }
        for e in &mut self.env {
            e.reset();
        }
    }

    /// The level this slot currently contributes to the output (left, right).
    pub fn output_level(&self) -> (f64, f64) {
        if self.discharging {
            self.dis.iter().fold((0.0, 0.0), |(l, r), d| (l + d.0, r + d.1))
        } else {
            self.sub.iter().fold((0.0, 0.0), |(l, r), s| {
                let (a, b) = s.total();
                (l + a as f64, r + b as f64)
            })
        }
    }

    /// Remove whatever this slot contributes with one band-limited step at chunk time 0 and
    /// return to the clean state (slot recycling under extreme load).
    pub fn force_clean(&mut self, blip: &mut BlipBuffer) {
        let (l, r) = self.output_level();
        if l != 0.0 || r != 0.0 {
            blip.add_step(0.0, -l as f32, -r as f32);
        }
        self.clear();
    }

    /// Smoothly discharge the level left behind by a finished voice: an exponential decay
    /// with [`DISCHARGE_TAU`], emitted as one exact per-sample delta (no band-limiting needed
    /// for such a slow signal). Returns `true` once the slot is clean.
    pub fn discharge_run(&mut self, len: usize, fs: f64, blip: &mut BlipBuffer) -> bool {
        if !self.discharging {
            return true;
        }
        let k = (-1.0 / (DISCHARGE_TAU * fs.max(1.0))).exp();
        let (mut l, mut r) = self.output_level();
        for n in 0..len {
            let (nl, nr) = (l * k, r * k);
            blip.add_raw(n, (nl - l) as f32, (nr - r) as f32);
            l = nl;
            r = nr;
        }
        let kn = k.powi(len as i32);
        for d in &mut self.dis {
            d.0 *= kn;
            d.1 *= kn;
        }
        let (l, r) = self.output_level();
        if l.abs() < DISCHARGE_FLOOR && r.abs() < DISCHARGE_FLOOR {
            if l != 0.0 || r != 0.0 {
                blip.add_raw(len, -l as f32, -r as f32);
            }
            self.clear();
            return true;
        }
        false
    }

    /// Whether a kill fade is in progress (the voice no longer counts against polyphony).
    pub fn is_dying(&self) -> bool {
        self.fade.active && self.fade.goal == FadeGoal::Kill
    }

    /// Whether the voice is in its release (key up, not dying).
    pub fn is_releasing(&self) -> bool {
        self.active && !self.gate && !self.is_dying()
    }

    /// Loudness estimate for voice stealing: the largest envelope level of the enabled
    /// channels.
    pub fn loudness(&self, on: &[bool; 4]) -> f32 {
        let mut m = 0.0f32;
        for (e, enabled) in self.env.iter().zip(on.iter()) {
            if *enabled {
                m = m.max(e.level());
            }
        }
        m * self.fade.gain
    }

    /// Remaining fade steps (for choosing which dying voice to recycle).
    pub fn fade_left(&self) -> u32 {
        if self.fade.active {
            self.fade.left
        } else {
            u32::MAX
        }
    }

    /// Current pitch of the note including glide (for legato glides).
    pub fn current_pitch(&self) -> f64 {
        let base = if self.kind == Kind::HeldArp { self.held_note as f64 } else { self.note as f64 };
        base + glide_offset(self.glide_from, self.glide_t, self.glide_time, self.glide_curve)
    }

    /// Envelope levels (telemetry).
    pub fn env_levels(&self) -> [f32; 4] {
        [self.env[0].level(), self.env[1].level(), self.env[2].level(), self.env[3].level()]
    }

    /// Noise register of the first sub-voice (telemetry).
    pub fn lfsr(&self) -> u16 {
        self.sub[0].noise.lfsr()
    }

    /// Wave step of the first sub-voice (telemetry).
    pub fn wave_step(&self) -> u8 {
        self.sub[0].wave.step()
    }

    /// Pitch modulation offset at the last update (telemetry).
    pub fn pitch_offset(&self) -> f32 {
        self.tele_pitch
    }

    /// Arp step at the last update (telemetry).
    pub fn arp_step(&self) -> Option<u8> {
        self.tele_arp
    }

    /// Number of unison sub-voices.
    pub fn sub_count(&self) -> usize {
        self.n_sub
    }

    /// Sub-voice access (tests, diagnostics).
    pub fn sub(&self, i: usize) -> &SubVoice {
        &self.sub[i.min(MAX_UNISON - 1)]
    }

    /// Advance envelopes and modulation clocks to `t`.
    fn advance_to(&mut self, t: f64, res: &Resolved, fs: f64) {
        let dt = t - self.t_upd;
        if dt > 0.0 && dt.is_finite() {
            for (e, p) in self.env.iter_mut().zip(res.env.iter()) {
                e.advance(dt, p);
            }
            self.age += dt;
            self.glide_t += dt;
            let ph = self.vib_phase + res.vib_rate * dt / fs;
            self.vib_phase = ph - ph.floor();
        }
        self.t_upd = t;
    }

    fn set_glide(&mut self, from: Option<f64>, res: &Resolved) {
        let target = if self.kind == Kind::HeldArp { self.held_note as f64 } else { self.note as f64 };
        match from {
            Some(p) if p.is_finite() && (p - target).abs() > 1e-9 => {
                self.glide_from = (p - target).clamp(-127.0, 127.0);
                self.glide_t = 0.0;
                self.glide_time = res.glide_time;
                self.glide_curve = res.glide_curve;
            }
            _ => {
                self.glide_from = 0.0;
                self.glide_t = 0.0;
            }
        }
    }

    /// Start a fresh note on a free voice at chunk time 0.
    pub fn start(&mut self, a: &StartArgs, ctx: &Ctx<'_>, blip: &mut BlipBuffer) {
        if self.discharging {
            // Only reused while discharging when every slot is busy.
            self.force_clean(blip);
        }
        if self.cfg != ctx.cfg {
            // Clean slot (nothing held): just adopt the current mode.
            for s in &mut self.sub {
                s.clear();
            }
            self.cfg = ctx.cfg;
        }
        self.active = true;
        self.t_amp = NEVER;
        self.kind = a.kind;
        self.note = a.note.min(127);
        self.held_note = a.note.min(127) as i16;
        self.gate = true;
        self.velocity = a.velocity;
        self.order = a.order;
        self.fade = Fade::idle();
        for e in &mut self.env {
            e.reset();
            e.trigger();
        }
        self.t_upd = 0.0;
        self.age = 0.0;
        self.vib_phase = 0.0;
        self.set_glide(a.glide_from, ctx.res);
        self.arp_c0 = ctx.arp_count;
        self.arp_seed = a.seed;
        self.n_sub = ctx.res.uni_voices.clamp(1, MAX_UNISON);
        for s in &mut self.sub[..self.n_sub] {
            s.fade = 1.0;
        }
        let mut rng = Rng::new(a.seed);
        let nes = self.cfg.chip == ChipMode::Nes;
        let tri_rest = WaveOsc::triangle_rest_step(self.cfg.unipolar()) as f64;
        let mut resets = [Reset::default(); MAX_UNISON];
        for (i, r) in resets.iter_mut().enumerate() {
            if i == 0 {
                r.pulse_phase = Some([0.0, 0.0]);
                // The NES triangle starts at its rest step (no DAC jump).
                r.wave_pos = Some(if nes { tri_rest } else { 0.0 });
                r.lfsr = Some(NoiseOsc::INITIAL);
            } else {
                r.pulse_phase = Some([rng.next_f64(), rng.next_f64()]);
                // The triangle has no volume control: a random start would click, so unison
                // layers start together and drift apart by their detune.
                r.wave_pos = Some(if nes { tri_rest } else { rng.next_f64() * 32.0 });
                r.lfsr = Some((rng.next_u64() as u16 & 0x7fff).max(1));
            }
        }
        self.update(ctx, 0.0, a.ahead, Some(&resets), blip);
    }

    /// Retrigger a sounding voice with a new note (MONO, or the same note again in POLY):
    /// envelopes attack from their current level, sweep/vibrato/arp restart, the noise
    /// register reloads; oscillator phases continue (no click).
    pub fn retrigger(&mut self, a: &StartArgs, ctx: &Ctx<'_>, blip: &mut BlipBuffer) {
        self.advance_to(0.0, ctx.res, ctx.fs);
        self.kind = a.kind;
        self.note = a.note.min(127);
        self.held_note = a.note.min(127) as i16;
        self.gate = true;
        self.velocity = a.velocity;
        self.order = a.order;
        for e in &mut self.env {
            e.trigger();
        }
        self.age = 0.0;
        self.vib_phase = 0.0;
        self.set_glide(a.glide_from, ctx.res);
        self.arp_c0 = ctx.arp_count;
        self.arp_seed = a.seed;
        if self.fade.active && self.fade.goal == FadeGoal::Kill {
            // Revive a dying voice: fade back in.
            self.fade_to(1.0, FadeGoal::In, ctx.fs, 0.0);
        }
        let mut rng = Rng::new(a.seed);
        let mut resets = [Reset::default(); MAX_UNISON];
        for (i, r) in resets.iter_mut().enumerate() {
            r.lfsr = Some(if i == 0 { NoiseOsc::INITIAL } else { (rng.next_u64() as u16 & 0x7fff).max(1) });
        }
        self.update(ctx, 0.0, a.ahead, Some(&resets), blip);
    }

    /// Legato note change: pitch only (and glide), no envelope or modulation restart.
    pub fn legato(
        &mut self,
        note: u8,
        order: u64,
        glide_from: Option<f64>,
        ahead: f64,
        ctx: &Ctx<'_>,
        blip: &mut BlipBuffer,
    ) {
        self.advance_to(0.0, ctx.res, ctx.fs);
        self.note = note.min(127);
        if self.kind == Kind::Normal {
            self.held_note = self.note as i16;
        }
        self.order = order;
        self.gate = true;
        self.set_glide(glide_from, ctx.res);
        self.update(ctx, 0.0, ahead, None, blip);
    }

    /// Re-evaluate pitch (HELD arp sequence changes) at chunk time 0.
    pub fn refresh(&mut self, ahead: f64, ctx: &Ctx<'_>, blip: &mut BlipBuffer) {
        self.update(ctx, 0.0, ahead, None, blip);
    }

    /// Key released (after the sustain pedal): all envelopes enter their release.
    pub fn release(&mut self, res: &Resolved, fs: f64) {
        self.advance_to(0.0, res, fs);
        self.gate = false;
        for (e, p) in self.env.iter_mut().zip(res.env.iter()) {
            e.release(p);
        }
    }

    /// Start a fade towards `target` gain over [`FADE_MS`] beginning at chunk time `t`.
    pub fn fade_to(&mut self, target: f32, goal: FadeGoal, fs: f64, t: f64) {
        let interval = (FADE_MS * fs / 1000.0 / FADE_STEPS as f64).max(1.0);
        let gain = if self.fade.active { self.fade.gain } else { self.sub[0].fade };
        self.fade = Fade {
            active: true,
            goal,
            gain,
            step: (target - gain) / FADE_STEPS as f32,
            left: FADE_STEPS,
            interval,
            next_t: t + interval,
        };
    }

    /// Steal / all-sound-off: fade out over 2 ms and free.
    pub fn kill(&mut self, fs: f64) {
        if !self.is_dying() {
            self.gate = false;
            self.fade_to(0.0, FadeGoal::Kill, fs, 0.0);
        }
    }

    /// Chip configuration changed: fade out, switch, fade back in (no clicks or glitches).
    pub fn begin_switch(&mut self, fs: f64) {
        if !self.is_dying() {
            self.fade_to(0.0, FadeGoal::Switch, fs, 0.0);
        }
    }

    /// Resync the wave channel after the wave RAM changed.
    pub fn resync_wave(&mut self, ctx: &Ctx<'_>, blip: &mut BlipBuffer) {
        if self.cfg.chip == ChipMode::Nes {
            return;
        }
        for s in &mut self.sub[..self.n_sub] {
            let (ol, or) = s.total();
            s.wave.resync(0.0, ctx.wave);
            let (nl, nr) = s.total();
            emit(blip, 0.0, nl - ol, nr - or);
            s.out_l = nl;
            s.out_r = nr;
        }
    }

    fn fade_step(&mut self, ctx: &Ctx<'_>, blip: &mut BlipBuffer) -> VoiceEnd {
        let t = self.fade.next_t;
        self.fade.left = self.fade.left.saturating_sub(1);
        self.fade.gain = if self.fade.left == 0 {
            match self.fade.goal {
                FadeGoal::In => 1.0,
                _ => 0.0,
            }
        } else {
            (self.fade.gain + self.fade.step).clamp(0.0, 1.0)
        };
        self.fade.next_t += self.fade.interval;
        let g = self.fade.gain;
        for s in &mut self.sub[..self.n_sub] {
            s.set_fade(g, t, blip);
        }
        if self.fade.left > 0 {
            return VoiceEnd::Running;
        }
        match self.fade.goal {
            FadeGoal::Kill => {
                self.clear();
                VoiceEnd::Killed
            }
            FadeGoal::In => {
                self.fade = Fade::idle();
                VoiceEnd::Running
            }
            FadeGoal::Switch => {
                // Silent now: adopt the new configuration and come back.
                self.advance_to(t, ctx.res, ctx.fs);
                let before = self.cfg;
                self.cfg = ctx.cfg;
                let nes_after = self.cfg.chip == ChipMode::Nes;
                if before.chip != self.cfg.chip || before.unipolar() != self.cfg.unipolar() {
                    // Silent at this point: restart the wave sequencer where the new mode
                    // expects it (the triangle at its rest step for the DAC polarity).
                    let step = if nes_after { WaveOsc::triangle_rest_step(self.cfg.unipolar()) } else { 0 };
                    for s in &mut self.sub[..self.n_sub] {
                        s.wave.park(step);
                        s.wave.set_position(step as f64, t);
                    }
                }
                self.update(ctx, t, 0.0, None, blip);
                self.fade_to(1.0, FadeGoal::In, ctx.fs, t);
                VoiceEnd::Running
            }
        }
    }

    /// Evaluate modulation at `at` (sampled `ahead` samples later) and apply it to all
    /// sub-voices. Returns whether all enabled channels' envelopes have finished.
    fn update(
        &mut self,
        ctx: &Ctx<'_>,
        at: f64,
        ahead: f64,
        resets: Option<&[Reset; MAX_UNISON]>,
        blip: &mut BlipBuffer,
    ) -> bool {
        let res = ctx.res;
        let fs = ctx.fs;
        self.advance_to(at, res, fs);
        let ahead = if ahead.is_finite() { ahead.max(0.0) } else { 0.0 };
        // FREE: fast envelope segments get amplitude-only micro-updates; sample the
        // amplitude at the centre of the coming micro-step so all updates agree.
        let micro = self.micro_interval(res);
        let amp_ahead = if self.cfg.chip.is_chip() { ahead } else { ahead.min(0.5 * micro) };
        self.t_amp = if micro < FREE_CONTROL { at + micro } else { NEVER };
        let env_s: [f32; 4] = std::array::from_fn(|c| self.env[c].peek(amp_ahead, &res.env[c]));
        let tt = self.age + ahead;

        let sweep = if res.sweep_on {
            sweep_offset(tt, res.sweep_amt, res.sweep_time, res.sweep_dir, res.sweep_curve)
        } else {
            0.0
        };
        let perf = ctx.perf;
        let mut depth = 50.0 * (perf.mod_wheel.clamp(0.0, 1.0) + perf.aftertouch.clamp(0.0, 1.0)) as f64;
        if res.vib_on {
            depth += res.vib_depth * vibrato_fade(tt, res.vib_delay, res.vib_fade);
        }
        let vib_cents =
            if depth > 0.0 { depth * lfo(self.vib_phase + res.vib_rate * ahead / fs, res.vib_shape) } else { 0.0 };
        let glide = glide_offset(self.glide_from, self.glide_t + ahead, self.glide_time, self.glide_curve);
        let bend = perf.bend.clamp(-1.0, 1.0) as f64 * res.pb_range;
        let (base_note, arp_off, arp_step) = self.arp(ctx);
        let tune = ctx.sm[sm::TUNE] as f64 / 100.0;
        let base = base_note + arp_off + glide + sweep + vib_cents / 100.0 + bend + tune;
        self.tele_pitch = (sweep + vib_cents / 100.0 + glide + bend + arp_off + base_note - self.note as f64) as f32;
        self.tele_arp = arp_step;

        let vel_gain = 1.0 - res.vel_sens + res.vel_sens * self.velocity;
        let plan = Plan {
            pitch: std::array::from_fn(|c| {
                (base + res.coarse[c] + ctx.sm[sm::FINE + c] as f64 / 100.0).clamp(-60.0, 200.0)
            }),
            amp: std::array::from_fn(|c| {
                ((ctx.sm[sm::LEVEL + c] / 15.0).clamp(0.0, 1.0) * env_s[c] * vel_gain).clamp(0.0, 1.0)
            }),
            tri_gate: res.on[2] && self.env[2].peek(ahead, &res.env[2]) * 15.0 >= 0.5,
            noise_mod: sweep + vib_cents / 100.0 + bend,
            uni_gain: 1.0 / (self.n_sub as f32).sqrt(),
            cfg: self.cfg,
        };
        let n = self.n_sub;
        for (i, s) in self.sub[..n].iter_mut().enumerate() {
            let lin = if n <= 1 { 0.0 } else { -1.0 + 2.0 * i as f64 / (n - 1) as f64 };
            s.apply(&plan, lin, ctx, at, resets.map(|r| &r[i]), blip);
        }
        (0..4).all(|c| !res.on[c] || self.env[c].is_idle())
    }

    /// FREE mode: samples between amplitude updates that keep every envelope step below
    /// [`MICRO_STEP`] of full scale (≥ [`FREE_CONTROL`] means the regular ticks suffice).
    fn micro_interval(&self, res: &Resolved) -> f64 {
        if self.cfg.chip.is_chip() {
            return NEVER;
        }
        let mut rate = 0.0f64;
        for c in 0..4 {
            if res.on[c] {
                rate = rate.max(self.env[c].rate(&res.env[c]));
            }
        }
        if rate * FREE_CONTROL <= MICRO_STEP {
            NEVER
        } else {
            (MICRO_STEP / rate).clamp(1.0, FREE_CONTROL)
        }
    }

    /// FREE mode amplitude-only update at `at` (between regular ticks during fast envelope
    /// segments).
    fn amp_update(&mut self, ctx: &Ctx<'_>, at: f64, blip: &mut BlipBuffer) {
        let res = ctx.res;
        self.advance_to(at, res, ctx.fs);
        let micro = self.micro_interval(res);
        let amp_ahead = 0.5 * micro.min(FREE_CONTROL);
        self.t_amp = if micro < FREE_CONTROL { at + micro } else { NEVER };
        let vel_gain = 1.0 - res.vel_sens + res.vel_sens * self.velocity;
        let amp: [f32; 4] = std::array::from_fn(|c| {
            let e = self.env[c].peek(amp_ahead, &res.env[c]);
            ((ctx.sm[sm::LEVEL + c] / 15.0).clamp(0.0, 1.0) * e * vel_gain).clamp(0.0, 1.0)
        });
        for s in &mut self.sub[..self.n_sub] {
            s.set_amps(&amp, at, blip);
        }
    }

    /// (note, arp offset, arp step) for the coming step.
    fn arp(&mut self, ctx: &Ctx<'_>) -> (f64, f64, Option<u8>) {
        let res = ctx.res;
        let step = ctx.arp_count.wrapping_sub(self.arp_c0);
        match self.kind {
            Kind::Normal => {
                if res.arp_on && res.arp_chord != ArpChord::Held {
                    let iv = res.arp_chord.intervals();
                    let mut base = [0i16; 4];
                    for (b, i) in base.iter_mut().zip(iv.iter()) {
                        *b = *i as i16;
                    }
                    let len = iv.len().clamp(1, 4);
                    let k = arp_index(res.arp_mode, step, len * res.arp_oct, self.arp_seed);
                    (self.note as f64, arp_note(&base[..len], k) as f64, Some(k.min(255) as u8))
                } else {
                    (self.note as f64, 0.0, None)
                }
            }
            Kind::HeldArp => {
                if res.held_arp() && !ctx.held_sorted.is_empty() {
                    let list = if res.arp_mode == ArpMode::Order { ctx.held_order } else { ctx.held_sorted };
                    let mut base = [0i16; 128];
                    let len = list.len().min(128);
                    for (b, n) in base.iter_mut().zip(list.iter()) {
                        *b = *n as i16;
                    }
                    let k = arp_index(res.arp_mode, step, len * res.arp_oct, self.arp_seed);
                    self.held_note = arp_note(&base[..len], k);
                    (self.held_note as f64, 0.0, Some(k.min(255) as u8))
                } else {
                    if let Some(last) = ctx.held_order.last() {
                        self.held_note = *last as i16;
                    }
                    (self.held_note as f64, 0.0, None)
                }
            }
        }
    }

    /// Render this voice over `[0, end)` of the chunk, handling ticks, fades and FREE-mode
    /// amplitude micro-updates.
    pub fn render(&mut self, base: &Ctx<'_>, ticks: &[Tick], end: f64, blip: &mut BlipBuffer) -> VoiceEnd {
        let mut ctx = *base;
        let mut ti = 0;
        loop {
            let t_tick = ticks.get(ti).map_or(NEVER, |t| t.t);
            let t_fade = if self.fade.active { self.fade.next_t } else { NEVER };
            let t_amp = self.t_amp;
            let t_ev = t_tick.min(t_fade).min(t_amp);
            let seg_end = t_ev.min(end);
            let tab = if self.cfg.chip == ChipMode::Nes { ctx.tri } else { ctx.wave };
            for s in &mut self.sub[..self.n_sub] {
                s.run(seg_end, tab, blip);
            }
            if t_ev >= end {
                return VoiceEnd::Running;
            }
            if t_fade <= t_tick && t_fade <= t_amp {
                let r = self.fade_step(&ctx, blip);
                if r == VoiceEnd::Killed {
                    return r;
                }
            } else if t_amp < t_tick {
                self.amp_update(&ctx, t_amp, blip);
            } else {
                let tk = &ticks[ti];
                ti += 1;
                ctx = Ctx { sm: &tk.sm, arp_count: tk.arp_count, ..*base };
                if self.update(&ctx, tk.t, tk.ahead, None, blip) {
                    self.finish();
                    return VoiceEnd::Finished;
                }
            }
        }
    }

    /// All enabled envelopes are done: free the voice; a level left behind (halted NES
    /// triangle) is discharged smoothly from the next chunk on.
    fn finish(&mut self) {
        self.active = false;
        self.gate = false;
        self.fade = Fade::idle();
        self.t_amp = NEVER;
        let mut any = false;
        for (d, s) in self.dis.iter_mut().zip(self.sub.iter()) {
            let (l, r) = s.total();
            *d = (l as f64, r as f64);
            any |= l.abs() > 1e-9 || r.abs() > 1e-9;
        }
        if any {
            self.discharging = true;
        } else {
            // Nothing held: the bookkeeping is (numerically) zero already.
            self.dis = [(0.0, 0.0); MAX_UNISON];
        }
    }

    /// Shift the time origin by `dt` samples (end of a chunk).
    pub fn shift_time(&mut self, dt: f64) {
        self.t_upd -= dt;
        if self.fade.active {
            self.fade.next_t -= dt;
        }
        if self.t_amp.is_finite() {
            self.t_amp -= dt;
        }
        for s in &mut self.sub {
            s.shift_time(dt);
        }
    }
}
