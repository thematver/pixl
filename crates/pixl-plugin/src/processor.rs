//! The framework-free audio processor: engine + effect chain + block splitting.
//!
//! [`Processor`] contains everything the plugin does on the audio thread, without any
//! nih-plug types, so it can be driven directly from tests:
//!
//! 1. pick up an editor preset load from the [`PresetMailbox`] (see [`crate::handover`]: the
//!    whole preset — parameters, wave RAM, macros — switches at this block boundary), or the
//!    wave RAM and macro routings from the lock-free shared containers (only when their
//!    version changed);
//! 2. compute the effective parameter values (base values + macro modulation, via
//!    [`pixl_core::apply_macros`]);
//! 3. split the block at every event and at least every [`SUB_BLOCK`] samples; before each
//!    sub-block the effective values go to the engine and the effect chain, events due at
//!    that position are applied, then the engine renders and the chain processes in place;
//! 4. guard the output (a non-finite sample resets the DSP and silences the sub-block — a
//!    last line of defence, the DSP crates must never produce one);
//! 5. collect telemetry (voices from the engine, meters from the chain).
//!
//! Nothing here allocates, locks or performs I/O after [`Processor::prepare`].

use crate::handover::{Handover, HandoverReceiver, PresetMailbox};
use crate::midi::TimedEvent;
use pixl_core::sync::{AtomicMacros, AtomicWave};
use pixl_core::{apply_macros, MacroAssignments, ParamId, ParamValues, TelemetrySnapshot, WaveRam, MACRO_COUNT};
use pixl_dsp::{Engine, EngineEvent, Transport};
use pixl_fx::FxChain;

/// Longest stretch rendered with constant parameters (samples).
pub const SUB_BLOCK: usize = 64;
/// Lowest sample rate PIXL renders at (Hz).
///
/// SPEC's quality guarantees cover 22.05–192 kHz ([`SPEC_MIN_SAMPLE_RATE`]..=
/// [`SPEC_MAX_SAMPLE_RATE`]); the engine and the effect chain render correctly from 8 to
/// 768 kHz. Outside that range they would clamp their internal rate (wrong pitch and
/// timing), so the processor outputs silence instead.
pub const MIN_SAMPLE_RATE: f32 = 8_000.0;
/// Highest sample rate PIXL renders at (Hz). See [`MIN_SAMPLE_RATE`].
pub const MAX_SAMPLE_RATE: f32 = 768_000.0;
/// Lowest sample rate covered by SPEC's quality guarantees (Hz).
pub const SPEC_MIN_SAMPLE_RATE: f32 = 22_050.0;
/// Highest sample rate covered by SPEC's quality guarantees (Hz).
pub const SPEC_MAX_SAMPLE_RATE: f32 = 192_000.0;

/// The voice engine as seen by the processor (implemented by [`pixl_dsp::Engine`]; tests use
/// mocks). All methods except `prepare` must be real-time safe.
pub trait VoiceEngine {
    fn prepare(&mut self, sample_rate: f32, max_block: usize);
    fn reset(&mut self);
    fn set_params(&mut self, params: &ParamValues);
    fn set_wave(&mut self, wave: &WaveRam);
    fn set_transport(&mut self, transport: Transport);
    fn event(&mut self, event: EngineEvent);
    fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]);
    fn fill_telemetry(&self, t: &mut TelemetrySnapshot);
    fn is_silent(&self) -> bool;
}

/// The effect chain as seen by the processor (implemented by [`pixl_fx::FxChain`]).
pub trait EffectChain {
    fn prepare(&mut self, sample_rate: f32, max_block: usize);
    fn reset(&mut self);
    fn set_params(&mut self, params: &ParamValues);
    fn set_tempo(&mut self, bpm: f64);
    fn process(&mut self, l: &mut [f32], r: &mut [f32]);
    fn fill_telemetry(&self, t: &mut TelemetrySnapshot);
    fn tail_samples(&self) -> u32;
}

impl VoiceEngine for Engine {
    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        Engine::prepare(self, sample_rate, max_block)
    }
    fn reset(&mut self) {
        Engine::reset(self)
    }
    fn set_params(&mut self, params: &ParamValues) {
        Engine::set_params(self, params)
    }
    fn set_wave(&mut self, wave: &WaveRam) {
        Engine::set_wave(self, wave)
    }
    fn set_transport(&mut self, transport: Transport) {
        Engine::set_transport(self, transport)
    }
    fn event(&mut self, event: EngineEvent) {
        Engine::event(self, event)
    }
    fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        Engine::render(self, out_l, out_r)
    }
    fn fill_telemetry(&self, t: &mut TelemetrySnapshot) {
        Engine::fill_telemetry(self, t)
    }
    fn is_silent(&self) -> bool {
        Engine::is_silent(self)
    }
}

impl EffectChain for FxChain {
    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        FxChain::prepare(self, sample_rate, max_block)
    }
    fn reset(&mut self) {
        FxChain::reset(self)
    }
    fn set_params(&mut self, params: &ParamValues) {
        FxChain::set_params(self, params)
    }
    fn set_tempo(&mut self, bpm: f64) {
        FxChain::set_tempo(self, bpm)
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        FxChain::process(self, l, r)
    }
    fn fill_telemetry(&self, t: &mut TelemetrySnapshot) {
        FxChain::fill_telemetry(self, t)
    }
    fn tail_samples(&self) -> u32 {
        FxChain::tail_samples(self)
    }
}

/// Inputs of one processing call besides audio and events.
#[derive(Clone, Copy)]
pub struct BlockInput<'a> {
    /// Base (unmodulated by macros) plain parameter values, already sanitized.
    pub params: &'a ParamValues,
    /// Shared wave RAM.
    pub wave: &'a AtomicWave,
    /// Shared macro routings.
    pub macros: &'a AtomicMacros,
    /// Host transport.
    pub transport: Transport,
}

/// What the processor reports after a block (mapped to the host's process status).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockStatus {
    /// Voices are sounding: keep processing.
    Active,
    /// No voices, but the effect chain still has this many samples of tail.
    Tail(u32),
    /// Completely silent.
    Silent,
}

/// Engine + effects + block splitting + macro application + telemetry.
pub struct Processor<E: VoiceEngine = Engine, F: EffectChain = FxChain> {
    engine: E,
    fx: F,
    sample_rate: f32,
    prepared: bool,
    rate_supported: bool,
    /// Effective (macro-applied) values of the current call.
    effective: ParamValues,
    /// Current macro routings (copied from the shared container when its version changes).
    assignments: MacroAssignments,
    /// The wave RAM the engine plays (re-sent after a reset or re-prepare instead of being
    /// re-read from the shared container, which may still hold an older preset's wave).
    wave: WaveRam,
    wave_dirty: bool,
    wave_version: Option<u32>,
    macros_version: Option<u32>,
    /// Audio side of the editor's preset handover.
    handover: HandoverReceiver,
    telemetry: TelemetrySnapshot,
    /// Number of times the output guard had to reset the DSP.
    recoveries: u64,
}

impl Processor<Engine, FxChain> {
    /// A processor with the real PIXL engine and effect chain. Allocates.
    pub fn new() -> Self {
        Self::with_parts(Engine::new(SUB_BLOCK), FxChain::new())
    }
}

impl Default for Processor<Engine, FxChain> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: VoiceEngine, F: EffectChain> Processor<E, F> {
    /// A processor around the given engine and effect chain.
    pub fn with_parts(engine: E, fx: F) -> Self {
        Self {
            engine,
            fx,
            sample_rate: 0.0,
            prepared: false,
            rate_supported: false,
            effective: ParamValues::defaults(),
            assignments: MacroAssignments::factory_default(),
            wave: pixl_core::wave::default_wave(),
            wave_dirty: true,
            wave_version: None,
            macros_version: None,
            handover: HandoverReceiver::new(),
            telemetry: TelemetrySnapshot::default(),
            recoveries: 0,
        }
    }

    /// Configure for a sample rate. Re-prepares the DSP only when the rate changed (the
    /// wrapper also calls `initialize` after every state load, so the common path must not
    /// allocate). May allocate otherwise. Returns whether the DSP was (re-)prepared.
    pub fn prepare(&mut self, sample_rate: f32) -> bool {
        let sample_rate = if sample_rate.is_finite() && sample_rate > 0.0 { sample_rate } else { 44_100.0 };
        self.rate_supported = (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&sample_rate);
        if self.prepared && sample_rate == self.sample_rate {
            return false;
        }
        self.sample_rate = sample_rate;
        if self.rate_supported {
            self.engine.prepare(sample_rate, SUB_BLOCK);
            self.fx.prepare(sample_rate, SUB_BLOCK);
        }
        self.prepared = true;
        self.wave_dirty = true;
        true
    }

    /// Silence everything and clear DSP state. Real-time safe.
    pub fn reset(&mut self) {
        self.engine.reset();
        self.fx.reset();
        // Make sure the engine gets the wave RAM again after a reset.
        self.wave_dirty = true;
        self.telemetry = TelemetrySnapshot::default();
    }

    /// Sample rate set by [`Self::prepare`].
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Whether the current sample rate is within the supported range.
    pub fn is_rate_supported(&self) -> bool {
        self.rate_supported
    }

    /// Effective (macro-applied) parameter values of the last processed block.
    pub fn effective_values(&self) -> &ParamValues {
        &self.effective
    }

    /// Telemetry gathered at the end of the last block.
    pub fn telemetry(&self) -> &TelemetrySnapshot {
        &self.telemetry
    }

    /// How often the output guard caught non-finite samples.
    pub fn recoveries(&self) -> u64 {
        self.recoveries
    }

    /// The engine (for tests and diagnostics).
    pub fn engine(&self) -> &E {
        &self.engine
    }

    /// The effect chain (for tests and diagnostics).
    pub fn fx(&self) -> &F {
        &self.fx
    }

    /// Render one block. `left`/`right` are overwritten (the shorter length is rendered; any
    /// excess of the longer one is zeroed). Event offsets are relative to the block start;
    /// events must come in non-decreasing offset order (later-arriving earlier offsets are
    /// applied immediately), events at or beyond the block end are applied after rendering.
    pub fn process<I>(&mut self, input: &BlockInput<'_>, events: I, left: &mut [f32], right: &mut [f32]) -> BlockStatus
    where
        I: IntoIterator<Item = TimedEvent>,
    {
        self.process_with(input, None, events, left, right)
    }

    /// [`Self::process`] with the editor's preset handover: while `handover` holds a preset
    /// the editor is loading, its parameter values, wave RAM and macros are played instead of
    /// `input` (all of them from the same block on), until nih-plug has applied the preset's
    /// parameter changes (see [`crate::handover`]). Wait-free, allocation-free.
    pub fn process_with<I>(
        &mut self,
        input: &BlockInput<'_>,
        handover: Option<&PresetMailbox>,
        events: I,
        left: &mut [f32],
        right: &mut [f32],
    ) -> BlockStatus
    where
        I: IntoIterator<Item = TimedEvent>,
    {
        let len = left.len().min(right.len());
        left[len..].fill(0.0);
        right[len..].fill(0.0);
        let (left, right) = (&mut left[..len], &mut right[..len]);

        if !self.prepared {
            self.prepare(44_100.0);
        }
        if let Some(mailbox) = handover {
            self.begin_handover(mailbox);
        }
        if !self.handover.is_holding() {
            self.sync_shared(input);
        }
        if self.wave_dirty {
            self.wave_dirty = false;
            self.engine.set_wave(&self.wave);
        }
        let base = match self.handover.held() {
            Some(record) => record.values,
            None => *input.params,
        };
        self.update_effective(&base);
        let transport = sanitize_transport(input.transport);

        if !self.rate_supported {
            // Unsupported sample rate: stay silent (events are consumed and dropped).
            events.into_iter().for_each(drop);
            left.fill(0.0);
            right.fill(0.0);
            self.telemetry = TelemetrySnapshot { tempo_bpm: transport.tempo_bpm as f32, transport_playing: transport.playing, ..Default::default() };
            self.end_handover(input, handover);
            return BlockStatus::Silent;
        }

        self.engine.set_transport(transport);
        self.fx.set_tempo(transport.tempo_bpm);

        let mut events = events.into_iter();
        let mut next = events.next();
        let mut pos = 0usize;
        while pos < len {
            // Parameters first: an event at this position (a note-on right after a preset
            // load or an automation step, which nih-plug delivers at offset 0 of a new call)
            // must see the values that apply from this sample on.
            self.set_params();
            while let Some(ev) = next.filter(|ev| ev.offset as usize <= pos) {
                self.engine.event(ev.event);
                next = events.next();
            }
            let limit = next.map_or(len, |ev| (ev.offset as usize).min(len));
            let end = (pos + SUB_BLOCK).min(limit).max(pos + 1);
            self.render_sub_block(&mut left[pos..end], &mut right[pos..end]);
            pos = end;
        }
        // Events at or beyond the block end (all events of an empty block).
        if next.is_some() {
            if len == 0 {
                self.set_params();
            }
            while let Some(ev) = next {
                self.engine.event(ev.event);
                next = events.next();
            }
        }

        self.collect_telemetry(transport);
        self.end_handover(input, handover);
        if !self.engine.is_silent() {
            BlockStatus::Active
        } else {
            match self.fx.tail_samples() {
                0 => BlockStatus::Silent,
                n => BlockStatus::Tail(n),
            }
        }
    }

    /// Start of a call: adopt a newly held preset's wave RAM and macros, or forget the held one
    /// after a host state load.
    fn begin_handover(&mut self, mailbox: &PresetMailbox) {
        match self.handover.begin(mailbox) {
            Handover::Hold => {
                if let Some(record) = self.handover.held() {
                    if record.wave != self.wave {
                        self.wave = record.wave;
                        self.wave_dirty = true;
                    }
                    let mut a = record.macros;
                    a.sanitize();
                    self.assignments = a;
                }
            }
            Handover::Cancel => {
                self.wave_version = None;
                self.macros_version = None;
            }
            Handover::Unchanged => {}
        }
    }

    /// End of a call: when the held preset is released, keep its wave RAM and macros until the
    /// shared containers hold them (the editor installs them once nih-plug has applied the
    /// parameters); re-read the shared ones if they already do.
    fn end_handover(&mut self, input: &BlockInput<'_>, handover: Option<&PresetMailbox>) {
        let Some(mailbox) = handover else { return };
        if let Some(released) = self.handover.end() {
            // Versions first: a later install bumps them, so it is never missed.
            let (wv, mv) = (input.wave.version(), input.macros.version());
            if mailbox.committed() >= released {
                self.wave_version = None;
                self.macros_version = None;
            } else {
                self.wave_version = Some(wv);
                self.macros_version = Some(mv);
            }
        }
    }

    fn sync_shared(&mut self, input: &BlockInput<'_>) {
        // Read the version before the data: a concurrent write bumps the version after
        // storing, so a torn read is always followed by a re-read on the next block.
        let wv = input.wave.version();
        if self.wave_version != Some(wv) {
            self.wave_version = Some(wv);
            let wave = input.wave.load();
            if wave != self.wave {
                self.wave = wave;
                self.wave_dirty = true;
            }
        }
        let mv = input.macros.version();
        if self.macros_version != Some(mv) {
            self.macros_version = Some(mv);
            let mut a = input.macros.load();
            a.sanitize();
            self.assignments = a;
        }
    }

    fn update_effective(&mut self, base: &ParamValues) {
        let macro_values: [f32; MACRO_COUNT] =
            std::array::from_fn(|m| (base.get(ParamId::macro_value(m)) / 100.0).clamp(0.0, 1.0));
        apply_macros(base, macro_values, &self.assignments, &mut self.effective);
    }

    /// Hand the effective values to the engine and the effect chain (before every sub-block
    /// and before the events at its start).
    fn set_params(&mut self) {
        self.engine.set_params(&self.effective);
        self.fx.set_params(&self.effective);
    }

    fn render_sub_block(&mut self, l: &mut [f32], r: &mut [f32]) {
        self.engine.render(l, r);
        self.fx.process(l, r);
        if l.iter().chain(r.iter()).any(|x| !x.is_finite()) {
            l.fill(0.0);
            r.fill(0.0);
            self.engine.reset();
            self.fx.reset();
            self.wave_dirty = true;
            self.recoveries = self.recoveries.wrapping_add(1);
        }
    }

    fn collect_telemetry(&mut self, transport: Transport) {
        let mut t = TelemetrySnapshot::default();
        self.engine.fill_telemetry(&mut t);
        self.fx.fill_telemetry(&mut t);
        t.tempo_bpm = transport.tempo_bpm as f32;
        t.transport_playing = transport.playing;
        sanitize_telemetry(&mut t);
        self.telemetry = t;
    }
}

/// Clamp host transport values into something the DSP can always use.
pub fn sanitize_transport(t: Transport) -> Transport {
    let tempo_bpm = if t.tempo_bpm.is_finite() && t.tempo_bpm > 0.0 { t.tempo_bpm.clamp(20.0, 999.0) } else { 120.0 };
    let beat_pos = t.beat_pos.filter(|b| b.is_finite());
    Transport { tempo_bpm, playing: t.playing, beat_pos }
}

fn sanitize_telemetry(t: &mut TelemetrySnapshot) {
    use pixl_core::bridge::METER_FLOOR_DB;
    let db = |v: &mut f32| {
        if !v.is_finite() {
            *v = METER_FLOOR_DB;
        }
        *v = v.max(METER_FLOOR_DB);
    };
    t.peak_db.iter_mut().for_each(db);
    t.rms_db.iter_mut().for_each(db);
    db(&mut t.peak_hold_db);
    for e in &mut t.env_level {
        *e = if e.is_finite() { e.clamp(0.0, 1.0) } else { 0.0 };
    }
    if !t.pitch_offset_st.is_finite() {
        t.pitch_offset_st = 0.0;
    }
    t.lfsr &= 0x7fff;
    t.wave_step = t.wave_step.filter(|s| (*s as usize) < pixl_core::WAVE_LEN);
    t.last_note = t.last_note.filter(|n| *n < 128);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pixl_core::wave;

    /// A deterministic stand-in engine: a "voice" is a constant 0.5 per held note, emitted
    /// from the exact sample its note-on was applied. Records every call.
    #[derive(Default)]
    pub(crate) struct MockEngine {
        pub(crate) prepared: Option<(f32, usize)>,
        pub(crate) held: Vec<u8>,
        pub(crate) events: Vec<(u64, EngineEvent)>,
        pub(crate) renders: Vec<usize>,
        pub(crate) params_seen: Vec<ParamValues>,
        pub(crate) wave: Option<WaveRam>,
        pub(crate) wave_sets: usize,
        pub(crate) transport: Option<Transport>,
        pub(crate) position: u64,
        pub(crate) resets: usize,
        pub(crate) poison: bool,
        /// Call order: 'p' set_params, 'e' event, 'r' render.
        pub(crate) trace: String,
        /// The values the engine had when each event arrived.
        pub(crate) params_at_event: Vec<ParamValues>,
    }

    impl VoiceEngine for MockEngine {
        fn prepare(&mut self, sample_rate: f32, max_block: usize) {
            self.prepared = Some((sample_rate, max_block));
        }
        fn reset(&mut self) {
            self.held.clear();
            self.resets += 1;
        }
        fn set_params(&mut self, params: &ParamValues) {
            self.params_seen.push(*params);
            self.trace.push('p');
        }
        fn set_wave(&mut self, wave: &WaveRam) {
            self.wave = Some(*wave);
            self.wave_sets += 1;
        }
        fn set_transport(&mut self, transport: Transport) {
            self.transport = Some(transport);
        }
        fn event(&mut self, event: EngineEvent) {
            self.events.push((self.position, event));
            self.trace.push('e');
            self.params_at_event.push(self.params_seen.last().copied().unwrap_or_default());
            match event {
                EngineEvent::NoteOn { note, .. } => self.held.push(note),
                EngineEvent::NoteOff { note } => self.held.retain(|n| *n != note),
                EngineEvent::AllNotesOff | EngineEvent::AllSoundOff => self.held.clear(),
                _ => {}
            }
        }
        fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
            assert!(out_l.len() <= SUB_BLOCK && out_l.len() == out_r.len() && !out_l.is_empty());
            let v = if self.poison { f32::NAN } else { 0.5 * self.held.len() as f32 };
            out_l.fill(v);
            out_r.fill(-v);
            self.trace.push('r');
            self.renders.push(out_l.len());
            self.position += out_l.len() as u64;
        }
        fn fill_telemetry(&self, t: &mut TelemetrySnapshot) {
            t.active_voices = self.held.len() as u8;
            t.last_note = self.held.last().copied();
            t.pitch_offset_st = f32::NAN; // must be sanitized
        }
        fn is_silent(&self) -> bool {
            self.held.is_empty()
        }
    }

    /// Effect chain stand-in: applies `out_gain` (dB) and reports a fixed tail.
    #[derive(Default)]
    pub(crate) struct MockFx {
        pub(crate) gain_db: f32,
        pub(crate) tempo: f64,
        pub(crate) tail: u32,
        pub(crate) processed: usize,
    }

    impl EffectChain for MockFx {
        fn prepare(&mut self, _sample_rate: f32, _max_block: usize) {}
        fn reset(&mut self) {}
        fn set_params(&mut self, params: &ParamValues) {
            self.gain_db = params.get(ParamId::OutGain);
        }
        fn set_tempo(&mut self, bpm: f64) {
            self.tempo = bpm;
        }
        fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
            let g = 10f32.powf(self.gain_db / 20.0);
            l.iter_mut().chain(r.iter_mut()).for_each(|x| *x *= g);
            self.processed += l.len();
        }
        fn fill_telemetry(&self, t: &mut TelemetrySnapshot) {
            t.peak_db = [-6.0, f32::NAN];
        }
        fn tail_samples(&self) -> u32 {
            self.tail
        }
    }

    pub(crate) struct Shared {
        pub(crate) wave: AtomicWave,
        pub(crate) macros: AtomicMacros,
    }

    impl Shared {
        pub(crate) fn new() -> Self {
            Self { wave: AtomicWave::new(wave::default_wave()), macros: AtomicMacros::new(&MacroAssignments::empty()) }
        }
        pub(crate) fn input<'a>(&'a self, params: &'a ParamValues) -> BlockInput<'a> {
            BlockInput { params, wave: &self.wave, macros: &self.macros, transport: Transport::default() }
        }
    }

    fn mock() -> Processor<MockEngine, MockFx> {
        let mut p = Processor::with_parts(MockEngine::default(), MockFx::default());
        p.prepare(48_000.0);
        p
    }

    fn on(offset: u32, note: u8) -> TimedEvent {
        TimedEvent { offset, event: EngineEvent::NoteOn { note, velocity: 1.0 } }
    }

    fn off(offset: u32, note: u8) -> TimedEvent {
        TimedEvent { offset, event: EngineEvent::NoteOff { note } }
    }

    #[test]
    fn events_are_sample_accurate() {
        for n in [0u32, 1, 17, 63, 64, 65, 200, 511] {
            let mut p = mock();
            let shared = Shared::new();
            let params = ParamValues::defaults();
            let (mut l, mut r) = (vec![9.0f32; 512], vec![9.0f32; 512]);
            let status = p.process(&shared.input(&params), [on(n, 60)], &mut l, &mut r);
            assert_eq!(status, BlockStatus::Active);
            let n = n as usize;
            assert!(l[..n].iter().all(|x| *x == 0.0), "silence before {n}");
            assert!(l[n..].iter().all(|x| *x == 0.5), "sound from {n}");
            assert!(r[n..].iter().all(|x| *x == -0.5));
            assert_eq!(p.engine().events, [(n as u64, EngineEvent::NoteOn { note: 60, velocity: 1.0 })]);
        }
    }

    #[test]
    fn blocks_are_split_at_events_and_every_64_samples() {
        let mut p = mock();
        let shared = Shared::new();
        let params = ParamValues::defaults();
        let (mut l, mut r) = (vec![0.0f32; 300], vec![0.0f32; 300]);
        let events = [on(10, 60), on(10, 64), off(100, 60), off(299, 64), on(450, 67)];
        p.process(&shared.input(&params), events, &mut l, &mut r);
        let renders = &p.engine().renders;
        assert_eq!(renders.iter().sum::<usize>(), 300);
        assert!(renders.iter().all(|n| *n <= SUB_BLOCK));
        assert_eq!(renders[..4], [10, 64, 26, 64]);
        // set_params before every render.
        assert_eq!(p.engine().params_seen.len(), renders.len());
        // Events at their exact positions; the out-of-block event is applied at the end.
        let positions: Vec<u64> = p.engine().events.iter().map(|e| e.0).collect();
        assert_eq!(positions, [10, 10, 100, 299, 300]);
        assert!(l[..10].iter().all(|x| *x == 0.0));
        assert!(l[10..100].iter().all(|x| *x == 1.0));
        assert!(l[100..299].iter().all(|x| *x == 0.5));
        assert_eq!(l[299], 0.0);
        // Empty blocks still apply events.
        let mut p = mock();
        p.process(&shared.input(&params), [on(0, 1)], &mut [], &mut []);
        assert_eq!(p.engine().events.len(), 1);
        assert!(p.engine().renders.is_empty());
    }

    #[test]
    fn parameters_are_set_before_the_events_at_the_same_position() {
        let mut p = mock();
        let shared = Shared::new();
        let mut params = ParamValues::defaults();
        let (mut l, mut r) = (vec![0.0f32; 128], vec![0.0f32; 128]);
        p.process(&shared.input(&params), [], &mut l, &mut r);
        // A parameter change followed by a note at offset 0 of the next call (what nih-plug
        // produces for automation or a preset load right before a note).
        params.set(ParamId::Chip, 2.0);
        params.set(ParamId::UniOn, 0.0);
        p.engine.trace.clear();
        p.process(&shared.input(&params), [on(0, 60), on(64, 62), off(64, 60)], &mut l, &mut r);
        assert_eq!(p.engine().trace, "perpeer");
        for seen in &p.engine().params_at_event {
            assert_eq!(seen.chip(), pixl_core::ChipMode::Free);
            assert!(!seen.bool(ParamId::UniOn));
        }
        // Empty blocks: parameters, then the events.
        params.set(ParamId::Chip, 0.0);
        p.engine.trace.clear();
        p.process(&shared.input(&params), [on(0, 61)], &mut [], &mut []);
        assert_eq!(p.engine().trace, "pe");
        assert_eq!(p.engine().params_at_event.last().map(|v| v.chip()), Some(pixl_core::ChipMode::Nes));
        // Events at the block end: after the last render, with current parameters.
        p.engine.trace.clear();
        p.process(&shared.input(&params), [off(128, 61)], &mut l, &mut r);
        assert_eq!(p.engine().trace, "prpre");
    }

    #[test]
    fn macros_modulate_the_values_sent_to_the_dsp() {
        let mut p = mock();
        let shared = Shared::new();
        let mut routing = MacroAssignments::empty();
        routing.set(0, ParamId::EchoMix, 1.0);
        routing.set(1, ParamId::OutGain, -0.25);
        shared.macros.store(&routing);
        let mut params = ParamValues::defaults();
        params.set(ParamId::Macro1, 50.0);
        params.set(ParamId::Macro2, 100.0);
        params.set(ParamId::OutGain, 0.0);
        let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
        p.process(&shared.input(&params), [on(0, 60)], &mut l, &mut r);
        let eff = p.effective_values();
        // echo_mix 25 % + 0.5 × 100 % → 75 %
        assert!((eff.get(ParamId::EchoMix) - 75.0).abs() < 1e-3);
        // out_gain: normalized 36/48 − 0.25 → 0.5 → −12 dB
        assert!((eff.get(ParamId::OutGain) + 12.0).abs() < 1e-3);
        assert_eq!(p.engine().params_seen.last(), Some(eff));
        assert!((p.fx().gain_db + 12.0).abs() < 1e-3);
        assert!((l[0] - 0.5 * 10f32.powf(-12.0 / 20.0)).abs() < 1e-6);
        // Base values are untouched for non-targets.
        assert_eq!(eff.get(ParamId::GlideTime), params.get(ParamId::GlideTime));
        // Macro routings changed by the GUI are picked up on the next block.
        shared.macros.store(&MacroAssignments::empty());
        p.process(&shared.input(&params), [], &mut l, &mut r);
        assert_eq!(p.effective_values().get(ParamId::EchoMix), 25.0);
    }

    #[test]
    fn wave_updates_are_versioned() {
        let mut p = mock();
        let shared = Shared::new();
        let params = ParamValues::defaults();
        let (mut l, mut r) = (vec![0.0f32; 32], vec![0.0f32; 32]);
        p.process(&shared.input(&params), [], &mut l, &mut r);
        p.process(&shared.input(&params), [], &mut l, &mut r);
        assert_eq!(p.engine().wave_sets, 1);
        assert_eq!(p.engine().wave, Some(wave::default_wave()));
        shared.wave.store(&wave::square());
        p.process(&shared.input(&params), [], &mut l, &mut r);
        assert_eq!(p.engine().wave_sets, 2);
        assert_eq!(p.engine().wave, Some(wave::square()));
        p.reset();
        p.process(&shared.input(&params), [], &mut l, &mut r);
        assert_eq!(p.engine().wave_sets, 3, "wave is re-sent after a reset");
    }

    #[test]
    fn telemetry_and_status() {
        let mut p = mock();
        let shared = Shared::new();
        let params = ParamValues::defaults();
        let (mut l, mut r) = (vec![0.0f32; 128], vec![0.0f32; 128]);
        let s = p.process(&shared.input(&params), [on(5, 72)], &mut l, &mut r);
        assert_eq!(s, BlockStatus::Active);
        let t = *p.telemetry();
        assert_eq!(t.active_voices, 1);
        assert_eq!(t.last_note, Some(72));
        assert_eq!(t.peak_db, [-6.0, pixl_core::bridge::METER_FLOOR_DB]);
        assert_eq!(t.pitch_offset_st, 0.0);
        let s = p.process(&shared.input(&params), [off(0, 72)], &mut l, &mut r);
        assert_eq!(s, BlockStatus::Silent);
        p.fx.tail = 4800;
        let s = p.process(&shared.input(&params), [], &mut l, &mut r);
        assert_eq!(s, BlockStatus::Tail(4800));
    }

    #[test]
    fn non_finite_output_is_caught() {
        let mut p = mock();
        let shared = Shared::new();
        let params = ParamValues::defaults();
        p.engine.poison = true;
        let (mut l, mut r) = (vec![0.0f32; 100], vec![0.0f32; 100]);
        p.process(&shared.input(&params), [on(0, 60)], &mut l, &mut r);
        assert!(l.iter().chain(r.iter()).all(|x| *x == 0.0));
        assert_eq!(p.recoveries(), 2);
        assert!(p.engine().resets >= 2);
    }

    #[test]
    fn mismatched_buffers_and_transport_are_handled() {
        let mut p = mock();
        let shared = Shared::new();
        let params = ParamValues::defaults();
        let (mut l, mut r) = (vec![7.0f32; 100], vec![7.0f32; 60]);
        let mut input = shared.input(&params);
        input.transport = Transport { tempo_bpm: f64::NAN, playing: true, beat_pos: Some(f64::INFINITY) };
        p.process(&input, [on(0, 60)], &mut l, &mut r);
        assert!(l[60..].iter().all(|x| *x == 0.0));
        assert_eq!(p.engine().renders.iter().sum::<usize>(), 60);
        let t = p.engine().transport.unwrap();
        assert_eq!(t.tempo_bpm, 120.0);
        assert_eq!(t.beat_pos, None);
        assert!(t.playing);
        assert_eq!(p.fx().tempo, 120.0);
        assert_eq!(p.telemetry().tempo_bpm, 120.0);
        assert!(p.telemetry().transport_playing);
    }

    #[test]
    fn telemetry_tracks_the_transport_used_for_audio_including_empty_blocks() {
        let mut p = mock();
        let shared = Shared::new();
        let params = ParamValues::defaults();
        let mut input = shared.input(&params);
        input.transport = Transport { tempo_bpm: 137.25, playing: true, beat_pos: Some(8.0) };
        p.process(&input, [on(0, 60)], &mut [0.0; 64], &mut [0.0; 64]);
        assert_eq!(p.telemetry().tempo_bpm, 137.25);
        assert!(p.telemetry().transport_playing);
        assert_eq!(p.telemetry().active_voices, 1);

        // A tempo/stop change is useful to the editor before the next rendered sample.
        // Transport stopped does not imply silence: a played note can remain sounding.
        for (host_bpm, playing, expected_bpm) in [
            (89.5, false, 89.5),
            (10.0, true, 20.0),
            (1_500.0, false, 999.0),
            (0.0, true, 120.0),
            (f64::INFINITY, false, 120.0),
        ] {
            input.transport = Transport { tempo_bpm: host_bpm, playing, beat_pos: None };
            p.process(&input, [], &mut [], &mut []);
            assert_eq!(p.engine().transport.unwrap().tempo_bpm, expected_bpm);
            assert_eq!(p.fx().tempo, expected_bpm);
            assert_eq!(p.telemetry().tempo_bpm, expected_bpm as f32);
            assert_eq!(p.telemetry().transport_playing, playing);
            assert_eq!(p.telemetry().active_voices, 1);
        }
    }

    #[test]
    fn supported_range_matches_the_dsp_crates() {
        assert_eq!(MIN_SAMPLE_RATE, pixl_fx::MIN_SAMPLE_RATE);
        assert_eq!(MAX_SAMPLE_RATE, pixl_fx::MAX_SAMPLE_RATE);
        for sr in [MIN_SAMPLE_RATE, SPEC_MIN_SAMPLE_RATE, 44_100.0, SPEC_MAX_SAMPLE_RATE, MAX_SAMPLE_RATE] {
            let mut p = Processor::with_parts(MockEngine::default(), MockFx::default());
            p.prepare(sr);
            assert!(p.is_rate_supported(), "{sr}");
            assert_eq!(p.engine().prepared, Some((sr, SUB_BLOCK)));
        }
    }

    #[test]
    fn unsupported_sample_rates_are_silent_and_rates_reprepare_only_on_change() {
        let mut p = Processor::with_parts(MockEngine::default(), MockFx::default());
        p.prepare(1_234.5);
        assert!(!p.is_rate_supported());
        let shared = Shared::new();
        let params = ParamValues::defaults();
        let (mut l, mut r) = (vec![3.0f32; 64], vec![3.0f32; 64]);
        let mut input = shared.input(&params);
        input.transport = Transport { tempo_bpm: 97.5, playing: true, beat_pos: None };
        assert_eq!(p.process(&input, [on(0, 60)], &mut l, &mut r), BlockStatus::Silent);
        assert!(l.iter().chain(r.iter()).all(|x| *x == 0.0));
        assert!(p.engine().prepared.is_none());
        assert_eq!(p.telemetry().tempo_bpm, 97.5);
        assert!(p.telemetry().transport_playing);
        p.prepare(48_000.0);
        assert_eq!(p.engine().prepared, Some((48_000.0, SUB_BLOCK)));
        p.engine.prepared = None;
        p.prepare(48_000.0);
        assert!(p.engine().prepared.is_none(), "same rate does not re-prepare");
        p.prepare(f32::NAN);
        assert_eq!(p.sample_rate(), 44_100.0);
    }
}
