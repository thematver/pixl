//! Shared helpers for the end-to-end tests: a small "host" that drives the real
//! [`Processor`] (production engine and effect chain) block by block with timed events,
//! plus signal measurements.

#![allow(dead_code)] // Each test binary uses a different subset.

use nih_plug::prelude::NoteEvent;
use pixl::midi::{map_note_event, TimedEvent};
use pixl::processor::{BlockInput, BlockStatus, Processor};
use pixl_core::preset::PresetState;
use pixl_core::sync::{AtomicMacros, AtomicWave};
use pixl_core::{ParamId, ParamValues, TelemetrySnapshot};
use pixl_dsp::{EngineEvent, Transport};

/// Output ceiling of the safety clipper (−0.01 dBFS).
pub const CEILING: f32 = 0.998_849_5;

/// A performance event at an absolute sample position.
#[derive(Clone, Copy, Debug)]
pub struct At(pub usize, pub EngineEvent);

pub fn on(at: usize, note: u8, velocity: f32) -> At {
    At(at, EngineEvent::NoteOn { note, velocity })
}

pub fn off(at: usize, note: u8) -> At {
    At(at, EngineEvent::NoteOff { note })
}

/// Map a nih-plug MIDI event at `at` through the plugin's MIDI mapping.
pub fn midi(at: usize, event: NoteEvent<()>) -> Vec<At> {
    map_note_event(&event).as_slice().iter().map(|e| At(at, *e)).collect()
}

/// Seconds → samples.
pub fn secs(sr: f32, s: f32) -> usize {
    (s * sr).round() as usize
}

/// A plugin instance as the host sees it: processor + shared state + parameter values.
pub struct Session {
    pub processor: Processor,
    pub wave: AtomicWave,
    pub macros: AtomicMacros,
    pub values: ParamValues,
    pub transport: Transport,
    pub sr: f32,
}

/// The rendered output of [`Session::render`].
pub struct Render {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Status and telemetry after each host block, with the block's end position.
    pub blocks: Vec<(usize, BlockStatus, TelemetrySnapshot)>,
}

impl Render {
    pub fn mono(&self) -> Vec<f32> {
        self.left.iter().zip(&self.right).map(|(l, r)| 0.5 * (l + r)).collect()
    }

    pub fn peak(&self) -> f32 {
        peak(&self.left).max(peak(&self.right))
    }

    pub fn all_finite(&self) -> bool {
        self.left.iter().chain(&self.right).all(|x| x.is_finite())
    }

    /// Telemetry after the host block containing sample `at`.
    pub fn telemetry_at(&self, at: usize) -> TelemetrySnapshot {
        self.blocks.iter().find(|b| b.0 > at).or(self.blocks.last()).map(|b| b.2).unwrap_or_default()
    }

    /// Status after the host block containing sample `at`.
    pub fn status_at(&self, at: usize) -> BlockStatus {
        self.blocks.iter().find(|b| b.0 > at).or(self.blocks.last()).map(|b| b.1).unwrap_or(BlockStatus::Silent)
    }
}

impl Session {
    pub fn new(state: &PresetState, sr: f32) -> Self {
        let mut processor = Processor::new();
        processor.prepare(sr);
        Self {
            processor,
            wave: AtomicWave::new(state.wave),
            macros: AtomicMacros::new(&state.macros),
            values: state.values,
            transport: Transport::default(),
            sr,
        }
    }

    /// Render `total` samples in host blocks of `block` samples, delivering `events` at their
    /// absolute positions (sorted by the caller or not; they are stably sorted here).
    pub fn render(&mut self, events: &[At], total: usize, block: usize) -> Render {
        let mut events = events.to_vec();
        events.sort_by_key(|e| e.0);
        let (mut left, mut right) = (vec![0.0f32; total], vec![0.0f32; total]);
        let mut blocks = Vec::new();
        let mut pos = 0;
        let mut next = 0;
        while pos < total {
            let end = (pos + block.max(1)).min(total);
            let mut timed = Vec::new();
            while next < events.len() && events[next].0 < end {
                let At(at, event) = events[next];
                timed.push(TimedEvent { offset: at.saturating_sub(pos) as u32, event });
                next += 1;
            }
            let input =
                BlockInput { params: &self.values, wave: &self.wave, macros: &self.macros, transport: self.transport };
            let status = self.processor.process(&input, timed, &mut left[pos..end], &mut right[pos..end]);
            blocks.push((end, status, *self.processor.telemetry()));
            pos = end;
        }
        Render { left, right, blocks }
    }
}

pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt() as f32
}

pub fn db(x: f32) -> f32 {
    20.0 * x.max(1e-12).log10()
}

/// Fundamental frequency of `x` near `guess` (within −20 %..+25 %).
///
/// Coarse estimate: normalized autocorrelation maximum with parabolic refinement (robust
/// against harmonics, detuned unison and slow amplitude changes, but biased by up to a few
/// cents on waveforms with sharp autocorrelation peaks). Fine estimate: the phase advance of
/// the fundamental between the two halves of the window (Hann-windowed DFT at the coarse
/// frequency), accurate to well below 0.1 cent on steady tones.
pub fn pitch(x: &[f32], sr: f32, guess: f32) -> f32 {
    let coarse = acf_pitch(x, sr, guess);
    let half = x.len() / 2;
    let phase = |seg: &[f32]| -> f64 {
        let n = seg.len() as f64;
        let w = std::f64::consts::TAU * coarse as f64 / sr as f64;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, v) in seg.iter().enumerate() {
            let hann = 0.5 - 0.5 * (std::f64::consts::TAU * (i as f64 + 0.5) / n).cos();
            let a = w * i as f64;
            re += hann * *v as f64 * a.cos();
            im -= hann * *v as f64 * a.sin();
        }
        im.atan2(re)
    };
    let (pa, pb) = (phase(&x[..half]), phase(&x[half..2 * half]));
    let expected = std::f64::consts::TAU * coarse as f64 * half as f64 / sr as f64;
    let mut d = pb - pa - expected;
    d = (d + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU) - std::f64::consts::PI;
    (coarse as f64 + d * sr as f64 / (std::f64::consts::TAU * half as f64)) as f32
}

/// Coarse fundamental estimate (autocorrelation only): a few cents of bias, but robust for
/// detuned unison and vibrato, where the phase refinement of [`pitch`] is not meaningful.
pub fn acf_pitch(x: &[f32], sr: f32, guess: f32) -> f32 {
    let lo = (sr / (guess * 1.25)).floor().max(2.0) as usize;
    let hi = (sr / (guess * 0.8)).ceil() as usize;
    assert!(x.len() > 2 * hi, "window too short for {guess} Hz");
    let n = x.len() - hi - 1;
    let acf = |lag: usize| -> f64 {
        let (mut num, mut e0, mut e1) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..n {
            let (a, b) = (x[i] as f64, x[i + lag] as f64);
            num += a * b;
            e0 += a * a;
            e1 += b * b;
        }
        if e0 <= 0.0 || e1 <= 0.0 {
            0.0
        } else {
            num / (e0 * e1).sqrt()
        }
    };
    let values: Vec<f64> = (lo - 1..=hi + 1).map(acf).collect();
    let (mut best, mut best_v) = (1, f64::MIN);
    for (i, v) in values.iter().enumerate().take(values.len() - 1).skip(1) {
        if *v > best_v {
            best_v = *v;
            best = i;
        }
    }
    let (a, b, c) = (values[best - 1], values[best], values[best + 1]);
    let denom = a - 2.0 * b + c;
    let shift = if denom.abs() > 1e-12 { 0.5 * (a - c) / denom } else { 0.0 };
    let lag = (lo - 1 + best) as f64 + shift.clamp(-0.5, 0.5);
    (sr as f64 / lag) as f32
}

/// Equal-tempered frequency of a MIDI note (A4 = 440 Hz).
pub fn note_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// Deviation of `f` from `reference` in cents.
pub fn cents(f: f32, reference: f32) -> f32 {
    1200.0 * (f / reference).log2()
}

/// A clean single-oscillator patch for pitch measurements: one channel, no pitch effects,
/// no unison, no echo/crush.
pub fn clean_patch(chip: f32, channel: pixl_core::Channel) -> PresetState {
    use pixl_core::{Channel, ChannelField};
    let mut v = ParamValues::defaults();
    v.set(ParamId::Chip, chip);
    for ch in Channel::ALL {
        v.set(ParamId::channel(*ch, ChannelField::On), if *ch == channel { 1.0 } else { 0.0 });
        v.set(ParamId::channel(*ch, ChannelField::Level), 15.0);
        v.set(ParamId::channel(*ch, ChannelField::Sustain), 15.0);
    }
    v.set(ParamId::P1Duty, 50.0);
    v.set(ParamId::P2Duty, 50.0);
    for id in [ParamId::SweepOn, ParamId::VibOn, ParamId::GlideOn, ParamId::UniOn, ParamId::ArpOn, ParamId::EchoOn] {
        v.set(id, 0.0);
    }
    v.set(ParamId::CrushOn, 0.0);
    v.set(ParamId::VelSens, 0.0);
    PresetState { values: v, wave: pixl_core::wave::default_wave(), macros: pixl_core::MacroAssignments::empty() }
}

/// Deterministic xorshift PRNG for fuzzing.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform in 0..1.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
}
