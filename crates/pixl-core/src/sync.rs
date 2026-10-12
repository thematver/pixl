//! Lock-free containers for audio ⇄ GUI state.
//!
//! All operations are wait-free and allocation-free, so they are safe on the audio thread.

use crate::bridge::TelemetrySnapshot;
use crate::macros::{MacroAssignments, MacroSlot, MACRO_COUNT, MACRO_SLOTS};
use crate::params::ParamId;
use crate::wave::{WaveRam, WAVE_LEN, WAVE_MAX};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

#[inline]
fn f32_to_bits(v: f32) -> u32 {
    v.to_bits()
}
#[inline]
fn bits_to_f32(b: u32) -> f32 {
    f32::from_bits(b)
}

/// Wave RAM shared between GUI (writer) and audio (reader).
pub struct AtomicWave {
    steps: [AtomicU8; WAVE_LEN],
    version: AtomicU32,
}

impl AtomicWave {
    pub fn new(wave: WaveRam) -> Self {
        Self { steps: std::array::from_fn(|i| AtomicU8::new(wave[i].min(WAVE_MAX))), version: AtomicU32::new(1) }
    }

    pub fn store(&self, wave: &WaveRam) {
        for (a, v) in self.steps.iter().zip(wave.iter()) {
            a.store((*v).min(WAVE_MAX), Ordering::Relaxed);
        }
        self.version.fetch_add(1, Ordering::Release);
    }

    pub fn load(&self) -> WaveRam {
        let _ = self.version.load(Ordering::Acquire);
        std::array::from_fn(|i| self.steps[i].load(Ordering::Relaxed))
    }

    /// Monotonic change counter; readers can skip work when unchanged.
    pub fn version(&self) -> u32 {
        self.version.load(Ordering::Acquire)
    }
}

/// Macro routings shared between GUI (writer) and audio (reader).
pub struct AtomicMacros {
    targets: [[AtomicU16; MACRO_SLOTS]; MACRO_COUNT],
    depths: [[AtomicU32; MACRO_SLOTS]; MACRO_COUNT],
    version: AtomicU32,
}

const NO_TARGET: u16 = u16::MAX;

impl AtomicMacros {
    pub fn new(a: &MacroAssignments) -> Self {
        let s = Self {
            targets: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU16::new(NO_TARGET))),
            depths: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU32::new(0))),
            version: AtomicU32::new(1),
        };
        s.store(a);
        s
    }

    pub fn store(&self, a: &MacroAssignments) {
        for m in 0..MACRO_COUNT {
            for k in 0..MACRO_SLOTS {
                let slot = a.slots[m][k];
                self.targets[m][k].store(slot.target.map_or(NO_TARGET, |t| t as u16), Ordering::Relaxed);
                self.depths[m][k].store(f32_to_bits(slot.depth), Ordering::Relaxed);
            }
        }
        self.version.fetch_add(1, Ordering::Release);
    }

    pub fn load(&self) -> MacroAssignments {
        let _ = self.version.load(Ordering::Acquire);
        let mut out = MacroAssignments::empty();
        for m in 0..MACRO_COUNT {
            for k in 0..MACRO_SLOTS {
                let t = self.targets[m][k].load(Ordering::Relaxed);
                let target = if t == NO_TARGET { None } else { ParamId::from_index(t as usize) };
                out.slots[m][k] = MacroSlot { target, depth: bits_to_f32(self.depths[m][k].load(Ordering::Relaxed)) };
            }
        }
        out
    }

    pub fn version(&self) -> u32 {
        self.version.load(Ordering::Acquire)
    }
}

/// Telemetry published by the audio thread, sampled by the GUI.
pub struct AtomicTelemetry {
    tempo_bpm: AtomicU32,
    transport_playing: AtomicBool,
    peak: [AtomicU32; 2],
    rms: [AtomicU32; 2],
    peak_hold: AtomicU32,
    active_voices: AtomicU8,
    held: [AtomicU64; 2],
    last_note: AtomicI32,
    lfsr: AtomicU16,
    wave_step: AtomicI32,
    env: [AtomicU32; 4],
    pitch_offset: AtomicU32,
    arp_step: AtomicI32,
    frame: AtomicU64,
}

impl Default for AtomicTelemetry {
    fn default() -> Self {
        let t = Self {
            tempo_bpm: AtomicU32::new(0),
            transport_playing: AtomicBool::new(false),
            peak: std::array::from_fn(|_| AtomicU32::new(0)),
            rms: std::array::from_fn(|_| AtomicU32::new(0)),
            peak_hold: AtomicU32::new(0),
            active_voices: AtomicU8::new(0),
            held: std::array::from_fn(|_| AtomicU64::new(0)),
            last_note: AtomicI32::new(-1),
            lfsr: AtomicU16::new(0x7fff),
            wave_step: AtomicI32::new(-1),
            env: std::array::from_fn(|_| AtomicU32::new(0)),
            pitch_offset: AtomicU32::new(0),
            arp_step: AtomicI32::new(-1),
            frame: AtomicU64::new(0),
        };
        t.publish(&TelemetrySnapshot::default());
        t
    }
}

impl AtomicTelemetry {
    pub fn publish(&self, s: &TelemetrySnapshot) {
        let r = Ordering::Relaxed;
        self.tempo_bpm.store(f32_to_bits(s.tempo_bpm), r);
        self.transport_playing.store(s.transport_playing, r);
        for i in 0..2 {
            self.peak[i].store(f32_to_bits(s.peak_db[i]), r);
            self.rms[i].store(f32_to_bits(s.rms_db[i]), r);
            self.held[i].store(s.held_notes[i], r);
        }
        self.peak_hold.store(f32_to_bits(s.peak_hold_db), r);
        self.active_voices.store(s.active_voices, r);
        self.last_note.store(s.last_note.map_or(-1, |n| n as i32), r);
        self.lfsr.store(s.lfsr, r);
        self.wave_step.store(s.wave_step.map_or(-1, |n| n as i32), r);
        for i in 0..4 {
            self.env[i].store(f32_to_bits(s.env_level[i]), r);
        }
        self.pitch_offset.store(f32_to_bits(s.pitch_offset_st), r);
        self.arp_step.store(s.arp_step.map_or(-1, |n| n as i32), r);
        self.frame.store(s.frame, Ordering::Release);
    }

    pub fn load(&self) -> TelemetrySnapshot {
        let frame = self.frame.load(Ordering::Acquire);
        let r = Ordering::Relaxed;
        let opt = |v: i32| if v < 0 { None } else { Some(v as u8) };
        TelemetrySnapshot {
            tempo_bpm: bits_to_f32(self.tempo_bpm.load(r)),
            transport_playing: self.transport_playing.load(r),
            peak_db: [bits_to_f32(self.peak[0].load(r)), bits_to_f32(self.peak[1].load(r))],
            rms_db: [bits_to_f32(self.rms[0].load(r)), bits_to_f32(self.rms[1].load(r))],
            peak_hold_db: bits_to_f32(self.peak_hold.load(r)),
            active_voices: self.active_voices.load(r),
            held_notes: [self.held[0].load(r), self.held[1].load(r)],
            last_note: opt(self.last_note.load(r)),
            lfsr: self.lfsr.load(r),
            wave_step: opt(self.wave_step.load(r)),
            env_level: std::array::from_fn(|i| bits_to_f32(self.env[i].load(r))),
            pitch_offset_st: bits_to_f32(self.pitch_offset.load(r)),
            arp_step: opt(self.arp_step.load(r)),
            frame,
        }
    }
}

/// A flag the audio thread or host callbacks raise and the GUI clears (e.g. "parameters
/// changed, redraw").
#[derive(Default)]
pub struct DirtyFlag(AtomicBool);

impl DirtyFlag {
    pub fn raise(&self) {
        self.0.store(true, Ordering::Release);
    }
    /// Returns whether the flag was raised, clearing it.
    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wave;

    #[test]
    fn wave_roundtrip() {
        let w = AtomicWave::new(wave::default_wave());
        let v0 = w.version();
        w.store(&wave::sine());
        assert_eq!(w.load(), wave::sine());
        assert!(w.version() > v0);
    }

    #[test]
    fn macros_roundtrip() {
        let mut a = MacroAssignments::empty();
        a.set(3, ParamId::EchoMix, 0.7);
        let am = AtomicMacros::new(&MacroAssignments::empty());
        am.store(&a);
        assert_eq!(am.load(), a);
    }

    #[test]
    fn telemetry_roundtrip() {
        let t = AtomicTelemetry::default();
        assert_eq!(t.load(), TelemetrySnapshot::default());
        let mut s = TelemetrySnapshot { tempo_bpm: 137.25, transport_playing: true, last_note: Some(60), wave_step: Some(7), frame: 42, ..Default::default() };
        s.held_notes[0] = 1 << 60;
        t.publish(&s);
        let l = t.load();
        assert_eq!(l, s);
        assert!(l.is_held(60));
        assert!(!l.is_held(61));
        s.tempo_bpm = 89.5;
        s.transport_playing = false;
        t.publish(&s);
        assert_eq!(t.load(), s, "tempo and stop changes are published even within the same chip frame");
    }
}
