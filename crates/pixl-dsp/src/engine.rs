//! The polyphonic engine (SPEC §4.1, §4.4, §4.7, §4.9): performance state, voice allocation,
//! the global frame / control / arpeggiator clocks and chunked rendering into the
//! [`BlipBuffer`].
//!
//! Real-time contract: [`Engine::new`] and [`Engine::prepare`] allocate (voice pool, tick list,
//! impulse buffers; the BLEP kernel is designed once per process on first use). Everything
//! else — `set_params`, `set_wave`, `event`, `render`, `fill_telemetry` — is allocation-,
//! lock- and I/O-free, never panics on any input and always produces finite output.

use crate::blip::{BlipBuffer, Kernel};
use crate::osc::WaveTable;
use crate::resolve::{targets, Resolved, Smoother};
use crate::voice::{Ctx, Kind, ModeCfg, Perf, StartArgs, Tick, Voice, VoiceEnd};
use crate::{EngineEvent, Transport};
use pixl_core::wave::NES_TRIANGLE;
use pixl_core::{GlideMode, ParamValues, TelemetrySnapshot, VoiceMode, WaveRam, PARAMS};

/// Maximum polyphony (the `polyphony` parameter's upper bound).
pub const MAX_POLYPHONY: usize = 16;
/// Voice slots: polyphony plus room for voices fading out after being stolen.
pub const VOICE_POOL: usize = 32;
/// FREE-mode modulation update interval (samples), SPEC §4.4 ("at least every 16 samples").
pub const CONTROL_INTERVAL: u64 = 16;
/// Sample rate used until [`Engine::prepare`] is called.
pub const DEFAULT_SAMPLE_RATE: f32 = 48_000.0;
/// Integrator level below which an engine without voices counts as silent.
pub const SILENCE_THRESHOLD: f32 = 2e-5;
/// Largest render chunk the engine accepts per `prepare` (larger blocks are split).
pub const MAX_CHUNK: usize = 8192;
/// Events buffered between two `set_params`/`render` calls (more are applied immediately).
pub const EVENT_QUEUE: usize = 1024;

/// Diagnostic view of one sounding voice (tests, GUI debugging).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceInfo {
    /// The note that started the voice (the current note for MONO/LEGATO).
    pub note: u8,
    /// Key logically held (pressed or sustained).
    pub gate: bool,
    /// Fading out after being stolen / all-sound-off.
    pub dying: bool,
    /// The HELD arpeggiator's voice.
    pub held_arp: bool,
    /// Start order (larger = more recent).
    pub order: u64,
    /// Unison sub-voices.
    pub sub_voices: usize,
}

/// Global arpeggiator clock: free-running from the first note-on (SPEC §4.6).
#[derive(Clone, Copy, Debug, Default)]
struct ArpClock {
    /// Fractional progress into the current step (time-based clocks).
    phase: f64,
    /// Frames into the current step (chip modes, free rate).
    frames: u32,
    /// Steps since the engine started (voices count relative to their start).
    count: u64,
}

impl ArpClock {
    fn restart(&mut self) {
        self.phase = 0.0;
        self.frames = 0;
    }

    fn advance_time(&mut self, steps: f64) {
        if !(steps.is_finite() && steps > 0.0) {
            return;
        }
        self.phase += steps;
        if self.phase >= 1.0 {
            let whole = self.phase.floor();
            self.count = self.count.wrapping_add(whole.min(1e12) as u64);
            self.phase -= whole;
        }
    }

    fn advance_frame(&mut self, frames_per_step: u32) {
        self.frames += 1;
        if self.frames >= frames_per_step.max(1) {
            self.frames = 0;
            self.count = self.count.wrapping_add(1);
        }
    }
}

/// Everything voices read; kept apart from the voices so both can be borrowed at once.
struct Shared {
    fs: f64,
    params: ParamValues,
    res: Resolved,
    smooth: Smoother,
    perf: Perf,
    transport: Transport,
    held_order: [u8; 128],
    n_order: usize,
    held_sorted: [u8; 128],
    n_sorted: usize,
    wave: WaveTable,
    tri: WaveTable,
    cfg: ModeCfg,
    arp: ArpClock,
    /// Time of the next frame tick, relative to the current chunk start.
    next_frame_t: f64,
    sample_count: u64,
}

impl Shared {
    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            fs: self.fs,
            res: &self.res,
            sm: self.smooth.current(),
            arp_count: self.arp.count,
            perf: self.perf,
            held_sorted: &self.held_sorted[..self.n_sorted],
            held_order: &self.held_order[..self.n_order],
            wave: &self.wave,
            tri: &self.tri,
            cfg: self.cfg,
        }
    }

    /// How far ahead a note event samples its initial values: a note-on starts its own
    /// first frame (chip modes) or control step (FREE) and plays the value at that step's
    /// centre, so a fast attack sounds at once regardless of where the global tick is.
    fn ahead_now(&self) -> f64 {
        if self.cfg.chip.is_chip() {
            self.frame_period() * 0.5
        } else {
            CONTROL_INTERVAL as f64 * 0.5
        }
    }

    fn frame_period(&self) -> f64 {
        self.fs / self.res.frame_hz()
    }

    /// Arp steps per sample for time-based clocks.
    fn arp_steps_per_sample(&self) -> f64 {
        let r = &self.res;
        if r.arp_sync {
            let beats = r.arp_div.beats().max(1e-6);
            self.transport.tempo_bpm / 60.0 / beats / self.fs
        } else {
            r.arp_rate / self.fs
        }
    }

    /// Whether the arp clock counts frames (chip modes with a free rate: an integer number of
    /// frames per step, at least one).
    fn arp_frames_per_step(&self) -> Option<u32> {
        if self.cfg.chip.is_chip() && !self.res.arp_sync {
            let n = (self.res.frame_hz() / self.res.arp_rate.max(1e-6)).round();
            Some(n.clamp(1.0, 1e6) as u32)
        } else {
            None
        }
    }

    fn hold_add(&mut self, note: u8) {
        self.hold_remove(note);
        if self.n_order < 128 {
            self.held_order[self.n_order] = note;
            self.n_order += 1;
        }
        if self.n_sorted < 128 {
            let pos = self.held_sorted[..self.n_sorted].iter().position(|n| *n > note).unwrap_or(self.n_sorted);
            self.held_sorted.copy_within(pos..self.n_sorted, pos + 1);
            self.held_sorted[pos] = note;
            self.n_sorted += 1;
        }
    }

    fn hold_remove(&mut self, note: u8) {
        if let Some(i) = self.held_order[..self.n_order].iter().position(|n| *n == note) {
            self.held_order.copy_within(i + 1..self.n_order, i);
            self.n_order -= 1;
        }
        if let Some(i) = self.held_sorted[..self.n_sorted].iter().position(|n| *n == note) {
            self.held_sorted.copy_within(i + 1..self.n_sorted, i);
            self.n_sorted -= 1;
        }
    }

    fn holds_clear(&mut self) {
        self.n_order = 0;
        self.n_sorted = 0;
    }
}

/// The polyphonic chip voice engine.
pub struct Engine {
    sh: Shared,
    blip: BlipBuffer,
    voices: Vec<Voice>,
    ticks: Vec<Tick>,
    capacity: usize,
    keys: [bool; 128],
    sustained: [bool; 128],
    pedal: bool,
    mono_voice: Option<usize>,
    arp_voice: Option<usize>,
    last_played: Option<u8>,
    order: u64,
    note_counter: u64,
    frame_count: u64,
    wave_dirty: bool,
    /// Events received since the last `set_params`/`render` (applied after the parameters,
    /// so a note at a sub-block start always uses that sub-block's values).
    pending: [EngineEvent; EVENT_QUEUE],
    n_pending: usize,
}

fn sanitize_rate(sample_rate: f32) -> f64 {
    if sample_rate.is_finite() && sample_rate > 0.0 {
        (sample_rate as f64).clamp(8_000.0, 768_000.0)
    } else {
        DEFAULT_SAMPLE_RATE as f64
    }
}

impl Engine {
    /// Allocate an engine able to render blocks of up to `max_block` samples (longer
    /// blocks are rendered in pieces). Designs the BLEP kernel on first use. Allocates; call
    /// off the audio thread.
    pub fn new(max_block: usize) -> Self {
        let _ = Kernel::get();
        let fs = DEFAULT_SAMPLE_RATE as f64;
        let capacity = max_block.clamp(1, MAX_CHUNK);
        let params = ParamValues::defaults();
        let res = Resolved::new(&params, fs);
        let mut smooth = Smoother::new(fs);
        smooth.set_targets(&targets(&params));
        let mut e = Self {
            sh: Shared {
                fs,
                params,
                res,
                smooth,
                perf: Perf::default(),
                transport: Transport::default(),
                held_order: [0; 128],
                n_order: 0,
                held_sorted: [0; 128],
                n_sorted: 0,
                wave: WaveTable::new(&pixl_core::wave::default_wave()),
                tri: WaveTable::new(&NES_TRIANGLE),
                cfg: ModeCfg { chip: res.chip, analog: res.analog },
                arp: ArpClock::default(),
                next_frame_t: 0.0,
                sample_count: 0,
            },
            blip: BlipBuffer::new(capacity),
            voices: vec![Voice::new(); VOICE_POOL],
            ticks: Vec::with_capacity(capacity / CONTROL_INTERVAL as usize + 4),
            capacity,
            keys: [false; 128],
            sustained: [false; 128],
            pedal: false,
            mono_voice: None,
            arp_voice: None,
            last_played: None,
            order: 0,
            note_counter: 0,
            frame_count: 0,
            wave_dirty: false,
            pending: [EngineEvent::AllNotesOff; EVENT_QUEUE],
            n_pending: 0,
        };
        e.reset();
        e
    }

    /// Configure for a sample rate and maximum block size. May allocate. Resets the voices.
    pub fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        let fs = sanitize_rate(sample_rate);
        let capacity = max_block.clamp(1, MAX_CHUNK);
        self.sh.fs = fs;
        if capacity != self.capacity || self.blip.capacity() != capacity {
            self.capacity = capacity;
            self.blip = BlipBuffer::new(capacity);
            self.ticks = Vec::with_capacity(capacity / CONTROL_INTERVAL as usize + 4);
        }
        self.sh.res = Resolved::new(&self.sh.params, fs);
        self.sh.smooth = Smoother::new(fs);
        self.sh.smooth.set_targets(&targets(&self.sh.params));
        self.reset();
    }

    /// Silence all voices and clear internal state. Real-time safe.
    pub fn reset(&mut self) {
        self.blip.clear();
        for v in &mut self.voices {
            *v = Voice::new();
        }
        self.keys = [false; 128];
        self.sustained = [false; 128];
        self.pedal = false;
        self.mono_voice = None;
        self.arp_voice = None;
        self.last_played = None;
        self.order = 0;
        self.note_counter = 0;
        self.frame_count = 0;
        self.wave_dirty = false;
        self.n_pending = 0;
        let sh = &mut self.sh;
        sh.holds_clear();
        sh.perf = Perf::default();
        sh.arp = ArpClock::default();
        sh.next_frame_t = 0.0;
        sh.sample_count = 0;
        sh.smooth.set_targets(&targets(&sh.params));
        sh.smooth.snap();
        sh.cfg = ModeCfg { chip: sh.res.chip, analog: sh.res.analog };
    }

    /// Current sample rate.
    pub fn sample_rate(&self) -> f32 {
        self.sh.fs as f32
    }

    /// Update parameters (effective, macro-applied plain values). Real-time safe.
    ///
    /// Values are sanitized. Continuous values are smoothed; a chip mode or `analog` change
    /// crossfades every sounding voice into the new mode (2 ms out, 2 ms in).
    pub fn set_params(&mut self, params: &ParamValues) {
        let mut p = *params;
        for (v, spec) in p.0.iter_mut().zip(PARAMS.iter()) {
            *v = spec.sanitize(*v);
        }
        let idle = !self.voices.iter().any(|v| v.active);
        let sh = &mut self.sh;
        if p != sh.params {
            sh.params = p;
            sh.res = Resolved::new(&p, sh.fs);
            sh.smooth.set_targets(&targets(&p));
        }
        if idle {
            // Nothing is sounding, so nothing can zipper: take new values immediately (a
            // preset load must not glide the first notes from the previous patch).
            sh.smooth.snap();
        }
        let cfg = ModeCfg { chip: sh.res.chip, analog: sh.res.analog };
        if cfg != sh.cfg {
            sh.cfg = cfg;
            let fs = sh.fs;
            for v in self.voices.iter_mut().filter(|v| v.active) {
                v.begin_switch(fs);
            }
        }
        self.apply_pending();
    }

    /// Update the wave RAM. Real-time safe. Sounding voices pick the change up immediately.
    pub fn set_wave(&mut self, wave: &WaveRam) {
        let tab = WaveTable::new(wave);
        if tab != self.sh.wave {
            self.sh.wave = tab;
            self.wave_dirty = true;
        }
    }

    /// Update transport info. Real-time safe.
    pub fn set_transport(&mut self, transport: Transport) {
        let tempo = if transport.tempo_bpm.is_finite() && transport.tempo_bpm > 0.0 {
            transport.tempo_bpm.clamp(1.0, 2000.0)
        } else {
            120.0
        };
        self.sh.transport = Transport {
            tempo_bpm: tempo,
            playing: transport.playing,
            beat_pos: transport.beat_pos.filter(|b| b.is_finite()),
        };
    }

    /// Apply a performance event at the current position. Real-time safe.
    ///
    /// The event takes effect at the next rendered sample, after the parameters of the next
    /// [`Engine::set_params`] (the caller's per-sub-block call) have been applied, so a note
    /// starting together with a parameter change always uses the new values. Telemetry and
    /// [`Engine::voices`] reflect it from then on.
    pub fn event(&mut self, event: EngineEvent) {
        if self.n_pending == EVENT_QUEUE {
            self.apply_pending();
        }
        self.pending[self.n_pending] = event;
        self.n_pending += 1;
    }

    fn apply_pending(&mut self) {
        let n = self.n_pending;
        self.n_pending = 0;
        for i in 0..n {
            let e = self.pending[i];
            self.apply_event(e);
        }
    }

    fn apply_event(&mut self, event: EngineEvent) {
        let unit = |v: f32, lo: f32| if v.is_finite() { v.clamp(lo, 1.0) } else { 0.0 };
        match event {
            EngineEvent::NoteOn { note, velocity } => self.note_on(note, velocity),
            EngineEvent::NoteOff { note } => self.note_off(note),
            EngineEvent::PitchBend { value } => self.sh.perf.bend = unit(value, -1.0),
            EngineEvent::ModWheel { value } => self.sh.perf.mod_wheel = unit(value, 0.0),
            EngineEvent::Aftertouch { value } => self.sh.perf.aftertouch = unit(value, 0.0),
            EngineEvent::Sustain { on } => self.set_pedal(on),
            EngineEvent::AllNotesOff => self.all_notes_off(),
            EngineEvent::AllSoundOff => self.all_sound_off(),
        }
    }

    // ----- performance -----------------------------------------------------------------

    fn logically_held(&self, note: u8) -> bool {
        let n = note as usize;
        n < 128 && (self.keys[n] || self.sustained[n])
    }

    fn any_held_except(&self, note: u8) -> bool {
        self.sh.held_order[..self.sh.n_order].iter().any(|n| *n != note)
    }

    fn next_seed(&mut self) -> u64 {
        self.note_counter = self.note_counter.wrapping_add(1);
        crate::modulation::mix64(self.note_counter ^ 0x5049_584C_0000_0000)
    }

    fn note_on(&mut self, note: u8, velocity: f32) {
        if note >= 128 {
            return;
        }
        let vel = if velocity.is_finite() { velocity.clamp(0.0, 1.0) } else { 0.0 };
        if vel <= 0.0 {
            self.note_off(note);
            return;
        }
        let others_held = self.any_held_except(note);
        let arp_sounding = self.arp_voice.is_some_and(|i| self.voices[i].active && self.voices[i].gate);
        if !others_held && !arp_sounding {
            self.sh.arp.restart();
        }
        self.keys[note as usize] = true;
        self.sustained[note as usize] = false;
        self.sh.hold_add(note);
        let prev = self.last_played;
        self.last_played = Some(note);
        self.order += 1;
        let seed = self.next_seed();
        let res = self.sh.res;
        let glide_ok = res.glide_on && (res.glide_mode == GlideMode::Always || others_held);
        let args = StartArgs {
            note,
            velocity: vel,
            order: self.order,
            glide_from: None,
            seed,
            kind: Kind::Normal,
            ahead: self.sh.ahead_now(),
        };
        if res.held_arp() {
            self.held_arp_on(args);
            return;
        }
        match res.voice_mode {
            VoiceMode::Poly => self.poly_on(args, prev, glide_ok),
            VoiceMode::Mono | VoiceMode::Legato => self.mono_on(args, prev, others_held, glide_ok),
        }
    }

    fn poly_on(&mut self, mut args: StartArgs, prev: Option<u8>, glide_ok: bool) {
        args.glide_from = if glide_ok { prev.map(|p| p as f64) } else { None };
        let same =
            self.voices.iter().position(|v| v.active && !v.is_dying() && v.kind == Kind::Normal && v.note == args.note);
        let i = match same {
            Some(i) => {
                let ctx = self.sh.ctx();
                self.voices[i].retrigger(&args, &ctx, &mut self.blip);
                return;
            }
            None => self.alloc_voice(self.sh.res.polyphony),
        };
        let ctx = self.sh.ctx();
        self.voices[i].start(&args, &ctx, &mut self.blip);
    }

    fn mono_on(&mut self, mut args: StartArgs, prev: Option<u8>, others_held: bool, glide_ok: bool) {
        let mv = self
            .mono_voice
            .filter(|&i| self.voices[i].active && !self.voices[i].is_dying() && self.voices[i].kind == Kind::Normal);
        let fs = self.sh.fs;
        for (i, v) in self.voices.iter_mut().enumerate() {
            if v.active && !v.is_dying() && Some(i) != mv {
                v.kill(fs);
            }
        }
        match mv {
            Some(i) => {
                let from = if glide_ok { Some(self.voices[i].current_pitch()) } else { None };
                let legato = self.sh.res.voice_mode == VoiceMode::Legato && others_held && self.voices[i].gate;
                let ctx = self.sh.ctx();
                if legato {
                    self.voices[i].legato(args.note, args.order, from, args.ahead, &ctx, &mut self.blip);
                } else {
                    args.glide_from = from;
                    self.voices[i].retrigger(&args, &ctx, &mut self.blip);
                }
            }
            None => {
                args.glide_from = if glide_ok { prev.map(|p| p as f64) } else { None };
                let i = self.alloc_voice(1);
                let ctx = self.sh.ctx();
                self.voices[i].start(&args, &ctx, &mut self.blip);
                self.mono_voice = Some(i);
            }
        }
    }

    fn held_arp_on(&mut self, mut args: StartArgs) {
        args.kind = Kind::HeldArp;
        args.glide_from = None;
        if let Some(i) = self
            .arp_voice
            .filter(|&i| self.voices[i].active && !self.voices[i].is_dying() && self.voices[i].kind == Kind::HeldArp)
        {
            if !self.voices[i].gate {
                // A new phrase while the arp voice is releasing: retrigger it.
                let ctx = self.sh.ctx();
                self.voices[i].retrigger(&args, &ctx, &mut self.blip);
            } else {
                // The new note joins the running sequence (pitch-only stepping); it becomes
                // the voice's reference note for telemetry.
                self.voices[i].note = args.note;
                self.voices[i].order = args.order;
            }
            return;
        }
        let limit = if self.sh.res.voice_mode == VoiceMode::Poly { self.sh.res.polyphony } else { 1 };
        let i = self.alloc_voice(limit);
        let ctx = self.sh.ctx();
        self.voices[i].start(&args, &ctx, &mut self.blip);
        self.arp_voice = Some(i);
        if self.sh.res.voice_mode != VoiceMode::Poly {
            self.mono_voice = None;
        }
    }

    /// Make room for one more voice under `limit` sounding voices and return a free slot.
    fn alloc_voice(&mut self, limit: usize) -> usize {
        let fs = self.sh.fs;
        let on = self.sh.res.on;
        let limit = limit.clamp(1, MAX_POLYPHONY);
        loop {
            let live = self.voices.iter().filter(|v| v.active && !v.is_dying()).count();
            if live < limit {
                break;
            }
            // Steal: the quietest releasing voice, else the oldest.
            let victim = self
                .voices
                .iter()
                .enumerate()
                .filter(|(_, v)| v.is_releasing())
                .min_by(|a, b| a.1.loudness(&on).total_cmp(&b.1.loudness(&on)).then(a.1.order.cmp(&b.1.order)))
                .or_else(|| {
                    self.voices
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| v.active && !v.is_dying())
                        .min_by_key(|(_, v)| v.order)
                })
                .map(|(i, _)| i);
            match victim {
                Some(i) => self.voices[i].kill(fs),
                None => break,
            }
        }
        if let Some(i) = self.voices.iter().position(|v| !v.active && !v.discharging) {
            return i;
        }
        if let Some(i) = self.voices.iter().position(|v| !v.active) {
            return i;
        }
        // Every slot is busy fading out (pathological note spam): recycle the one closest to
        // silence. Its remaining level is removed with a single band-limited step.
        let i = self.voices.iter().enumerate().min_by_key(|(_, v)| v.fade_left()).map_or(0, |(i, _)| i);
        self.hard_stop(i);
        i
    }

    fn hard_stop(&mut self, i: usize) {
        self.voices[i].force_clean(&mut self.blip);
    }

    fn note_off(&mut self, note: u8) {
        if note >= 128 {
            return;
        }
        let n = note as usize;
        if !self.keys[n] && !self.sustained[n] {
            return;
        }
        self.keys[n] = false;
        if self.pedal {
            self.sustained[n] = true;
            return;
        }
        self.release_note(note);
    }

    fn release_note(&mut self, note: u8) {
        self.sustained[note as usize] = false;
        self.sh.hold_remove(note);
        let res = self.sh.res;
        let fs = self.sh.fs;
        // HELD arpeggiator voice: releases once no note is held any more.
        if let Some(i) = self
            .arp_voice
            .filter(|&i| self.voices[i].active && self.voices[i].kind == Kind::HeldArp && self.voices[i].gate)
        {
            if self.sh.n_order == 0 {
                self.voices[i].release(&res, fs);
            }
        }
        let mono = res.voice_mode != VoiceMode::Poly;
        let mv = self
            .mono_voice
            .filter(|&i| self.voices[i].active && self.voices[i].gate && self.voices[i].kind == Kind::Normal);
        if mono {
            if let Some(i) = mv.filter(|&i| self.voices[i].note == note) {
                if let Some(&top) = self.sh.held_order[..self.sh.n_order].last() {
                    // Return to the most recent still-held note (MONO retriggers, LEGATO not).
                    let glide_ok = res.glide_on;
                    let from = if glide_ok { Some(self.voices[i].current_pitch()) } else { None };
                    self.order += 1;
                    let ahead = self.sh.ahead_now();
                    if res.voice_mode == VoiceMode::Legato {
                        let ctx = self.sh.ctx();
                        self.voices[i].legato(top, self.order, from, ahead, &ctx, &mut self.blip);
                    } else {
                        let seed = self.next_seed();
                        let args = StartArgs {
                            note: top,
                            velocity: self.voices[i].velocity,
                            order: self.order,
                            glide_from: from,
                            seed,
                            kind: Kind::Normal,
                            ahead,
                        };
                        let ctx = self.sh.ctx();
                        self.voices[i].retrigger(&args, &ctx, &mut self.blip);
                    }
                } else {
                    self.voices[i].release(&res, fs);
                }
            }
        }
        for (i, v) in self.voices.iter_mut().enumerate() {
            if v.active && v.gate && v.kind == Kind::Normal && v.note == note && !(mono && Some(i) == mv) {
                v.release(&res, fs);
            }
        }
    }

    fn set_pedal(&mut self, on: bool) {
        self.pedal = on;
        if !on {
            for n in 0..128u8 {
                if self.sustained[n as usize] && !self.keys[n as usize] {
                    self.release_note(n);
                }
            }
        }
    }

    fn all_notes_off(&mut self) {
        self.keys = [false; 128];
        self.sustained = [false; 128];
        self.sh.holds_clear();
        let res = self.sh.res;
        let fs = self.sh.fs;
        for v in &mut self.voices {
            if v.active && v.gate {
                v.release(&res, fs);
            }
        }
    }

    fn all_sound_off(&mut self) {
        self.all_notes_off();
        let fs = self.sh.fs;
        for v in self.voices.iter_mut().filter(|v| v.active) {
            v.kill(fs);
        }
        self.mono_voice = None;
        self.arp_voice = None;
    }

    // ----- rendering -------------------------------------------------------------------

    /// Render `out_l.len()` samples (≤ max_block; longer buffers are rendered in pieces),
    /// overwriting both buffers. Real-time safe.
    pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        self.apply_pending();
        let len = out_l.len().min(out_r.len());
        out_l[len..].fill(0.0);
        out_r[len..].fill(0.0);
        let mut pos = 0;
        while pos < len {
            let n = (len - pos).min(self.capacity);
            self.render_chunk(&mut out_l[pos..pos + n], &mut out_r[pos..pos + n]);
            pos += n;
        }
    }

    fn render_chunk(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let len = out_l.len();
        let lenf = len as f64;

        if self.wave_dirty {
            self.wave_dirty = false;
            let ctx = self.sh.ctx();
            for v in self.voices.iter_mut().filter(|v| v.active) {
                v.resync_wave(&ctx, &mut self.blip);
            }
        }

        // Chunk-start snapshot for voices (events and the first segment).
        let sm0 = *self.sh.smooth.current();
        let arp0 = self.sh.arp.count;

        // Modulation ticks with the global values at each of them.
        self.ticks.clear();
        let chip = self.sh.cfg.chip.is_chip();
        let period = self.sh.frame_period().max(1.0);
        let frames_per_step = self.sh.arp_frames_per_step();
        let sps = self.sh.arp_steps_per_sample();
        let mut t_last = 0.0f64;
        let mut ft = self.sh.next_frame_t.max(0.0);
        let mut ct = ((CONTROL_INTERVAL - self.sh.sample_count % CONTROL_INTERVAL) % CONTROL_INTERVAL) as f64;
        loop {
            // Next tick: frame ticks drive modulation in chip modes, control ticks in FREE;
            // the frame clock always runs (telemetry counter).
            let t = if chip { ft } else { ct.min(ft) };
            if t >= lenf {
                break;
            }
            let is_frame = t == ft;
            let is_mod = if chip { is_frame } else { t == ct };
            if is_frame {
                ft += period;
                self.frame_count = self.frame_count.wrapping_add(1);
            }
            if !chip && t == ct {
                ct += CONTROL_INTERVAL as f64;
            }
            if !is_mod {
                continue;
            }
            self.sh.smooth.advance(t - t_last);
            match frames_per_step {
                Some(n) => self.sh.arp.advance_frame(n),
                None => self.sh.arp.advance_time((t - t_last) * sps),
            }
            t_last = t;
            let ahead = if chip { period * 0.5 } else { CONTROL_INTERVAL as f64 * 0.5 };
            if self.ticks.len() < self.ticks.capacity() {
                self.ticks.push(Tick { t, ahead, arp_count: self.sh.arp.count, sm: *self.sh.smooth.current() });
            }
        }
        // Frame ticks beyond the modulation loop (FREE mode counts frames too).
        while ft < lenf {
            ft += period;
            self.frame_count = self.frame_count.wrapping_add(1);
        }

        {
            let sh = &self.sh;
            let base = Ctx {
                fs: sh.fs,
                res: &sh.res,
                sm: &sm0,
                arp_count: arp0,
                perf: sh.perf,
                held_sorted: &sh.held_sorted[..sh.n_sorted],
                held_order: &sh.held_order[..sh.n_order],
                wave: &sh.wave,
                tri: &sh.tri,
                cfg: sh.cfg,
            };
            for v in self.voices.iter_mut() {
                if v.active {
                    match v.render(&base, &self.ticks, lenf, &mut self.blip) {
                        VoiceEnd::Running | VoiceEnd::Finished | VoiceEnd::Killed => {}
                    }
                } else if v.discharging {
                    v.discharge_run(len, sh.fs, &mut self.blip);
                }
            }
        }

        self.blip.read(out_l, out_r);

        // Advance the global clocks to the chunk end and move the time origin.
        self.sh.smooth.advance(lenf - t_last);
        if frames_per_step.is_none() {
            self.sh.arp.advance_time((lenf - t_last) * sps);
        }
        self.sh.next_frame_t = ft - lenf;
        self.sh.sample_count = self.sh.sample_count.wrapping_add(len as u64);
        for v in &mut self.voices {
            v.shift_time(lenf);
        }
        if self.mono_voice.is_some_and(|i| !self.voices[i].active) {
            self.mono_voice = None;
        }
        if self.arp_voice.is_some_and(|i| !self.voices[i].active) {
            self.arp_voice = None;
        }
        if !self.voices.iter().any(|v| v.active || v.discharging) && self.blip.is_quiet(SILENCE_THRESHOLD) {
            self.blip.flush();
            // Exact digital silence once everything has decayed.
            if out_l.iter().chain(out_r.iter()).all(|x| x.abs() < SILENCE_THRESHOLD) {
                out_l.fill(0.0);
                out_r.fill(0.0);
            }
        }
    }

    // ----- telemetry -------------------------------------------------------------------

    /// Number of sounding voices (including releasing and fading ones).
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// The sounding voices, in slot order.
    pub fn voices(&self) -> impl Iterator<Item = VoiceInfo> + '_ {
        self.voices.iter().filter(|v| v.active).map(|v| VoiceInfo {
            note: v.note,
            gate: v.gate,
            dying: v.is_dying(),
            held_arp: v.kind == Kind::HeldArp,
            order: v.order,
            sub_voices: v.sub_count(),
        })
    }

    /// Fill the voice-related telemetry fields (everything except the meters).
    pub fn fill_telemetry(&self, t: &mut TelemetrySnapshot) {
        t.active_voices = self.active_voices().min(255) as u8;
        let mut held = [0u64; 2];
        for (n, k) in self.keys.iter().enumerate() {
            if *k || self.sustained[n] {
                held[n / 64] |= 1 << (n % 64);
            }
        }
        t.held_notes = held;
        t.frame = self.frame_count;
        let recent = self.voices.iter().filter(|v| v.active).max_by_key(|v| (!v.is_dying(), v.order));
        match recent {
            Some(v) => {
                t.last_note = Some(v.note);
                t.lfsr = v.lfsr() & 0x7fff;
                t.wave_step = Some(v.wave_step().min(31));
                t.env_level = v.env_levels();
                t.pitch_offset_st = if v.pitch_offset().is_finite() { v.pitch_offset() } else { 0.0 };
                t.arp_step = v.arp_step();
            }
            None => {
                t.last_note = None;
                t.wave_step = None;
                t.env_level = [0.0; 4];
                t.pitch_offset_st = 0.0;
                t.arp_step = None;
            }
        }
    }

    /// True when no voice is sounding and the output has decayed to silence.
    pub fn is_silent(&self) -> bool {
        self.n_pending == 0
            && !self.voices.iter().any(|v| v.active || v.discharging)
            && self.blip.is_quiet(SILENCE_THRESHOLD)
    }

    /// Whether `note` is logically held (key down or sustained by the pedal).
    pub fn is_note_held(&self, note: u8) -> bool {
        self.logically_held(note)
    }
}
