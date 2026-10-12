//! A minimal "host" around the production [`Processor`]: it owns the shared wave/macro
//! containers and the base parameter values exactly like the plugin does, and renders a
//! list of timed note events in fixed host blocks.

use crate::measure::SR;
use pixl::midi::TimedEvent;
use pixl::processor::{BlockInput, BlockStatus, Processor};
use pixl_core::preset::PresetState;
use pixl_core::sync::{AtomicMacros, AtomicWave};
use pixl_core::{ParamId, ParamValues, TelemetrySnapshot};
use pixl_dsp::{EngineEvent, Transport};

/// Host block size (samples).
pub const BLOCK: usize = 512;
/// Silence rendered (and discarded) before the first event, so smoothed parameters such as
/// the output gain have settled.
pub const PRE_ROLL: usize = 4800;

/// A note event at an absolute sample position (relative to the end of the pre-roll).
#[derive(Clone, Copy, Debug)]
pub struct At(pub usize, pub EngineEvent);

/// Note on at `at` (velocity 0..1).
pub fn on(at: usize, note: u8, velocity: f32) -> At {
    At(at, EngineEvent::NoteOn { note, velocity })
}

/// Note off at `at`.
pub fn off(at: usize, note: u8) -> At {
    At(at, EngineEvent::NoteOff { note })
}

/// Seconds → samples at [`SR`].
pub fn secs(s: f64) -> usize {
    (s * SR as f64).round() as usize
}

/// A rendered stereo signal plus what the processor reported.
pub struct Render {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Status after the last block.
    pub final_status: BlockStatus,
    /// End (samples, after the pre-roll) of the last block in which voices were sounding.
    pub voices_end: usize,
    /// End of the last block that was not completely silent (voices or effect tail).
    pub sound_end: usize,
    /// End of the last block after which the telemetry still counted sounding voices.
    pub voice_count_end: usize,
    /// Per host block (after the pre-roll): sounding voices and the most recent voice's
    /// pitch modulation offset (semitones: sweep, vibrato, glide, arp, bend).
    pub voices: Vec<u8>,
    pub pitch_mod: Vec<f32>,
    /// Times the processor's output guard had to reset the DSP (must stay 0).
    pub recoveries: u64,
}

/// Render `total` samples of `state` with `values` overriding the stored parameters
/// (macro positions, output gain), delivering `events`.
pub fn render(state: &PresetState, values: &ParamValues, events: &[At], total: usize) -> Render {
    render_traced(state, values, events, total, |_, _| {})
}

/// [`render`], calling `trace(block_end, telemetry)` after every host block (block ends are
/// relative to the end of the pre-roll; pre-roll blocks are not reported).
pub fn render_traced(
    state: &PresetState,
    values: &ParamValues,
    events: &[At],
    total: usize,
    mut trace: impl FnMut(usize, &TelemetrySnapshot),
) -> Render {
    let mut processor = Processor::new();
    processor.prepare(SR);
    let wave = AtomicWave::new(state.wave);
    let macros = AtomicMacros::new(&state.macros);
    let transport = Transport::default();
    let mut events: Vec<At> = events.iter().map(|e| At(e.0 + PRE_ROLL, e.1)).collect();
    events.sort_by_key(|e| e.0);
    let len = total + PRE_ROLL;
    let (mut left, mut right) = (vec![0.0f32; len], vec![0.0f32; len]);
    let mut status = BlockStatus::Silent;
    let (mut voices_end, mut sound_end, mut voice_count_end) = (0usize, 0usize, 0usize);
    let (mut voices, mut pitch_mod) = (Vec::new(), Vec::new());
    let mut pos = 0;
    let mut next = 0;
    while pos < len {
        let end = (pos + BLOCK).min(len);
        let mut timed = Vec::new();
        while next < events.len() && events[next].0 < end {
            let At(at, event) = events[next];
            timed.push(TimedEvent { offset: at.saturating_sub(pos) as u32, event });
            next += 1;
        }
        let input = BlockInput { params: values, wave: &wave, macros: &macros, transport };
        status = processor.process(&input, timed, &mut left[pos..end], &mut right[pos..end]);
        if status == BlockStatus::Active {
            voices_end = end.saturating_sub(PRE_ROLL);
        }
        if status != BlockStatus::Silent {
            sound_end = end.saturating_sub(PRE_ROLL);
        }
        if end > PRE_ROLL {
            let t = processor.telemetry();
            if t.active_voices > 0 {
                voice_count_end = end - PRE_ROLL;
            }
            voices.push(t.active_voices);
            pitch_mod.push(t.pitch_offset_st);
            trace(end - PRE_ROLL, t);
        }
        pos = end;
    }
    left.drain(..PRE_ROLL);
    right.drain(..PRE_ROLL);
    Render {
        left,
        right,
        final_status: status,
        voices_end,
        sound_end,
        voice_count_end,
        voices,
        pitch_mod,
        recoveries: processor.recoveries(),
    }
}

/// The preset's stored values with the output gain replaced and optionally one macro moved.
pub fn values_with(state: &PresetState, out_gain_db: f32, macro_override: Option<(usize, f32)>) -> ParamValues {
    let mut v = state.values;
    v.set(ParamId::OutGain, out_gain_db);
    if let Some((m, value)) = macro_override {
        v.set(ParamId::macro_value(m), value);
    }
    v
}
