//! Parameter resolution and smoothing (SPEC §4.8).
//!
//! [`Resolved`] is a typed, sanitized view of the effective parameter values with times
//! converted to samples. It is recomputed whenever `set_params` is called (every ≤ 64
//! samples) and costs a few hundred operations.
//!
//! Continuous parameters that would zipper (channel levels and enables, pans, fine tunes,
//! master tune, FREE duty, unison detune and width) go through [`Smoother`]: one-pole, 10 ms,
//! advanced exactly by elapsed time at every modulation tick. They are global (the same for
//! every voice), so one smoother set serves all sub-voices.

use crate::env::EnvParams;
use pixl_core::{
    ArpChord, ArpDiv, ArpMode, Channel, ChannelField, ChipMode, Curve, FrameRate, GlideMode, NoiseMode, ParamId,
    ParamValues, SweepDir, VibShape, VoiceMode,
};

/// The four chip channels in parameter order.
const CHANNELS: [Channel; 4] = [Channel::P1, Channel::P2, Channel::Wave, Channel::Noise];

/// Smoothing time constant (ms).
pub const SMOOTH_MS: f64 = 10.0;

/// Indices into the smoothed value array.
pub mod sm {
    /// Channel level × enable (0..15), per channel.
    pub const LEVEL: usize = 0;
    /// Channel pan (−100..100), per channel.
    pub const PAN: usize = 4;
    /// Channel fine tune (cents), per channel.
    pub const FINE: usize = 8;
    /// Master tune (cents).
    pub const TUNE: usize = 12;
    /// Pulse duty (percent), pulse 1 and 2.
    pub const DUTY: usize = 13;
    /// Unison detune (cents).
    pub const DETUNE: usize = 15;
    /// Unison width (percent).
    pub const WIDTH: usize = 16;
    /// Number of smoothed values.
    pub const COUNT: usize = 17;
}

/// Snapshot of all smoothed values.
pub type Smoothed = [f32; sm::COUNT];

/// Typed, sanitized parameters (times in samples).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolved {
    /// Emulated chip.
    pub chip: ChipMode,
    /// Hardware DAC behaviour (unipolar levels, 2A03 mixer) in chip modes.
    pub analog: bool,
    /// POLY / MONO / LEGATO.
    pub voice_mode: VoiceMode,
    /// Maximum sounding voices in POLY (1..16).
    pub polyphony: usize,
    /// NTSC / PAL (NES CPU clock and frame rate).
    pub frame_rate: FrameRate,
    /// Pitch-bend range in semitones.
    pub pb_range: f64,
    /// Velocity sensitivity 0..1.
    pub vel_sens: f32,
    /// Channel enables (P1, P2, wave, noise).
    pub on: [bool; 4],
    /// Channel coarse tune in semitones.
    pub coarse: [f64; 4],
    /// Channel envelopes (times in samples).
    pub env: [EnvParams; 4],
    /// Noise short (looped) mode.
    pub ns_short: bool,
    /// Noise period index 0..15.
    pub ns_period: f64,
    /// Noise key tracking.
    pub ns_track: bool,
    /// Sweep enabled.
    pub sweep_on: bool,
    /// Sweep amount in semitones.
    pub sweep_amt: f64,
    /// Sweep direction.
    pub sweep_dir: SweepDir,
    /// Sweep time in samples.
    pub sweep_time: f64,
    /// Sweep curve.
    pub sweep_curve: Curve,
    /// Vibrato enabled.
    pub vib_on: bool,
    /// Vibrato rate in Hz.
    pub vib_rate: f64,
    /// Vibrato depth in cents.
    pub vib_depth: f64,
    /// Vibrato delay in samples.
    pub vib_delay: f64,
    /// Vibrato fade-in in samples.
    pub vib_fade: f64,
    /// Vibrato LFO shape.
    pub vib_shape: VibShape,
    /// Arpeggiator enabled.
    pub arp_on: bool,
    /// Arpeggiator order.
    pub arp_mode: ArpMode,
    /// Free arp rate in Hz.
    pub arp_rate: f64,
    /// Tempo-synced arp.
    pub arp_sync: bool,
    /// Synced arp step length.
    pub arp_div: ArpDiv,
    /// Octaves the arp sequence spans (1..4).
    pub arp_oct: usize,
    /// Arp chord (or HELD notes).
    pub arp_chord: ArpChord,
    /// Glide enabled.
    pub glide_on: bool,
    /// Glide time in samples.
    pub glide_time: f64,
    /// When glide applies.
    pub glide_mode: GlideMode,
    /// Glide curve.
    pub glide_curve: Curve,
    /// Unison sub-voices for new notes (1 when unison is off).
    pub uni_voices: usize,
}

impl Resolved {
    /// Resolve sanitized parameter values at a sample rate.
    pub fn new(p: &ParamValues, fs: f64) -> Self {
        let ms = |id: ParamId| p.get(id) as f64 * fs / 1000.0;
        let env = |ch: Channel| {
            EnvParams::from_ms(
                p.channel(ch, ChannelField::Attack),
                p.channel(ch, ChannelField::Decay),
                p.channel(ch, ChannelField::Sustain),
                p.channel(ch, ChannelField::Release),
                fs,
            )
        };
        let uni_on = p.bool(ParamId::UniOn);
        Self {
            chip: p.chip(),
            analog: p.bool(ParamId::Analog),
            voice_mode: p.voice_mode(),
            polyphony: p.int(ParamId::Polyphony).clamp(1, 16) as usize,
            frame_rate: p.frame_rate(),
            pb_range: p.get(ParamId::PbRange) as f64,
            vel_sens: p.get(ParamId::VelSens) / 100.0,
            on: CHANNELS.map(|c| p.channel(c, ChannelField::On) >= 0.5),
            coarse: CHANNELS.map(|c| p.channel(c, ChannelField::Coarse).round() as f64),
            env: CHANNELS.map(env),
            ns_short: p.noise_mode() == NoiseMode::Short,
            ns_period: p.get(ParamId::NsPeriod) as f64,
            ns_track: p.bool(ParamId::NsTrack),
            sweep_on: p.bool(ParamId::SweepOn),
            sweep_amt: p.get(ParamId::SweepAmt) as f64,
            sweep_dir: p.sweep_dir(),
            sweep_time: ms(ParamId::SweepTime),
            sweep_curve: p.sweep_curve(),
            vib_on: p.bool(ParamId::VibOn),
            vib_rate: p.get(ParamId::VibRate) as f64,
            vib_depth: p.get(ParamId::VibDepth) as f64,
            vib_delay: ms(ParamId::VibDelay),
            vib_fade: ms(ParamId::VibFade),
            vib_shape: p.vib_shape(),
            arp_on: p.bool(ParamId::ArpOn),
            arp_mode: p.arp_mode(),
            arp_rate: p.get(ParamId::ArpRate) as f64,
            arp_sync: p.bool(ParamId::ArpSync),
            arp_div: p.arp_div(),
            arp_oct: p.int(ParamId::ArpOct).clamp(1, 4) as usize,
            arp_chord: p.arp_chord(),
            glide_on: p.bool(ParamId::GlideOn),
            glide_time: ms(ParamId::GlideTime),
            glide_mode: p.glide_mode(),
            glide_curve: p.glide_curve(),
            uni_voices: if uni_on { p.int(ParamId::UniVoices).clamp(1, 4) as usize } else { 1 },
        }
    }

    /// Whether the arpeggiator uses the held notes (one arp voice).
    pub fn held_arp(&self) -> bool {
        self.arp_on && self.arp_chord == ArpChord::Held
    }

    /// Frame rate of the chip clock (Hz): GB is fixed at 59.7275 Hz, NES (and the FREE-mode
    /// telemetry counter) follow the NTSC/PAL setting.
    pub fn frame_hz(&self) -> f64 {
        match self.chip {
            ChipMode::Gb => crate::chip::GB_FRAME_HZ,
            _ => self.frame_rate.hz(),
        }
    }

    /// NES CPU clock for the current frame-rate standard.
    pub fn nes_cpu(&self) -> f64 {
        crate::chip::nes_cpu(self.frame_rate == FrameRate::Pal)
    }
}

/// Smoothing targets from parameter values.
pub fn targets(p: &ParamValues) -> Smoothed {
    let mut t = [0.0f32; sm::COUNT];
    for (i, ch) in CHANNELS.iter().enumerate() {
        let on = p.channel(*ch, ChannelField::On) >= 0.5;
        t[sm::LEVEL + i] = if on { p.channel(*ch, ChannelField::Level) } else { 0.0 };
        t[sm::PAN + i] = p.channel(*ch, ChannelField::Pan);
        t[sm::FINE + i] = p.channel(*ch, ChannelField::Fine);
    }
    t[sm::TUNE] = p.get(ParamId::Tune);
    t[sm::DUTY] = p.get(ParamId::P1Duty);
    t[sm::DUTY + 1] = p.get(ParamId::P2Duty);
    t[sm::DETUNE] = p.get(ParamId::UniDetune);
    t[sm::WIDTH] = p.get(ParamId::UniWidth);
    t
}

/// One-pole smoothers for all [`Smoothed`] values, advanced by exact elapsed time.
#[derive(Clone, Copy, Debug)]
pub struct Smoother {
    cur: Smoothed,
    target: Smoothed,
    /// Time constant in samples.
    tau: f64,
    primed: bool,
}

impl Smoother {
    /// Smoothers with a [`SMOOTH_MS`] time constant at `fs`.
    pub fn new(fs: f64) -> Self {
        Self { cur: [0.0; sm::COUNT], target: [0.0; sm::COUNT], tau: SMOOTH_MS * fs / 1000.0, primed: false }
    }

    /// Set new targets. The first call after construction/reset jumps straight to them.
    pub fn set_targets(&mut self, t: &Smoothed) {
        self.target = *t;
        if !self.primed {
            self.cur = *t;
            self.primed = true;
        }
    }

    /// Jump to the targets (reset).
    pub fn snap(&mut self) {
        self.cur = self.target;
    }

    /// Forget the current values; the next `set_targets` jumps.
    pub fn unprime(&mut self) {
        self.primed = false;
    }

    /// Advance by `dt` samples.
    pub fn advance(&mut self, dt: f64) {
        if dt.is_nan() || dt <= 0.0 {
            return;
        }
        let c = (1.0 - (-dt / self.tau.max(1e-9)).exp()) as f32;
        for (cur, tgt) in self.cur.iter_mut().zip(self.target.iter()) {
            let d = *tgt - *cur;
            if d.abs() <= 1e-5 * (1.0 + tgt.abs()) {
                *cur = *tgt;
            } else {
                *cur += d * c;
            }
        }
    }

    /// Current smoothed values.
    pub fn current(&self) -> &Smoothed {
        &self.cur
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_defaults() {
        let p = ParamValues::defaults();
        let r = Resolved::new(&p, 48_000.0);
        assert_eq!(r.chip, ChipMode::Gb);
        assert_eq!(r.uni_voices, 3);
        assert_eq!(r.on, [false, false, true, false]);
        assert!((r.sweep_time - 0.18 * 48_000.0).abs() < 1e-6);
        assert!((r.glide_time - 0.09 * 48_000.0).abs() < 1e-6);
        assert!((r.env[2].sustain - 0.8).abs() < 1e-9);
        assert!((r.frame_hz() - 59.7275).abs() < 1e-9);
        let t = targets(&p);
        assert_eq!(t[sm::LEVEL], 0.0, "disabled channel smooths to silence");
        assert_eq!(t[sm::LEVEL + 2], 12.0);
    }

    #[test]
    fn smoother_time_constant() {
        let mut s = Smoother::new(48_000.0);
        let mut t = [0.0; sm::COUNT];
        s.set_targets(&t);
        t[0] = 1.0;
        s.set_targets(&t);
        s.advance(480.0); // one time constant
        assert!((s.current()[0] - (1.0 - (-1.0f32).exp())).abs() < 1e-5);
        s.advance(48_000.0);
        assert_eq!(s.current()[0], 1.0);
        s.advance(f64::NAN);
        assert_eq!(s.current()[0], 1.0);
    }
}
