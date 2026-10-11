//! Shared test rig: drives the engine exactly like the plugin does (set_params before every
//! sub-block of ≤ 64 samples, events at sub-block starts).
#![allow(dead_code)]

use pixl_core::{ChipMode, ParamId, ParamValues};
use pixl_dsp::{Engine, EngineEvent, Transport};

/// Sub-block length used by the plugin.
pub const SUB: usize = 64;

/// Engine + parameters.
pub struct Rig {
    pub e: Engine,
    pub p: ParamValues,
    pub fs: f32,
}

impl Rig {
    /// A prepared engine at `fs` with parameters `p` already applied.
    pub fn new(fs: f32, p: ParamValues) -> Self {
        let mut e = Engine::new(SUB);
        e.prepare(fs, SUB);
        e.set_params(&p);
        Self { e, p, fs }
    }

    /// Note on at the current position (applied with the current parameters, like the
    /// plugin's event-then-`set_params` order).
    pub fn on(&mut self, note: u8, velocity: f32) {
        self.ev(EngineEvent::NoteOn { note, velocity });
    }

    pub fn off(&mut self, note: u8) {
        self.ev(EngineEvent::NoteOff { note });
    }

    /// Any event at the current position; `set_params` applies it immediately.
    pub fn ev(&mut self, ev: EngineEvent) {
        self.e.event(ev);
        self.e.set_params(&self.p);
    }

    pub fn transport(&mut self, bpm: f64) {
        self.e.set_transport(Transport { tempo_bpm: bpm, playing: true, beat_pos: Some(0.0) });
    }

    /// Render `n` samples in ≤ 64-sample sub-blocks.
    pub fn render(&mut self, n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; n];
        let mut r = vec![0.0; n];
        self.render_into(&mut l, &mut r);
        (l, r)
    }

    pub fn render_into(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        let mut pos = 0;
        while pos < n {
            let m = (n - pos).min(SUB);
            self.e.set_params(&self.p);
            self.e.render(&mut l[pos..pos + m], &mut r[pos..pos + m]);
            pos += m;
        }
    }

    /// Render `n` samples and return the left channel only.
    pub fn left(&mut self, n: usize) -> Vec<f32> {
        self.render(n).0
    }

    pub fn set(&mut self, id: ParamId, v: f32) {
        self.p.set(id, v);
    }
}

/// Seconds → samples.
pub fn secs(fs: f32, s: f64) -> usize {
    (fs as f64 * s).round() as usize
}

pub const CHANNEL_ON: [ParamId; 4] = [ParamId::P1On, ParamId::P2On, ParamId::WvOn, ParamId::NsOn];
pub const CHANNEL_LEVEL: [ParamId; 4] = [ParamId::P1Level, ParamId::P2Level, ParamId::WvLevel, ParamId::NsLevel];
pub const CHANNEL_ATTACK: [ParamId; 4] = [ParamId::P1Attack, ParamId::P2Attack, ParamId::WvAttack, ParamId::NsAttack];
pub const CHANNEL_DECAY: [ParamId; 4] = [ParamId::P1Decay, ParamId::P2Decay, ParamId::WvDecay, ParamId::NsDecay];
pub const CHANNEL_SUSTAIN: [ParamId; 4] =
    [ParamId::P1Sustain, ParamId::P2Sustain, ParamId::WvSustain, ParamId::NsSustain];
pub const CHANNEL_RELEASE: [ParamId; 4] =
    [ParamId::P1Release, ParamId::P2Release, ParamId::WvRelease, ParamId::NsRelease];

/// A "clean" patch: only `channel` on at level 15 with full sustain, no pitch effects, no
/// unison, velocity-insensitive.
pub fn clean(chip: ChipMode, channel: usize) -> ParamValues {
    let mut p = ParamValues::defaults();
    p.set(ParamId::Chip, chip.index() as f32);
    for id in [ParamId::UniOn, ParamId::SweepOn, ParamId::VibOn, ParamId::GlideOn, ParamId::ArpOn, ParamId::VelSens] {
        p.set(id, 0.0);
    }
    for c in 0..4 {
        p.set(CHANNEL_ON[c], if c == channel { 1.0 } else { 0.0 });
        p.set(CHANNEL_LEVEL[c], 15.0);
        p.set(CHANNEL_SUSTAIN[c], 15.0);
        p.set(CHANNEL_ATTACK[c], 0.0);
    }
    p.set(ParamId::P1Duty, 50.0);
    p.set(ParamId::P2Duty, 50.0);
    p
}

/// MIDI note → Hz.
pub fn hz(note: f64) -> f64 {
    440.0 * ((note - 69.0) / 12.0).exp2()
}
