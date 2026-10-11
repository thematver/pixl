//! PIXL synthesis engine (voices only; effects live in `pixl-fx`). SPEC §3–§4.
//!
//! # Architecture
//!
//! - [`blip`]: zero-latency minimum-phase band-limited step synthesis (Blip_Buffer style).
//!   Every amplitude change of every channel becomes one band-limited step; nothing is
//!   sampled, so pulses, waves and noise are alias-free at any pitch.
//! - [`chip`]: NES 2A03 / Game Boy DMG constants, register quantization, duty sequences,
//!   noise tables, LFSRs and the 2A03 mixer formulas.
//! - [`osc`]: event-driven oscillators that know the exact time of their next output change
//!   (pulse edges, wave steps, LFSR output flips; integrate-and-dump for very fast noise).
//! - [`env`], [`modulation`]: ADSR, sweep, vibrato, glide and arpeggiator sequencing.
//! - [`voice`]: voices with up to four unison sub-voices × four chip channels, the linear and
//!   2A03 mixers, frame-stepped modulation and click-free fades.
//! - [`engine`]: performance state (note stack, sustain, poly/mono/legato allocation and
//!   stealing), the global frame / control / arpeggiator clocks and chunked rendering.
//! - [`analysis`]: FFT and measurement helpers for tests and tools (not for the audio thread).
//!
//! # Contract (SPEC §4.1)
//!
//! The caller splits blocks at event times and at least every 64 samples, calling
//! [`Engine::set_params`] before each [`Engine::render`]. Events apply at the start of the
//! next rendered sample. Supported sample rates: 22.05–192 kHz (others are clamped to a safe
//! range and still render).

#![warn(missing_docs)]

pub mod analysis;
pub mod blip;
pub mod chip;
pub mod engine;
pub mod env;
pub mod modulation;
pub mod osc;
pub mod resolve;
pub mod voice;

pub use engine::{Engine, VoiceInfo};

/// Host transport information used for tempo sync.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transport {
    /// Tempo in BPM (fallback 120 when the host provides none).
    pub tempo_bpm: f64,
    /// Whether the host transport is playing.
    pub playing: bool,
    /// Song position in quarter-note beats, if known.
    pub beat_pos: Option<f64>,
}

impl Default for Transport {
    fn default() -> Self {
        Self { tempo_bpm: 120.0, playing: false, beat_pos: None }
    }
}

/// Performance events, applied at the current render position. The caller splits blocks
/// at event times for sample accuracy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineEvent {
    /// Note on (0 velocity is a note-off).
    NoteOn {
        /// MIDI note 0..127 (others are ignored).
        note: u8,
        /// Velocity 0..1.
        velocity: f32,
    },
    /// Note off (deferred while the sustain pedal is down).
    NoteOff {
        /// MIDI note 0..127.
        note: u8,
    },
    /// Pitch bend.
    PitchBend {
        /// −1..1 (scaled by the `pb_range` parameter).
        value: f32,
    },
    /// Mod wheel (CC1): adds up to +50 cents of vibrato, even with the vibrato tile off.
    ModWheel {
        /// 0..1.
        value: f32,
    },
    /// Channel pressure: adds up to +50 cents of vibrato.
    Aftertouch {
        /// 0..1.
        value: f32,
    },
    /// Sustain pedal (CC64).
    Sustain {
        /// Pedal down.
        on: bool,
    },
    /// Release all notes (CC123).
    AllNotesOff,
    /// Silence immediately (CC120, transport stop/reset): every voice fades out in 2 ms.
    AllSoundOff,
}
