//! Event-driven chip oscillators (SPEC §4.2, §4.3).
//!
//! Every chip channel is a piecewise-constant signal. Instead of computing samples, each
//! oscillator knows the exact (fractional) time of its next output change ([`PulseOsc::next_t`]
//! etc.). The voice renders a segment by repeatedly committing the earliest change and
//! handing the resulting delta to the [`BlipBuffer`], which band-limits it.
//!
//! Oscillators produce *unit values* `u` (pulse bit 0/1, 4-bit wave sample after the volume
//! shift, noise bit 0/1); the voice maps them to DAC values (`dac = u·g + o`) and mixes.
//!
//! Times are in samples relative to the start of the current render chunk (`f64`); after each
//! chunk the engine calls `shift_time(len)` so values stay small and precise forever.
//!
//! Pitch changes keep phase continuity: [`PulseOsc::configure`] (and the wave/noise
//! equivalents) first advance the phase to the change time with the old rate, then continue
//! with the new rate.

use crate::blip::BlipBuffer;
use pixl_core::{WaveRam, WAVE_LEN};

/// "Never": the time of the next change of a halted oscillator.
pub const NEVER: f64 = f64::INFINITY;

/// Noise clocks per output sample above which the noise channel switches to
/// integrate-and-dump (≈ 4 output transitions per sample, SPEC §4.2).
pub const ID_ENTER: f64 = 8.0;
/// Clocks per sample below which an integrate-and-dump noise channel returns to BLEP
/// rendering (hysteresis against toggling at the threshold).
pub const ID_LEAVE: f64 = 7.0;

/// Output transitions per sample above which the wave channel switches to integrate-and-dump
/// (SPEC §4.2: "more than ~4 transitions per output sample"). Counted from the table: a step
/// rate of `inc` steps per sample makes `inc · changes / 32` transitions per sample.
pub const WAVE_ID_ENTER: f64 = 4.0;
/// Transitions per sample below which an integrating wave channel returns to BLEP rendering.
pub const WAVE_ID_LEAVE: f64 = 3.5;

/// Longest LFSR run searched for the next output change before giving up (a valid, non-zero
/// LFSR state changes its output bit within 15 clocks).
const LFSR_SEARCH: u32 = 64;

#[inline]
fn wrap(x: f64, period: f64) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    let r = x - (x / period).floor() * period;
    if r >= period || r < 0.0 {
        0.0
    } else {
        r
    }
}

#[inline]
fn sane_inc(inc: f64) -> f64 {
    if inc.is_finite() && inc > 0.0 {
        inc
    } else {
        0.0
    }
}

/// Smallest increment treated as running (below it an oscillator is effectively halted).
const MIN_INC: f64 = 1e-12;

// ---------------------------------------------------------------------------------------
// Pulse
// ---------------------------------------------------------------------------------------

/// Pulse oscillator with one rising and one falling edge per cycle.
///
/// Chip duties are 8-step sequences, but each has exactly one rising and one falling edge
/// (see [`crate::chip::NES_EDGES`]), so the same edge-based oscillator renders the chip
/// sequences (edges on 1/8 positions) and the FREE-mode continuous PWM (edges at 0 and duty).
#[derive(Clone, Copy, Debug)]
pub struct PulseOsc {
    /// Cycle position 0..1 at `t_pos`.
    pos: f64,
    t_pos: f64,
    /// Cycles per sample.
    inc: f64,
    inv_inc: f64,
    rise: f64,
    fall: f64,
    high: bool,
    running: bool,
    u: f32,
    next_t: f64,
}

impl Default for PulseOsc {
    fn default() -> Self {
        Self::new()
    }
}

impl PulseOsc {
    /// A halted oscillator at phase 0 (output low).
    pub const fn new() -> Self {
        Self {
            pos: 0.0,
            t_pos: 0.0,
            inc: 0.0,
            inv_inc: 0.0,
            rise: 0.0,
            fall: 0.5,
            high: false,
            running: false,
            u: 0.0,
            next_t: NEVER,
        }
    }

    /// Current unit value (0/1, or the static value while ultrasonic/muted).
    #[inline]
    pub fn u(&self) -> f32 {
        self.u
    }

    /// Time of the next output change.
    #[inline]
    pub fn next_t(&self) -> f64 {
        self.next_t
    }

    /// Cycle position (0..1) at the last update.
    pub fn phase(&self) -> f64 {
        self.pos
    }

    /// Mean unit value over a cycle: the duty while running, else the held value.
    pub fn mean_unit(&self) -> f32 {
        if self.running {
            wrap(self.fall - self.rise, 1.0) as f32
        } else {
            self.u
        }
    }

    /// Set the cycle position at time `t` (fresh voices). The output is recomputed.
    pub fn set_phase(&mut self, pos: f64, t: f64) {
        self.pos = wrap(pos, 1.0);
        self.t_pos = t;
        if self.running {
            self.high = self.in_high(self.pos);
            self.u = self.high as u8 as f32;
            self.schedule();
        }
    }

    #[inline]
    fn in_high(&self, pos: f64) -> bool {
        if self.rise < self.fall {
            pos >= self.rise && pos < self.fall
        } else {
            pos >= self.rise || pos < self.fall
        }
    }

    #[inline]
    fn schedule(&mut self) {
        if !self.running {
            self.next_t = NEVER;
            return;
        }
        let edge = if self.high { self.fall } else { self.rise };
        let mut d = edge - self.pos;
        if d <= 0.0 {
            d += 1.0;
        }
        self.next_t = self.t_pos + d * self.inv_inc;
    }

    /// Re-anchor at time `t` with a new rate (cycles/sample) and edges. `stat` holds the
    /// output at a constant unit value instead (ultrasonic pitch → cycle mean, or muted).
    pub fn configure(&mut self, t: f64, inc: f64, rise: f64, fall: f64, stat: Option<f32>) {
        let dt = (t - self.t_pos).max(0.0);
        self.pos = wrap(self.pos + dt * self.inc, 1.0);
        self.t_pos = t;
        self.inc = sane_inc(inc);
        self.inv_inc = if self.inc > MIN_INC { 1.0 / self.inc } else { 0.0 };
        self.rise = wrap(rise, 1.0);
        self.fall = wrap(fall, 1.0);
        match stat {
            Some(v) => {
                self.running = false;
                self.u = if v.is_finite() { v } else { 0.0 };
                self.next_t = NEVER;
            }
            None => {
                self.running = self.inc > MIN_INC;
                self.high = self.in_high(self.pos);
                self.u = self.high as u8 as f32;
                self.schedule();
            }
        }
    }

    /// Apply the pending edge (at [`Self::next_t`]); returns the change of the unit value.
    #[inline]
    pub fn commit(&mut self) -> f32 {
        let edge = if self.high { self.fall } else { self.rise };
        self.pos = edge;
        self.t_pos = self.next_t;
        self.high = !self.high;
        let nu = self.high as u8 as f32;
        let du = nu - self.u;
        self.u = nu;
        self.schedule();
        du
    }

    /// Render all edges before `t_end` with per-unit gains `al`/`ar`.
    #[inline]
    pub fn run(&mut self, t_end: f64, al: f32, ar: f32, blip: &mut BlipBuffer) {
        while self.next_t < t_end {
            let t = self.next_t;
            let du = self.commit();
            blip.add_step(t, du * al, du * ar);
        }
    }

    /// Shift the time origin by `dt` samples (end of a render chunk).
    #[inline]
    pub fn shift_time(&mut self, dt: f64) {
        self.t_pos -= dt;
        self.next_t -= dt;
    }
}

// ---------------------------------------------------------------------------------------
// Wave
// ---------------------------------------------------------------------------------------

/// A 32-step wave with precomputed "distance to the next different step" tables for the
/// three Game Boy volume shifts, so equal neighbouring steps cost nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveTable {
    values: [u8; WAVE_LEN],
    /// `next[shift][step]`: steps (1..=32) until the shifted value changes; 0 = constant wave.
    next: [[u8; WAVE_LEN]; 3],
    /// Mean of the shifted values (the band-limited output of an ultrasonic wave).
    mean: [f32; 3],
    /// `prefix[shift][k]`: sum of the first `k` shifted values (integrate-and-dump).
    prefix: [[f32; WAVE_LEN + 1]; 3],
    /// Number of steps whose shifted value differs from the next one (transitions per cycle).
    changes: [u8; 3],
}

impl WaveTable {
    /// Build the tables for a wave (values above 15 are clamped).
    pub fn new(wave: &WaveRam) -> Self {
        let mut values = [0u8; WAVE_LEN];
        for (d, s) in values.iter_mut().zip(wave.iter()) {
            *d = (*s).min(15);
        }
        let mut next = [[0u8; WAVE_LEN]; 3];
        let mut mean = [0.0f32; 3];
        let mut prefix = [[0.0f32; WAVE_LEN + 1]; 3];
        let mut changes = [0u8; 3];
        for shift in 0..3 {
            let v = |i: usize| values[i % WAVE_LEN] >> shift;
            for (step, n) in next[shift].iter_mut().enumerate() {
                *n = (1..=WAVE_LEN).find(|d| v(step + d) != v(step)).unwrap_or(0) as u8;
            }
            mean[shift] = (0..WAVE_LEN).map(|i| v(i) as f32).sum::<f32>() / WAVE_LEN as f32;
            for k in 0..WAVE_LEN {
                prefix[shift][k + 1] = prefix[shift][k] + v(k) as f32;
            }
            changes[shift] = (0..WAVE_LEN).filter(|i| v(*i) != v(i + 1)).count() as u8;
        }
        Self { values, next, mean, prefix, changes }
    }

    /// Integral of the shifted wave over step positions `0..p` (`p ≥ 0`, any number of
    /// cycles): whole cycles, whole steps and the partial step.
    #[inline]
    fn integral(&self, p: f64, shift: u8) -> f64 {
        let sh = (shift as usize).min(2);
        let p = if p.is_finite() { p.max(0.0) } else { 0.0 };
        let cycles = (p / WAVE_LEN as f64).floor();
        let r = p - cycles * WAVE_LEN as f64;
        let k = (r as usize).min(WAVE_LEN - 1);
        let whole = self.prefix[sh][WAVE_LEN] as f64;
        cycles * whole + self.prefix[sh][k] as f64 + (r - k as f64) * self.value(k, shift) as f64
    }

    /// Output transitions per cycle of the shifted wave.
    #[inline]
    pub fn changes(&self, shift: u8) -> u8 {
        self.changes[(shift as usize).min(2)]
    }

    /// Shifted 4-bit value of a step.
    #[inline]
    pub fn value(&self, step: usize, shift: u8) -> f32 {
        (self.values[step % WAVE_LEN] >> shift.min(3)) as f32
    }

    /// Raw 4-bit value of a step.
    #[inline]
    pub fn raw(&self, step: usize) -> u8 {
        self.values[step % WAVE_LEN]
    }

    /// Mean of the shifted values.
    #[inline]
    pub fn mean(&self, shift: u8) -> f32 {
        self.mean[(shift as usize).min(2)]
    }

    #[inline]
    fn next_change(&self, step: usize, shift: u8) -> u8 {
        self.next[(shift as usize).min(2)][step % WAVE_LEN]
    }
}

/// How a wave oscillator advances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WaveMode {
    /// Step through the table.
    Run,
    /// Halt on the current step (NES triangle gated off or ultrasonic, SPEC §4.3).
    Hold,
    /// Output a constant unit value (ultrasonic wave → its mean).
    Static(f32),
}

/// 32-step wave / NES triangle sequencer.
#[derive(Clone, Copy, Debug)]
pub struct WaveOsc {
    /// Step position 0..32 at `t_pos`.
    pos: f64,
    t_pos: f64,
    /// Steps per sample.
    inc: f64,
    inv_inc: f64,
    step: u8,
    shift: u8,
    mode: WaveMode,
    u: f32,
    next_t: f64,
    next_step: u8,
    // integrate-and-dump (SPEC §4.2), like the noise channel
    id: bool,
    acc: f64,
    t_acc: f64,
    next_n: f64,
    shown: f32,
}

impl Default for WaveOsc {
    fn default() -> Self {
        Self::new()
    }
}

impl WaveOsc {
    /// Step index of the NES triangle where its output is 0 at the start of the rising half;
    /// clean voices start here with the unipolar DAC so the first note never jumps the DAC.
    pub const TRIANGLE_REST_STEP: u8 = 16;
    /// Rest step for the centred (bipolar) DAC: value 8 on the rising half, the step closest
    /// to the centre 7.5.
    pub const TRIANGLE_REST_STEP_BIPOLAR: u8 = 24;

    /// Rest step of the NES triangle for a DAC polarity.
    pub fn triangle_rest_step(unipolar: bool) -> u8 {
        if unipolar {
            Self::TRIANGLE_REST_STEP
        } else {
            Self::TRIANGLE_REST_STEP_BIPOLAR
        }
    }

    /// A held oscillator at step 0.
    pub const fn new() -> Self {
        Self {
            pos: 0.0,
            t_pos: 0.0,
            inc: 0.0,
            inv_inc: 0.0,
            step: 0,
            shift: 0,
            mode: WaveMode::Hold,
            u: 0.0,
            next_t: NEVER,
            next_step: 0,
            id: false,
            acc: 0.0,
            t_acc: 0.0,
            next_n: 1.0,
            shown: 0.0,
        }
    }

    /// The unit value the mixer currently sees: the instantaneous step value, or the last
    /// interval average while integrating.
    #[inline]
    pub fn u(&self) -> f32 {
        if self.id {
            self.shown
        } else {
            self.u
        }
    }

    /// Whether the channel currently renders with integrate-and-dump.
    #[inline]
    pub fn is_integrating(&self) -> bool {
        self.id
    }

    /// Next sample boundary at which an integrating channel emits.
    #[inline]
    pub fn next_boundary(&self) -> f64 {
        self.next_n
    }

    /// Time of the next output change.
    #[inline]
    pub fn next_t(&self) -> f64 {
        self.next_t
    }

    /// Current step 0..32.
    #[inline]
    pub fn step(&self) -> u8 {
        self.step
    }

    /// Position (steps) at the last update.
    pub fn position(&self) -> f64 {
        self.pos
    }

    /// Mean unit value over a cycle of `tab`: the table mean while stepping, else the held or
    /// static value.
    pub fn mean_unit(&self, tab: &WaveTable) -> f32 {
        match self.mode {
            WaveMode::Run if self.inc > MIN_INC => tab.mean(self.shift),
            _ => self.u,
        }
    }

    /// Park the sequencer on `step` (held, silent until the next `configure`). Used when a
    /// voice slot is cleaned: the NES triangle then restarts from its zero step.
    pub fn park(&mut self, step: u8) {
        self.step = step % WAVE_LEN as u8;
        self.pos = self.step as f64;
        self.t_pos = 0.0;
        self.mode = WaveMode::Hold;
        self.u = 0.0;
        self.next_t = NEVER;
        self.id = false;
        self.shown = 0.0;
    }

    /// Set the step position at time `t` without changing the mode. Callers re-`configure`
    /// afterwards to refresh the output.
    pub fn set_position(&mut self, pos: f64, t: f64) {
        self.pos = wrap(pos, WAVE_LEN as f64);
        self.step = (self.pos as usize).min(WAVE_LEN - 1) as u8;
        self.t_pos = t;
    }

    #[inline]
    fn schedule(&mut self, tab: &WaveTable) {
        if self.mode != WaveMode::Run || self.inc <= MIN_INC {
            self.next_t = NEVER;
            return;
        }
        let d = tab.next_change(self.step as usize, self.shift);
        if d == 0 {
            self.next_t = NEVER;
            return;
        }
        let edge = self.step as f64 + d as f64;
        self.next_t = self.t_pos + (edge - self.pos).max(0.0) * self.inv_inc;
        self.next_step = ((self.step as usize + d as usize) % WAVE_LEN) as u8;
    }

    /// Re-anchor at time `t` with a new rate (steps/sample), volume shift and mode. An
    /// integrating channel is first integrated up to `t` with the old settings.
    pub fn configure(&mut self, t: f64, inc: f64, shift: u8, mode: WaveMode, tab: &WaveTable) {
        if self.id {
            self.sync_window(t, tab);
        }
        // A delayed start ([`Self::delay_start`]) still pending: the sequencer keeps waiting on
        // its step until then (at the new rate afterwards).
        let waiting = self.mode == WaveMode::Run && mode == WaveMode::Run && self.t_pos > t;
        let dt = (t - self.t_pos).max(0.0);
        if self.mode != WaveMode::Hold {
            // The sequencer was moving: latch the step it has reached.
            self.pos = wrap(self.pos + dt * self.inc, WAVE_LEN as f64);
            self.step = (self.pos as usize).min(WAVE_LEN - 1) as u8;
        }
        if !waiting {
            self.t_pos = t;
        }
        self.inc = sane_inc(inc);
        self.inv_inc = if self.inc > MIN_INC { 1.0 / self.inc } else { 0.0 };
        self.shift = shift.min(2);
        self.mode = mode;
        match mode {
            WaveMode::Run | WaveMode::Hold => {
                self.u = tab.value(self.step as usize, self.shift);
                self.schedule(tab);
            }
            WaveMode::Static(v) => {
                self.u = if v.is_finite() { v } else { 0.0 };
                self.next_t = NEVER;
            }
        }
        // Integrate-and-dump while the wave makes more than ~4 transitions per sample.
        let per_sample = self.inc * tab.changes(self.shift) as f64 / WAVE_LEN as f64;
        let want_id =
            mode == WaveMode::Run && if self.id { per_sample >= WAVE_ID_LEAVE } else { per_sample > WAVE_ID_ENTER };
        if want_id {
            self.next_t = NEVER;
            if !self.id {
                // The mixer keeps seeing the current step until the first window closes.
                self.id = true;
                self.shown = self.u;
                self.restart_window(t);
                self.acc = self.u as f64 * (self.t_acc - (self.next_n - 1.0));
            }
        } else if self.id {
            // Back to stepping: the mixer sees the instantaneous value again (the caller emits
            // the change of its total).
            self.id = false;
        }
    }

    /// Step position at time `t` (unwrapped; constant before a delayed start).
    #[inline]
    fn position_at(&self, t: f64) -> f64 {
        self.pos + (t - self.t_pos).max(0.0) * self.inc
    }

    /// Integrate-and-dump: add the integral of the output from `t_acc` to `to`.
    #[inline]
    fn advance_id(&mut self, to: f64, tab: &WaveTable) {
        let from = self.t_acc;
        // `partial_cmp` keeps NaN times out: anything but a strictly later `to` is a no-op.
        if to.partial_cmp(&from) != Some(std::cmp::Ordering::Greater) {
            return;
        }
        // Waiting for a delayed start: the current step's value until `t_pos`.
        let wait = (to.min(self.t_pos) - from).max(0.0);
        let mut area = wait * self.u as f64;
        let run_from = from.max(self.t_pos);
        if to > run_from && self.inc > MIN_INC {
            let (p0, p1) = (self.position_at(run_from), self.position_at(to));
            area += (tab.integral(p1, self.shift) - tab.integral(p0, self.shift)) * self.inv_inc;
        }
        self.acc += area;
        self.t_acc = to;
    }

    /// Integrate-and-dump: close the window ending at [`Self::next_boundary`]; returns the
    /// change of the emitted value.
    #[inline]
    pub fn boundary(&mut self, tab: &WaveTable) -> f32 {
        let n = self.next_n;
        self.advance_id(n, tab);
        let avg = self.acc.clamp(0.0, 15.0) as f32;
        self.acc = 0.0;
        self.next_n = n + 1.0;
        let d = avg - self.shown;
        self.shown = avg;
        d
    }

    /// Integrate-and-dump: integrate the partial window up to `t_end` (no emission).
    #[inline]
    pub fn finish(&mut self, t_end: f64, tab: &WaveTable) {
        if self.id {
            self.advance_id(t_end, tab);
        }
    }

    /// Integrate-and-dump: a fresh window at `t` (nothing accumulated, the next boundary is
    /// the next sample boundary). The emitted value is unchanged.
    #[inline]
    fn restart_window(&mut self, t: f64) {
        self.acc = 0.0;
        self.t_acc = if t.is_finite() { t } else { 0.0 };
        self.next_n = self.t_acc.floor() + 1.0;
    }

    /// Integrate-and-dump: bring the window to `t` — integrate the rendered partial window,
    /// or restart it if the channel was not rendered (an idle voice slot whose times kept
    /// shifting): bounded work however long the gap (SPEC §0).
    fn sync_window(&mut self, t: f64, tab: &WaveTable) {
        let valid = self.t_acc.is_finite()
            && self.t_acc <= t
            && self.t_acc >= t - 1.0
            && self.next_n >= self.t_acc
            && self.next_n - 1.0 <= self.t_acc;
        if valid && t <= self.next_n {
            self.advance_id(t, tab);
        } else {
            self.restart_window(t);
        }
    }

    /// Delayed start: a running sequencer waits on its current step until time `t` and only
    /// then starts stepping (a later `configure` keeps the wait; `set_position`, a halt or a
    /// static mode cancel it). Unison layers of the volume-less NES triangle use it to start at
    /// a random phase without a DAC jump (SPEC §4.3/§4.7). Ignored unless running.
    pub fn delay_start(&mut self, t: f64, tab: &WaveTable) {
        if self.mode == WaveMode::Run && t.is_finite() && t > self.t_pos {
            self.t_pos = t;
            if !self.id {
                self.schedule(tab);
            }
        }
    }

    /// Re-read the current step from (possibly changed) table data and reschedule.
    pub fn resync(&mut self, t: f64, tab: &WaveTable) {
        let (inc, shift, mode) = (self.inc, self.shift, self.mode);
        if let WaveMode::Static(_) = mode {
            return;
        }
        self.configure(t, inc, shift, mode, tab);
    }

    /// Apply the pending step change; returns the change of the unit value.
    #[inline]
    pub fn commit(&mut self, tab: &WaveTable) -> f32 {
        self.step = self.next_step;
        self.pos = self.step as f64;
        self.t_pos = self.next_t;
        let nu = tab.value(self.step as usize, self.shift);
        let du = nu - self.u;
        self.u = nu;
        self.schedule(tab);
        du
    }

    /// Render everything before `t_end` (band-limited steps, or integrate-and-dump windows).
    #[inline]
    pub fn run(&mut self, t_end: f64, al: f32, ar: f32, tab: &WaveTable, blip: &mut BlipBuffer) {
        if self.id {
            if !(self.next_n >= self.t_acc && self.next_n - 1.0 <= self.t_acc && self.t_acc >= -1.0) {
                // Defensive, like the noise channel: never walk a stale window sample by sample.
                self.restart_window(self.t_acc.clamp(0.0, t_end.max(0.0)));
            }
            while self.next_n < t_end {
                let n = self.next_n;
                let d = self.boundary(tab);
                if d != 0.0 {
                    blip.add_raw(n.max(0.0) as usize, d * al, d * ar);
                }
            }
            self.advance_id(t_end, tab);
            return;
        }
        while self.next_t < t_end {
            let t = self.next_t;
            let du = self.commit(tab);
            if du != 0.0 {
                blip.add_step(t, du * al, du * ar);
            }
        }
    }

    /// Shift the time origin by `dt` samples.
    #[inline]
    pub fn shift_time(&mut self, dt: f64) {
        self.t_pos -= dt;
        self.next_t -= dt;
        self.t_acc -= dt;
        self.next_n -= dt;
    }
}

// ---------------------------------------------------------------------------------------
// Noise
// ---------------------------------------------------------------------------------------

/// Feedback tap and "also write bit 6" flag of an LFSR variant: NES long `bit0^bit1`, NES short
/// `bit0^bit6`, GB `bit0^bit1` (short mode also writes the feedback into bit 6).
#[inline]
pub fn lfsr_kind(nes: bool, short: bool) -> (u32, bool) {
    match (nes, short) {
        (true, false) => (1, false),
        (true, true) => (6, false),
        (false, false) => (1, false),
        (false, true) => (1, true),
    }
}

/// One clock of a 15-bit LFSR variant (see [`lfsr_kind`]); equivalent to
/// [`crate::chip::nes_lfsr_step`] / [`crate::chip::gb_lfsr_step`].
#[inline(always)]
pub fn lfsr_step(s: u16, tap: u32, bit6: bool) -> u16 {
    let x = (s ^ (s >> tap)) & 1;
    let mut n = (s >> 1) | (x << 14);
    if bit6 {
        n = (n & !(1 << 6)) | (x << 6);
    }
    n & 0x7fff
}

/// 15-bit LFSR noise (NES or Game Boy feedback) with exact clock timing.
///
/// The oscillator looks ahead to the clock at which output bit 0 next changes, so only real
/// output transitions cost a BLEP. Above [`ID_ENTER`] clocks per sample it switches to
/// integrate-and-dump: the time-average of the output over each sample interval is emitted
/// as one unfiltered delta at the sample boundary (SPEC §4.2).
#[derive(Clone, Copy, Debug)]
pub struct NoiseOsc {
    /// Register at the last committed output change.
    lfsr: u16,
    /// Register after `k` more clocks (where the output changes).
    lfsr_next: u16,
    /// Clocks from the last change to the next one (`u32::MAX` = never).
    k: u32,
    /// Clocks elapsed since the last change, as of `t_pos`.
    elapsed: f64,
    t_pos: f64,
    /// Clocks per sample.
    inc: f64,
    inv_inc: f64,
    nes: bool,
    short: bool,
    u: f32,
    next_t: f64,
    // integrate-and-dump
    id: bool,
    acc: f64,
    t_acc: f64,
    next_n: f64,
    shown: f32,
}

impl Default for NoiseOsc {
    fn default() -> Self {
        Self::new()
    }
}

impl NoiseOsc {
    /// Power-on register value (SPEC §4.3).
    pub const INITIAL: u16 = 0x7fff;

    /// A halted oscillator with the power-on register.
    pub const fn new() -> Self {
        Self {
            lfsr: Self::INITIAL,
            lfsr_next: Self::INITIAL,
            k: u32::MAX,
            elapsed: 0.0,
            t_pos: 0.0,
            inc: 0.0,
            inv_inc: 0.0,
            nes: false,
            short: false,
            u: 0.0,
            next_t: NEVER,
            id: false,
            acc: 0.0,
            t_acc: 0.0,
            next_n: 1.0,
            shown: 0.0,
        }
    }

    /// The LFSR register (15 bits).
    #[inline]
    pub fn lfsr(&self) -> u16 {
        self.lfsr
    }

    /// Whether the channel currently renders with integrate-and-dump.
    #[inline]
    pub fn is_integrating(&self) -> bool {
        self.id
    }

    /// The value that the mixer currently sees: the instantaneous unit value, or the last
    /// interval average while integrating.
    #[inline]
    pub fn value(&self) -> f32 {
        if self.id {
            self.shown
        } else {
            self.u
        }
    }

    /// Time of the next output change (BLEP mode).
    #[inline]
    pub fn next_t(&self) -> f64 {
        self.next_t
    }

    /// Next sample boundary at which an integrating channel emits.
    #[inline]
    pub fn next_boundary(&self) -> f64 {
        self.next_n
    }

    #[inline]
    fn clock(&self, s: u16) -> u16 {
        let (tap, bit6) = lfsr_kind(self.nes, self.short);
        lfsr_step(s, tap, bit6)
    }

    #[inline]
    fn bit_u(s: u16) -> f32 {
        // Output is high while bit 0 is clear.
        (1 - (s & 1)) as f32
    }

    fn find_change(&mut self) {
        if self.id {
            return;
        }
        let b0 = self.lfsr & 1;
        let mut s = self.lfsr;
        for i in 1..=LFSR_SEARCH {
            s = self.clock(s);
            if s & 1 != b0 {
                self.k = i;
                self.lfsr_next = s;
                return;
            }
        }
        self.k = u32::MAX;
        self.lfsr_next = s;
    }

    #[inline]
    fn schedule(&mut self) {
        self.next_t = if self.inc > MIN_INC && self.k != u32::MAX {
            self.t_pos + (self.k as f64 - self.elapsed).max(0.0) * self.inv_inc
        } else {
            NEVER
        };
    }

    /// Advance the register by `n` clocks (capped) without emitting anything.
    fn skip(&mut self, n: u64) {
        let n = n.min(LFSR_SEARCH as u64 * 2);
        let mut s = self.lfsr;
        for _ in 0..n {
            s = self.clock(s);
        }
        self.lfsr = s;
    }

    /// Load the register (note-on) at time `t`. A zero state (which would lock up) becomes the
    /// power-on value.
    pub fn reset(&mut self, state: u16, t: f64) {
        if self.id {
            // Close the integration up to `t` with the old register (or restart the window if
            // the channel was not rendered); the interval average accumulated so far stays.
            self.sync_window(t);
        }
        let s = state & 0x7fff;
        self.lfsr = if s == 0 { Self::INITIAL } else { s };
        self.elapsed = 0.0;
        self.t_pos = t;
        self.u = Self::bit_u(self.lfsr);
        if self.id {
            self.next_t = NEVER;
        } else {
            self.find_change();
            self.schedule();
        }
    }

    /// Integrate-and-dump: whether the window state is consistent at time `t`, i.e. the
    /// channel was integrated up to `t` (`t_acc` lies within one sample before `t`) and the
    /// pending boundary closes the interval containing `t_acc`.
    #[inline]
    fn window_valid(&self, t: f64) -> bool {
        self.t_acc.is_finite()
            && self.next_n.is_finite()
            && self.t_acc <= t
            && self.t_acc >= t - 1.0
            && self.next_n >= self.t_acc
            && self.next_n - 1.0 <= self.t_acc
    }

    /// Integrate-and-dump: bring the integration window to time `t`.
    ///
    /// A channel that was rendered up to `t` simply continues (the partial interval is
    /// integrated). A channel that was not rendered — a voice slot that sat idle while the
    /// engine shifted its time origin chunk after chunk, or a muted channel — restarts the
    /// window at `t`: the skipped time produced no output, so there is nothing to catch up.
    /// Either way the work is bounded (real-time safe, SPEC §0), however long the gap.
    fn sync_window(&mut self, t: f64) {
        if self.window_valid(t) && t <= self.next_n {
            if self.t_acc < t {
                self.advance_id(t);
            }
        } else {
            self.restart_window(t);
            // The caller (a note-on or re-configuration) emits the change of the mixer's view.
            self.shown = self.u;
        }
    }

    /// Integrate-and-dump: start a fresh integration window at `t` (nothing accumulated, the
    /// next boundary is the next sample boundary). The emitted value (`shown`) is unchanged.
    #[inline]
    fn restart_window(&mut self, t: f64) {
        self.acc = 0.0;
        self.t_acc = if t.is_finite() { t } else { 0.0 };
        self.next_n = self.t_acc.floor() + 1.0;
    }

    /// Re-anchor at time `t` with a new clock rate (clocks/sample) and register mode. When
    /// integrating, the caller should have integrated up to `t` (see [`Self::run`]).
    pub fn configure(&mut self, t: f64, inc: f64, nes: bool, short: bool) {
        if self.id {
            self.sync_window(t);
            // While integrating, `elapsed` is the phase inside the current clock and `lfsr`
            // the current register, so a mode change simply continues with the new feedback.
            self.nes = nes;
            self.short = short;
        } else {
            let dt = (t - self.t_pos).max(0.0);
            self.elapsed += dt * self.inc;
            if !self.elapsed.is_finite() {
                self.elapsed = 0.0;
            }
            if self.k != u32::MAX && self.elapsed >= self.k as f64 {
                // Changes were skipped while the channel was silent: catch up (bounded work).
                let n = self.elapsed.floor();
                self.skip(n as u64);
                self.elapsed -= n;
                self.u = Self::bit_u(self.lfsr);
                self.find_change();
            }
            if nes != self.nes || short != self.short {
                let n = self.elapsed.floor();
                self.skip(n as u64);
                self.elapsed -= n;
                self.nes = nes;
                self.short = short;
                self.u = Self::bit_u(self.lfsr);
                self.find_change();
            }
        }
        self.t_pos = t;
        self.inc = sane_inc(inc);
        self.inv_inc = if self.inc > MIN_INC { 1.0 / self.inc } else { 0.0 };
        let want_id = if self.id { self.inc >= ID_LEAVE } else { self.inc > ID_ENTER };
        if want_id && !self.id {
            // Make `elapsed` the phase inside the current clock (no output change happens
            // before `k`, so only the register moves).
            let n = self.elapsed.floor();
            self.skip(n as u64);
            self.elapsed -= n;
            self.id = true;
            self.shown = self.u;
            self.t_acc = t;
            self.acc = self.u as f64 * (t - t.floor());
            self.next_n = t.floor() + 1.0;
            self.next_t = NEVER;
        } else if !want_id && self.id {
            self.id = false;
            self.find_change();
            self.schedule();
        } else if !self.id {
            self.schedule();
        }
    }

    /// Apply the pending output change; returns the change of the unit value.
    #[inline]
    pub fn commit(&mut self) -> f32 {
        self.lfsr = self.lfsr_next;
        self.elapsed = 0.0;
        self.t_pos = self.next_t;
        let nu = Self::bit_u(self.lfsr);
        let du = nu - self.u;
        self.u = nu;
        self.find_change();
        self.schedule();
        du
    }

    /// Integrate-and-dump: accumulate the time the output is high from `t_acc` to `to`,
    /// clocking the register in a tight loop.
    #[inline]
    fn advance_id(&mut self, to: f64) {
        let dur = to - self.t_acc;
        if dur.is_nan() || dur <= 0.0 {
            return;
        }
        let (tap, bit6) = lfsr_kind(self.nes, self.short);
        let mut rem = (dur * self.inc).min(1e6);
        let mut s = self.lfsr;
        let mut u = 1 - (s & 1) as u32;
        let to_end = 1.0 - self.elapsed;
        let mut high = 0.0f64;
        if rem < to_end {
            high += u as f64 * rem;
            self.elapsed += rem;
        } else {
            high += u as f64 * to_end;
            rem -= to_end;
            s = lfsr_step(s, tap, bit6);
            u = 1 - (s & 1) as u32;
            let whole = rem as u32;
            let mut ones = 0u32;
            for _ in 0..whole {
                ones += u;
                s = lfsr_step(s, tap, bit6);
                u = 1 - (s & 1) as u32;
            }
            let frac = rem - whole as f64;
            high += ones as f64 + u as f64 * frac;
            self.elapsed = frac;
        }
        self.lfsr = s;
        self.u = u as f32;
        self.acc += high * self.inv_inc;
        self.t_acc = to;
    }

    /// Integrate-and-dump: close the interval ending at [`Self::next_boundary`] and return the
    /// change of the emitted value.
    #[inline]
    pub fn boundary(&mut self) -> f32 {
        let n = self.next_n;
        self.advance_id(n);
        let avg = self.acc.clamp(0.0, 1.0) as f32;
        self.acc = 0.0;
        self.next_n = n + 1.0;
        let d = avg - self.shown;
        self.shown = avg;
        d
    }

    /// Integrate-and-dump: integrate the partial interval up to `t_end` (no emission).
    #[inline]
    pub fn finish(&mut self, t_end: f64) {
        if self.id {
            self.advance_id(t_end);
        }
    }

    /// Render everything before `t_end` (BLEP transitions or integrate-and-dump boundaries).
    #[inline]
    pub fn run(&mut self, t_end: f64, al: f32, ar: f32, blip: &mut BlipBuffer) {
        if self.id {
            if !(self.t_acc >= -1.0 && self.next_n >= self.t_acc && self.next_n - 1.0 <= self.t_acc) {
                // Defensive: `configure`/`reset` restart stale windows, but a window left behind
                // by an unrendered stretch (times are chunk-relative, so a rendered channel
                // never lags more than a sample behind the chunk start) must never make the
                // loop below iterate over the stale time. The emitted value stays as it is, so
                // the integrator bookkeeping is untouched.
                self.restart_window(self.t_acc.clamp(0.0, t_end.max(0.0)));
            }
            while self.next_n < t_end {
                let n = self.next_n;
                let d = self.boundary();
                if d != 0.0 {
                    blip.add_raw(n.max(0.0) as usize, d * al, d * ar);
                }
            }
            self.advance_id(t_end);
        } else {
            while self.next_t < t_end {
                let t = self.next_t;
                let du = self.commit();
                blip.add_step(t, du * al, du * ar);
            }
        }
    }

    /// Shift the time origin by `dt` samples.
    #[inline]
    pub fn shift_time(&mut self, dt: f64) {
        self.t_pos -= dt;
        self.next_t -= dt;
        self.t_acc -= dt;
        self.next_n -= dt;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip::{lfsr_period, nes_lfsr_step, GB_EDGES};

    #[test]
    fn pulse_edges_are_exact() {
        let mut p = PulseOsc::new();
        // 100 samples per cycle, 25 % duty (GB 25 %: high on [7/8, 1/8)).
        let (r, f) = GB_EDGES[1];
        p.configure(0.0, 0.01, r, f, None);
        assert_eq!(p.u(), 1.0, "phase 0 lies inside the high part");
        assert!((p.next_t() - 12.5).abs() < 1e-9);
        p.commit();
        assert_eq!(p.u(), 0.0);
        assert!((p.next_t() - 87.5).abs() < 1e-9);
        p.commit();
        assert!((p.next_t() - 112.5).abs() < 1e-9);
        // Doubling the rate at t = 100 keeps the phase: the pending edge at 112.5 moves to 106.25.
        p.configure(100.0, 0.02, r, f, None);
        assert!((p.next_t() - 106.25).abs() < 1e-9);
    }

    #[test]
    fn pulse_static_and_halted() {
        let mut p = PulseOsc::new();
        p.configure(0.0, 0.01, 0.0, 0.5, Some(0.5));
        assert_eq!(p.next_t(), NEVER);
        assert_eq!(p.u(), 0.5);
        p.configure(10.0, 0.0, 0.0, 0.5, None);
        assert_eq!(p.next_t(), NEVER);
        p.configure(20.0, f64::NAN, 0.0, 0.5, None);
        assert_eq!(p.next_t(), NEVER);
    }

    #[test]
    fn wave_skips_equal_steps() {
        let mut w = [0u8; 32];
        w[..16].fill(15);
        let tab = WaveTable::new(&w);
        let mut o = WaveOsc::new();
        o.set_position(0.0, 0.0);
        o.configure(0.0, 1.0, 0, WaveMode::Run, &tab);
        assert_eq!(o.u(), 15.0);
        assert!((o.next_t() - 16.0).abs() < 1e-12);
        assert_eq!(o.commit(&tab), -15.0);
        assert!((o.next_t() - 32.0).abs() < 1e-12);
        // Shift 2: 15 >> 2 = 3.
        o.configure(20.0, 1.0, 2, WaveMode::Run, &tab);
        assert_eq!(o.u(), 0.0);
        assert!((tab.mean(0) - 7.5).abs() < 1e-6);
        let flat = WaveTable::new(&[7; 32]);
        o.configure(25.0, 1.0, 0, WaveMode::Run, &flat);
        assert_eq!(o.next_t(), NEVER);
    }

    #[test]
    fn wave_hold_freezes_position() {
        let tab = WaveTable::new(&pixl_core::wave::NES_TRIANGLE);
        let mut o = WaveOsc::new();
        o.set_position(3.5, 0.0);
        o.configure(0.0, 0.1, 0, WaveMode::Run, &tab);
        o.configure(10.0, 0.1, 0, WaveMode::Hold, &tab); // pos 4.5 → step 4 held
        assert_eq!(o.step(), 4);
        assert_eq!(o.u(), 11.0);
        o.configure(1000.0, 0.1, 0, WaveMode::Run, &tab);
        assert_eq!(o.step(), 4, "held sequencer resumes where it stopped");
    }

    #[test]
    fn noise_transitions_follow_the_register() {
        let mut n = NoiseOsc::new();
        n.reset(NoiseOsc::INITIAL, 0.0);
        n.configure(0.0, 0.5, true, false); // 2 samples per clock
                                            // Reference: count output changes over many clocks by stepping the register directly.
        let mut s = NoiseOsc::INITIAL;
        let mut changes = Vec::new();
        for clk in 1..=2000u32 {
            let ns = nes_lfsr_step(s, false);
            if ns & 1 != s & 1 {
                changes.push(clk as f64 * 2.0);
            }
            s = ns;
        }
        for &t in &changes {
            assert!((n.next_t() - t).abs() < 1e-6, "{} vs {t}", n.next_t());
            n.commit();
        }
        assert_eq!(n.lfsr(), {
            let mut s = NoiseOsc::INITIAL;
            for _ in 0..(*changes.last().unwrap() / 2.0) as usize {
                s = nes_lfsr_step(s, false);
            }
            s
        });
        assert_eq!(lfsr_period(NoiseOsc::INITIAL, |s| nes_lfsr_step(s, false), 40_000), Some(32767));
    }

    #[test]
    fn generic_lfsr_matches_chip_definitions() {
        use crate::chip::{gb_lfsr_step, nes_lfsr_step};
        let mut s = [0x7fffu16; 4];
        for _ in 0..70_000 {
            for (i, (nes, short)) in
                [(true, false), (true, true), (false, false), (false, true)].into_iter().enumerate()
            {
                let (tap, b6) = lfsr_kind(nes, short);
                let reference = if nes { nes_lfsr_step(s[i], short) } else { gb_lfsr_step(s[i], short) };
                let got = lfsr_step(s[i], tap, b6);
                assert_eq!(got, reference);
                s[i] = got;
            }
        }
    }

    #[test]
    fn fast_wave_integrates_and_dumps() {
        // SPEC §4.2 (DSP-07): above ~4 transitions per sample the wave channel averages each
        // sample interval instead of emitting up to 16 band-limited steps per sample.
        let alt: WaveRam = std::array::from_fn(|i| if i % 2 == 0 { 0 } else { 15 });
        let tab = WaveTable::new(&alt);
        assert_eq!(tab.changes(0), 32);
        let mut w = WaveOsc::new();
        w.set_position(0.0, 0.0);
        w.configure(0.0, 3.9 / 1.0, 0, WaveMode::Run, &tab); // 3.9 transitions per sample
        assert!(!w.is_integrating());
        w.configure(0.0, 8.0, 0, WaveMode::Run, &tab); // 8 steps = 8 transitions per sample
        assert!(w.is_integrating());
        let mut b = BlipBuffer::new(64);
        let (mut l, mut r) = ([0.0f32; 64], [0.0f32; 64]);
        w.run(64.0, 1.0, 1.0, &tab, &mut b);
        w.finish(64.0, &tab);
        b.read(&mut l, &mut r);
        // Every whole sample interval averages 0 and 15 equally: exactly 7.5.
        assert!(l[2..].iter().all(|v| (*v - 7.5).abs() < 1e-4), "{:?}", &l[..8]);
        // Hysteresis: stays integrating down to 3.5 transitions per sample.
        w.configure(64.0, 3.7, 0, WaveMode::Run, &tab);
        assert!(w.is_integrating());
        w.configure(64.0, 3.4, 0, WaveMode::Run, &tab);
        assert!(!w.is_integrating());
        // An idle (unrendered) channel restarts its window instead of walking the gap.
        w.configure(64.0, 8.0, 0, WaveMode::Run, &tab);
        for _ in 0..45_000 {
            w.shift_time(64.0);
        }
        w.configure(0.0, 8.0, 0, WaveMode::Run, &tab);
        assert!((0.0..=1.0).contains(&w.next_boundary()), "{}", w.next_boundary());
        // Smooth tables integrate exactly too: a constant wave gives its value.
        let flat = WaveTable::new(&[9; 32]);
        let mut w = WaveOsc::new();
        w.configure(0.0, 12.0, 0, WaveMode::Run, &flat);
        let mut b = BlipBuffer::new(64);
        w.run(64.0, 1.0, 1.0, &flat, &mut b);
        b.read(&mut l, &mut r);
        assert!(l.iter().all(|v| v.abs() < 1e-6), "a constant wave makes no transitions: never integrating");
        assert!(!w.is_integrating());
    }

    #[test]
    fn integrate_and_dump_window_survives_long_idle() {
        // SPEC §0: a slot idle for minutes (the engine shifts its time origin every chunk)
        // must restart its integration window on reuse instead of walking every idle sample.
        let mut n = NoiseOsc::new();
        n.reset(NoiseOsc::INITIAL, 0.0);
        n.configure(0.0, 9.3, true, false);
        assert!(n.is_integrating());
        let mut b = BlipBuffer::new(64);
        n.run(64.0, 0.1, 0.1, &mut b);
        for _ in 0..45_000 {
            n.shift_time(64.0); // 60 s at 48 kHz, not rendered
        }
        n.reset(0x1234, 0.0);
        assert!((0.0..=1.0).contains(&n.next_boundary()), "window restarted: {}", n.next_boundary());
        n.configure(0.0, 9.3, true, false);
        assert!((0.0..=1.0).contains(&n.next_boundary()));
        let mut l = [0.0f32; 64];
        let mut r = [0.0f32; 64];
        b.read(&mut l, &mut r);
        n.run(64.0, 0.1, 0.1, &mut b);
        assert_eq!(n.next_boundary(), 64.0, "exactly one boundary per sample");
        // Even without a reset, a stale window is never walked sample by sample.
        for _ in 0..45_000 {
            n.shift_time(64.0);
        }
        n.run(64.0, 0.1, 0.1, &mut b);
        assert!((63.0..=64.0).contains(&n.next_boundary()), "{}", n.next_boundary());
    }

    #[test]
    fn integrate_and_dump_averages() {
        let mut n = NoiseOsc::new();
        n.reset(NoiseOsc::INITIAL, 0.0);
        n.configure(0.0, 10.0, true, false);
        assert!(n.is_integrating());
        let mut sum = 0.0f64;
        let mut count = 0;
        while n.next_boundary() < 4000.0 {
            n.boundary();
            sum += n.value() as f64;
            count += 1;
            assert!((0.0..=1.0).contains(&n.value()));
        }
        // A maximal-length sequence is high (bit 0 clear) almost exactly half the time.
        let mean = sum / count as f64;
        assert!((mean - 0.5).abs() < 0.02, "{mean}");
        n.configure(4000.0, 2.0, true, false);
        assert!(!n.is_integrating());
        n.configure(4000.0, 7.5, true, false);
        assert!(!n.is_integrating(), "hysteresis");
    }
}
