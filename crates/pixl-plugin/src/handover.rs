//! Editor preset loads without `GuiContext::set_state` (SPEC §0, §8).
//!
//! Loading a preset from the editor must never block the host's main thread, never run on a
//! thread that can outlive the plugin instance, and never put deserialization, allocation, locks
//! or logging on the audio thread. nih-plug's `GuiContext::set_state` cannot guarantee any of
//! that while the plugin is in its processing state — which an AUv2 instance is for its whole
//! life (clap-wrapper starts processing in `Initialize` and stops it in `Cleanup`): the call then
//! blocks the calling thread until the next render, and the state restore (`filter_state`,
//! `deserialize_fields`, `initialize`) runs on the audio thread. The editor therefore loads a
//! preset the way a user would turn every control at once:
//!
//! 1. the complete preset (parameter values, wave RAM, macro routings) is published to the
//!    lock-free [`PresetMailbox`], a seqlock the editor writes and the audio thread reads;
//! 2. every parameter whose value changes is sent to the host as an ordinary gesture
//!    (`begin` / `set` / `end` through nih-plug's `GuiContext`), so the host displays and can
//!    record the new values. nih-plug queues them and applies them right after the next
//!    processing call (or in a parameter flush when the host is not processing);
//! 3. the audio thread ([`HandoverReceiver`]) plays the mailbox's preset from the next block
//!    on — parameters, wave RAM and macros switch together at one block boundary — until the
//!    queued parameter changes have certainly been applied, then hands back to the regular
//!    parameters (which hold the preset's values by then) while keeping the preset's wave and
//!    macros;
//! 4. on the main thread, [`EditorLoads`] promotes the preset's wave, macros and header
//!    metadata into the persistent state only once nih-plug's parameters hold the preset's
//!    values, so a project saved at any moment stores a self-consistent state.
//!
//! While the host does not process at all (Logic does not render idle, non-live tracks) the
//! editor still shows the requested preset at once ([`EditorLoads`] is the editor's view);
//! further loads are coalesced so nih-plug's bounded parameter-event queue holds at most one
//! preset's worth of changes, and everything completes as soon as the host renders again.
//!
//! Host-driven state loads (project open, AU presets, undo) keep nih-plug's regular path;
//! [`EditorLoads::cancel`] then drops whatever the editor had in flight and, if editor changes
//! are still waiting in nih-plug's queue, queues the loaded values after them so they win.
//!
//! Nothing here spawns a thread or blocks; everything the audio thread touches is wait-free and
//! allocation-free.

use crate::params::PixlParams;
use crate::presets::PresetMeta;
use crate::state::SharedState;
use nih_plug::prelude::GuiContext;
use pixl_core::preset::PresetState;
use pixl_core::sync::{AtomicMacros, AtomicWave};
use pixl_core::{MacroAssignments, ParamId, ParamKind, ParamValues, WaveRam, PARAMS, PARAM_COUNT};
use std::sync::atomic::{fence, AtomicU32, AtomicU64, AtomicU8, Ordering};

// ---------------------------------------------------------------------------------------
// The lock-free mailbox
// ---------------------------------------------------------------------------------------

/// What a mailbox record asks the audio thread to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordKind {
    /// Play this state until the parameter changes queued with it have been applied.
    Preset,
    /// Drop any state being held and return to the shared state (a host state load replaced
    /// everything).
    Cancel,
}

/// One consistent mailbox snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Record {
    /// Handover ID (monotonic; shares its number space with the editor's parameter changes,
    /// see [`PresetMailbox::queued`]).
    pub id: u64,
    /// What to do with the record.
    pub kind: RecordKind,
    /// Unmodulated parameter values.
    pub values: ParamValues,
    /// Wave RAM.
    pub wave: WaveRam,
    /// Macro routings.
    pub macros: MacroAssignments,
}

/// Single-slot seqlock carrying the newest editor preset to the audio thread, plus the counters
/// that tell each side how far the other has got.
///
/// The writer (the editor's [`EditorLoads`], serialized by its mutex) and the reader (the audio
/// thread's [`HandoverReceiver`]) never wait for each other: a read that overlaps a write is
/// detected and simply retried on the next block.
pub struct PresetMailbox {
    /// Even when stable, odd while a record is being written.
    seq: AtomicU32,
    id: AtomicU64,
    kind: AtomicU8,
    values: [AtomicU32; PARAM_COUNT],
    wave: AtomicWave,
    macros: AtomicMacros,
    /// Highest ID whose parameter gestures have all been handed to nih-plug (editor).
    queued: AtomicU64,
    /// Highest ID whose parameter gestures nih-plug has certainly applied (audio thread: the
    /// gestures were queued before a processing call started, and that call has finished).
    applied: AtomicU64,
    /// ID of the newest preset the editor has installed into the shared wave RAM, macros and
    /// header (editor).
    committed: AtomicU64,
}

impl Default for PresetMailbox {
    fn default() -> Self {
        Self::new()
    }
}

impl PresetMailbox {
    /// An empty mailbox.
    pub fn new() -> Self {
        let defaults = ParamValues::defaults();
        Self {
            seq: AtomicU32::new(0),
            id: AtomicU64::new(0),
            kind: AtomicU8::new(RecordKind::Cancel as u8),
            values: std::array::from_fn(|i| AtomicU32::new(defaults.0[i].to_bits())),
            wave: AtomicWave::new(pixl_core::wave::default_wave()),
            macros: AtomicMacros::new(&MacroAssignments::empty()),
            queued: AtomicU64::new(0),
            applied: AtomicU64::new(0),
            committed: AtomicU64::new(0),
        }
    }

    /// Write a record. Callers must be the only writer (the editor side holds the
    /// [`EditorLoads`] mutex).
    pub fn publish(&self, id: u64, kind: RecordKind, state: &PresetState) {
        let s = self.seq.load(Ordering::Relaxed);
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.id.store(id, Ordering::Relaxed);
        self.kind.store(kind as u8, Ordering::Relaxed);
        for (dst, v) in self.values.iter().zip(state.values.0.iter()) {
            dst.store(v.to_bits(), Ordering::Relaxed);
        }
        self.wave.store(&state.wave);
        self.macros.store(&state.macros);
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// Read the record if it changed since sequence number `seen` and no write is in progress.
    /// Wait-free and allocation-free; returns the record's sequence number with it.
    pub fn read(&self, seen: u32) -> Option<(u32, Record)> {
        let s1 = self.seq.load(Ordering::Acquire);
        if s1 == seen || s1 & 1 == 1 {
            return None;
        }
        let id = self.id.load(Ordering::Relaxed);
        let kind = if self.kind.load(Ordering::Relaxed) == RecordKind::Preset as u8 {
            RecordKind::Preset
        } else {
            RecordKind::Cancel
        };
        let mut values = ParamValues::defaults();
        for (dst, src) in values.0.iter_mut().zip(self.values.iter()) {
            *dst = f32::from_bits(src.load(Ordering::Relaxed));
        }
        let wave = self.wave.load();
        let macros = self.macros.load();
        fence(Ordering::Acquire);
        if self.seq.load(Ordering::Relaxed) != s1 {
            return None;
        }
        Some((s1, Record { id, kind, values, wave, macros }))
    }

    /// Announce that every gesture of `id` (and of all smaller IDs) is queued in nih-plug.
    pub fn mark_queued(&self, id: u64) {
        self.queued.fetch_max(id, Ordering::Release);
    }

    /// Highest fully queued ID.
    pub fn queued(&self) -> u64 {
        self.queued.load(Ordering::Acquire)
    }

    /// Highest ID whose gestures nih-plug has certainly applied.
    pub fn applied(&self) -> u64 {
        self.applied.load(Ordering::Acquire)
    }

    /// Announce that the shared wave RAM and macros now contain preset `id` (stored before).
    pub fn mark_committed(&self, id: u64) {
        self.committed.fetch_max(id, Ordering::Release);
    }

    /// ID of the newest preset installed into the shared state.
    pub fn committed(&self) -> u64 {
        self.committed.load(Ordering::Acquire)
    }

    /// Audio thread (single writer): everything queued up to `id` has been applied.
    fn advance_applied(&self, id: u64) {
        if id > self.applied.load(Ordering::Relaxed) {
            self.applied.store(id, Ordering::Release);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Audio side
// ---------------------------------------------------------------------------------------

/// What changed at the start of a processing call (see [`HandoverReceiver::begin`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handover {
    /// Nothing new.
    Unchanged,
    /// A new (or edited) state is held from now on: [`HandoverReceiver::held`].
    Hold,
    /// Whatever was held is dropped; the shared wave RAM and macros must be read again.
    Cancel,
}

/// The audio thread's end of the handover. Owned by the processor; every method is wait-free
/// and allocation-free.
#[derive(Clone, Debug, Default)]
pub struct HandoverReceiver {
    seen: u32,
    held: Option<Record>,
    released_through: u64,
    queued_at_start: u64,
    /// IDs queued before the previous call started: nih-plug applied them right after it.
    armed: u64,
}

impl HandoverReceiver {
    /// A receiver that has seen nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Call at the start of every processing call, before the parameters are used.
    pub fn begin(&mut self, mailbox: &PresetMailbox) -> Handover {
        mailbox.advance_applied(self.armed);
        // Read before the record: if a push is complete, all of its gestures are in nih-plug's
        // queue before this call starts and are applied right after it.
        self.queued_at_start = mailbox.queued();
        if let Some((seq, record)) = mailbox.read(self.seen) {
            self.seen = seq;
            if record.id > self.released_through {
                return match record.kind {
                    RecordKind::Preset => {
                        self.held = Some(record);
                        Handover::Hold
                    }
                    RecordKind::Cancel => {
                        self.held = None;
                        self.released_through = record.id;
                        Handover::Cancel
                    }
                };
            }
        }
        Handover::Unchanged
    }

    /// The state to play instead of the host parameters and the shared wave/macros.
    pub fn held(&self) -> Option<&Record> {
        self.held.as_ref()
    }

    /// Whether a state is being held.
    pub fn is_holding(&self) -> bool {
        self.held.is_some()
    }

    /// Call at the end of every processing call. Returns the ID of the held state when it was
    /// released: its parameter changes are applied from the next call on, so the regular
    /// parameters take over. The caller keeps playing the held wave RAM and macros until the
    /// shared ones contain them (see [`PresetMailbox::committed`]).
    pub fn end(&mut self) -> Option<u64> {
        self.armed = self.queued_at_start;
        match &self.held {
            Some(r) if self.queued_at_start >= r.id => {
                let id = r.id;
                self.released_through = id;
                self.held = None;
                Some(id)
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Editor side
// ---------------------------------------------------------------------------------------

/// The parameter values nih-plug currently holds (implemented by [`PixlParams`]; tests emulate
/// the wrapper).
pub(crate) trait HostValues {
    /// Unmodulated plain value of `id`.
    fn base_value(&self, id: ParamId) -> f32;

    /// All unmodulated plain values.
    fn base_values(&self) -> ParamValues {
        let mut v = ParamValues::defaults();
        for spec in PARAMS.iter() {
            v.0[spec.id.index()] = self.base_value(spec.id);
        }
        v
    }
}

impl HostValues for PixlParams {
    fn base_value(&self, id: ParamId) -> f32 {
        PixlParams::base_value(self, id)
    }
}

/// Where the editor's parameter gestures go: the host, through nih-plug's `GuiContext`.
pub(crate) trait ParamSink {
    fn begin(&self, id: ParamId);
    fn set(&self, id: ParamId, plain: f32);
    fn end(&self, id: ParamId);
}

/// [`ParamSink`] over nih-plug's `GuiContext` (queued, applied after the next processing call).
pub(crate) struct ContextSink<'a> {
    pub(crate) context: &'a dyn GuiContext,
    pub(crate) params: &'a PixlParams,
}

impl ParamSink for ContextSink<'_> {
    fn begin(&self, id: ParamId) {
        // SAFETY: the pointer belongs to `params`, which outlives this call.
        unsafe { self.context.raw_begin_set_parameter(self.params.ptr(id)) };
    }
    fn set(&self, id: ParamId, plain: f32) {
        let ptr = self.params.ptr(id);
        // SAFETY: as above; `preview_normalized` matches the table's normalization.
        unsafe {
            let normalized = ptr.preview_normalized(id.sanitize(plain));
            self.context.raw_set_parameter_normalized(ptr, normalized);
        }
    }
    fn end(&self, id: ParamId) {
        // SAFETY: as above.
        unsafe { self.context.raw_end_set_parameter(self.params.ptr(id)) };
    }
}

/// A preset requested by the editor.
#[derive(Clone, Debug)]
struct Batch {
    id: u64,
    state: PresetState,
    meta: PresetMeta,
    /// Parameters whose change was sent to the host.
    pushed: [bool; PARAM_COUNT],
}

/// The last value the editor queued for a parameter and the ID it was queued under.
#[derive(Clone, Copy, Debug)]
struct Queued {
    value: f32,
    id: u64,
}

/// The editor's bookkeeping of preset loads and parameter gestures that nih-plug has not applied
/// yet. Main thread only (lives behind a mutex the audio thread never touches).
///
/// The editor's view of the plugin is the applied state with everything still in flight laid
/// over it: the newest requested preset, and the last queued value of every parameter.
pub(crate) struct EditorLoads {
    next_id: u64,
    /// Presets whose gestures were sent, oldest first, until nih-plug has applied them (at most
    /// two: see [`Self::close`]).
    in_flight: Vec<Batch>,
    /// The newest request while an older one is still in flight (or a gesture is open). The
    /// audio thread already plays it from the mailbox; its gestures follow once the queue is
    /// free.
    waiting: Option<Batch>,
    shadow: [Option<Queued>; PARAM_COUNT],
    /// Open gestures; `Some(true)` when their `begin` was sent to the host.
    open: [Option<bool>; PARAM_COUNT],
}

impl Default for EditorLoads {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether two plain values of `id` are the same setting (tolerant to the host's float
/// round-trip through the normalized domain).
fn same(id: ParamId, a: f32, b: f32) -> bool {
    match id.spec().kind {
        ParamKind::Float { min, max, .. } => (a - b).abs() <= 1e-5 * (max - min).abs().max(1.0),
        _ => (a - b).abs() < 0.5,
    }
}

impl EditorLoads {
    /// Nothing in flight.
    pub(crate) fn new() -> Self {
        Self {
            next_id: 0,
            in_flight: Vec::new(),
            waiting: None,
            shadow: [None; PARAM_COUNT],
            open: [None; PARAM_COUNT],
        }
    }

    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Whether the last change the editor queued for `id` has been applied by nih-plug.
    fn applied(&self, sh: &SharedState, host: &dyn HostValues, id: ParamId) -> bool {
        match self.shadow[id.index()] {
            None => true,
            Some(q) => q.id <= sh.mailbox.applied() || same(id, q.value, host.base_value(id)),
        }
    }

    /// The value `id` has once everything queued is applied.
    fn target(&self, sh: &SharedState, host: &dyn HostValues, id: ParamId) -> f32 {
        match self.shadow[id.index()] {
            Some(q) if !self.applied(sh, host, id) => q.value,
            _ => host.base_value(id),
        }
    }

    fn newest(&self) -> Option<&Batch> {
        self.waiting.as_ref().or(self.in_flight.last())
    }

    fn newest_mut(&mut self) -> Option<&mut Batch> {
        self.waiting.as_mut().or(self.in_flight.last_mut())
    }

    fn gesture_open(&self) -> bool {
        self.open.iter().any(Option::is_some)
    }

    /// Whether nothing is on its way to the audio thread or the host (the editor's view is the
    /// applied state).
    pub(crate) fn is_settled(&self) -> bool {
        self.in_flight.is_empty() && self.waiting.is_none() && self.shadow.iter().all(Option::is_none)
    }

    /// The editor's view of a parameter (unmodulated plain value).
    pub(crate) fn view_value(&self, sh: &SharedState, host: &dyn HostValues, id: ParamId) -> f32 {
        match &self.waiting {
            Some(w) => w.state.values.get(id),
            None => self.target(sh, host, id),
        }
    }

    /// The editor's view of the wave RAM.
    pub(crate) fn view_wave(&self, sh: &SharedState) -> WaveRam {
        self.newest().map_or_else(|| sh.wave.load(), |b| b.state.wave)
    }

    /// The editor's view of the macro routings (sanitized).
    pub(crate) fn view_macros(&self, sh: &SharedState) -> MacroAssignments {
        let mut m = self.newest().map_or_else(|| sh.macros.load(), |b| b.state.macros);
        m.sanitize();
        m
    }

    /// The editor's view of the whole sound-defining state.
    pub(crate) fn view_state(&self, sh: &SharedState, host: &dyn HostValues) -> PresetState {
        let mut values = ParamValues::defaults();
        for spec in PARAMS.iter() {
            values.0[spec.id.index()] = self.view_value(sh, host, spec.id);
        }
        PresetState { values, wave: self.view_wave(sh), macros: self.view_macros(sh) }
    }

    /// The editor's view of the header metadata.
    pub(crate) fn view_meta(&self, sh: &SharedState) -> PresetMeta {
        match self.newest() {
            Some(b) => b.meta.clone(),
            None => sh.preset.lock().clone(),
        }
    }

    /// Apply `f` to the header metadata the editor shows, e.g. after saving it as a user
    /// preset. With `all`, also to every metadata still in flight and to the committed one
    /// (e.g. after deleting a user preset).
    pub(crate) fn edit_meta(&mut self, sh: &SharedState, all: bool, mut f: impl FnMut(&mut PresetMeta)) {
        if all {
            for b in self.in_flight.iter_mut().chain(self.waiting.iter_mut()) {
                f(&mut b.meta);
            }
            f(&mut sh.preset.lock());
        } else {
            match self.newest_mut() {
                Some(b) => f(&mut b.meta),
                None => f(&mut sh.preset.lock()),
            }
        }
    }

    /// Install the newest preset whose parameter changes nih-plug has applied into the
    /// persistent state (wave, macros, header), and forget applied parameter changes.
    pub(crate) fn promote(&mut self, sh: &SharedState, host: &dyn HostValues) {
        if self.is_settled() {
            return;
        }
        let applied = sh.mailbox.applied();
        let mut done = None;
        while let Some(first) = self.in_flight.first() {
            let settled = first.id <= applied
                || PARAMS.iter().all(|spec| !first.pushed[spec.id.index()] || self.applied(sh, host, spec.id));
            if !settled {
                break;
            }
            done = Some(self.in_flight.remove(0));
        }
        if let Some(b) = done {
            sh.wave.store(&b.state.wave);
            sh.macros.store(&b.state.macros);
            sh.mailbox.mark_committed(b.id);
            *sh.preset.lock() = b.meta;
        }
        for spec in PARAMS.iter() {
            if self.applied(sh, host, spec.id) {
                self.shadow[spec.id.index()] = None;
            }
        }
    }

    /// Send a batch's parameter changes to the host.
    fn push(&mut self, sh: &SharedState, host: &dyn HostValues, sink: &dyn ParamSink, mut batch: Batch) {
        for spec in PARAMS.iter() {
            let id = spec.id;
            let v = batch.state.values.get(id);
            if !same(id, v, self.target(sh, host, id)) {
                sink.begin(id);
                sink.set(id, v);
                sink.end(id);
                self.shadow[id.index()] = Some(Queued { value: v, id: batch.id });
                batch.pushed[id.index()] = true;
            }
        }
        sh.mailbox.mark_queued(batch.id);
        self.in_flight.push(batch);
    }

    /// Load a preset (complete state plus its header metadata). Never blocks; see the module
    /// docs.
    pub(crate) fn request(
        &mut self,
        sh: &SharedState,
        host: &dyn HostValues,
        sink: &dyn ParamSink,
        state: PresetState,
        meta: PresetMeta,
    ) {
        self.promote(sh, host);
        let id = self.alloc_id();
        let mut state = state;
        state.macros.sanitize();
        let batch = Batch { id, state, meta, pushed: [false; PARAM_COUNT] };
        sh.mailbox.publish(id, RecordKind::Preset, &batch.state);
        if self.in_flight.is_empty() && !self.gesture_open() {
            self.push(sh, host, sink, batch);
        } else {
            // Coalesce: only the newest request is kept; its gestures follow once nih-plug has
            // taken the previous ones (the audio thread already plays it from the mailbox).
            self.waiting = Some(batch);
        }
    }

    /// Progress everything that waits for the host: promote applied presets and send a waiting
    /// preset's gestures. The bridge calls it at least once per editor frame.
    pub(crate) fn pump(&mut self, sh: &SharedState, host: &dyn HostValues, sink: &dyn ParamSink) {
        self.promote(sh, host);
        if self.in_flight.is_empty() && !self.gesture_open() {
            if let Some(batch) = self.waiting.take() {
                self.push(sh, host, sink, batch);
            }
        }
    }

    /// The editor goes away: close open gestures and send what is still waiting (nih-plug
    /// applies it as soon as the host processes; at most two presets are then in flight).
    pub(crate) fn close(&mut self, sh: &SharedState, host: &dyn HostValues, sink: &dyn ParamSink) {
        for spec in PARAMS.iter() {
            if self.open[spec.id.index()].take() == Some(true) {
                sink.end(spec.id);
            }
        }
        self.promote(sh, host);
        if let Some(batch) = self.waiting.take() {
            self.push(sh, host, sink, batch);
        }
    }

    /// Start a parameter gesture.
    pub(crate) fn begin_edit(&mut self, sh: &SharedState, host: &dyn HostValues, sink: &dyn ParamSink, id: ParamId) {
        self.promote(sh, host);
        let waiting = self.waiting.is_some();
        let slot = &mut self.open[id.index()];
        if *slot == Some(true) {
            return;
        }
        if waiting {
            // A preset is waiting: the change becomes part of it and reaches the host with its
            // gestures.
            *slot = Some(false);
        } else {
            sink.begin(id);
            *slot = Some(true);
        }
    }

    /// Set a parameter (inside or outside a gesture).
    pub(crate) fn set_param(&mut self, sh: &SharedState, sink: &dyn ParamSink, id: ParamId, plain: f32) {
        let plain = id.sanitize(plain);
        if let Some(w) = &mut self.waiting {
            w.state.values.set(id, plain);
            sh.mailbox.publish(w.id, RecordKind::Preset, &w.state);
            return;
        }
        if self.open[id.index()] == Some(false) {
            sink.begin(id);
            self.open[id.index()] = Some(true);
        }
        sink.set(id, plain);
        let qid = self.alloc_id();
        self.shadow[id.index()] = Some(Queued { value: plain, id: qid });
        sh.mailbox.mark_queued(qid);
    }

    /// End a parameter gesture.
    pub(crate) fn end_edit(&mut self, sh: &SharedState, host: &dyn HostValues, sink: &dyn ParamSink, id: ParamId) {
        if self.open[id.index()].take() == Some(true) {
            sink.end(id);
        }
        self.pump(sh, host, sink);
    }

    /// Replace the wave RAM the editor shows (and the audio thread plays).
    pub(crate) fn store_wave(&mut self, sh: &SharedState, host: &dyn HostValues, wave: &WaveRam) {
        self.promote(sh, host);
        match self.newest_mut() {
            Some(b) => {
                b.state.wave = *wave;
                sh.mailbox.publish(b.id, RecordKind::Preset, &b.state);
            }
            None => sh.wave.store(wave),
        }
    }

    /// Replace the macro routings the editor shows (and the audio thread uses).
    pub(crate) fn store_macros(&mut self, sh: &SharedState, host: &dyn HostValues, macros: &MacroAssignments) {
        self.promote(sh, host);
        let mut macros = *macros;
        macros.sanitize();
        match self.newest_mut() {
            Some(b) => {
                b.state.macros = macros;
                sh.mailbox.publish(b.id, RecordKind::Preset, &b.state);
            }
            None => sh.macros.store(&macros),
        }
    }

    /// A host state load replaced the parameters and persistent fields (call it after both were
    /// restored). Forgets everything in flight and makes the audio thread drop a held preset.
    ///
    /// Editor changes still waiting in nih-plug's queue would be applied on top of the loaded
    /// state; with an editor open (`sink`), the loaded values of those parameters are queued
    /// after them, and the audio thread holds the loaded state until they are applied. Without
    /// an editor nothing can be queued; the stale changes then apply when the host processes
    /// (nih-plug's own behaviour for any editor change).
    pub(crate) fn cancel(&mut self, sh: &SharedState, host: &dyn HostValues, sink: Option<&dyn ParamSink>) {
        let stale: [bool; PARAM_COUNT] =
            std::array::from_fn(|i| ParamId::from_index(i).is_some_and(|id| !self.applied(sh, host, id)));
        self.in_flight.clear();
        self.waiting = None;
        let id = self.alloc_id();
        let loaded = PresetState { values: host.base_values(), wave: sh.wave.load(), macros: sh.macros.load() };
        match sink {
            Some(sink) if stale.iter().any(|s| *s) => {
                sh.mailbox.publish(id, RecordKind::Preset, &loaded);
                for spec in PARAMS.iter() {
                    let pid = spec.id;
                    if stale[pid.index()] {
                        let v = loaded.values.get(pid);
                        sink.begin(pid);
                        sink.set(pid, v);
                        sink.end(pid);
                        self.shadow[pid.index()] = Some(Queued { value: v, id });
                    }
                }
                sh.mailbox.mark_queued(id);
            }
            _ => sh.mailbox.publish(id, RecordKind::Cancel, &loaded),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::init_meta;
    use crate::processor::tests::{MockEngine, MockFx};
    use crate::processor::{BlockInput, Processor};
    use pixl_core::wave;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::Arc;

    fn state_with(chip: f32, wave: WaveRam) -> PresetState {
        let mut values = ParamValues::defaults();
        values.set(ParamId::Chip, chip);
        PresetState { values, wave, macros: MacroAssignments::empty() }
    }

    #[test]
    fn seqlock_reads_are_consistent_or_rejected() {
        let mb = Arc::new(PresetMailbox::new());
        assert!(mb.read(0).is_none(), "empty mailbox");
        let writer = {
            let mb = mb.clone();
            std::thread::spawn(move || {
                for i in 1..=20_000u64 {
                    let v = (i % 16) as u8;
                    let mut s = state_with((i % 3) as f32, [v; 32]);
                    s.values.set(ParamId::P1Level, v as f32);
                    mb.publish(i, RecordKind::Preset, &s);
                }
            })
        };
        let mut seen = 0;
        let mut reads = 0;
        while !writer.is_finished() || reads == 0 {
            if let Some((seq, r)) = mb.read(seen) {
                seen = seq;
                reads += 1;
                // Every field of a successful read comes from the same write.
                let v = (r.id % 16) as u8;
                assert!(r.wave.iter().all(|w| *w == v), "torn wave in record {}", r.id);
                assert_eq!(r.values.get(ParamId::P1Level), v as f32);
                assert_eq!(r.values.get(ParamId::Chip), (r.id % 3) as f32);
            }
        }
        writer.join().unwrap();
        let (_, last) = mb.read(0).unwrap();
        assert_eq!(last.id, 20_000);
    }

    #[test]
    fn receiver_holds_until_the_gestures_are_queued_and_applied() {
        let mb = PresetMailbox::new();
        let mut rx = HandoverReceiver::new();
        assert_eq!(rx.begin(&mb), Handover::Unchanged);
        assert_eq!(rx.end(), None);
        // Published, gestures not queued yet: hold across calls.
        mb.publish(1, RecordKind::Preset, &state_with(0.0, wave::square()));
        assert_eq!(rx.begin(&mb), Handover::Hold);
        assert_eq!(rx.held().map(|r| r.wave), Some(wave::square()));
        assert_eq!(rx.held().map(|r| r.values.get(ParamId::Chip)), Some(0.0));
        assert_eq!(rx.end(), None);
        assert_eq!(rx.begin(&mb), Handover::Unchanged);
        // Queued during this call: still held for one more call (nih-plug may have popped part
        // of the gestures before they were all queued).
        mb.mark_queued(1);
        assert_eq!(rx.end(), None);
        assert_eq!(rx.begin(&mb), Handover::Unchanged);
        assert!(rx.is_holding());
        assert_eq!(mb.applied(), 0);
        assert_eq!(rx.end(), Some(1), "released after a call that started with the gestures queued");
        assert!(rx.held().is_none());
        assert_eq!(mb.applied(), 0, "applied is only confirmed once the next call starts");
        assert_eq!(rx.begin(&mb), Handover::Unchanged);
        assert_eq!(mb.applied(), 1);
        assert_eq!(rx.end(), None);
        // Re-publishing an already released record (an edit racing the release) is ignored.
        mb.publish(1, RecordKind::Preset, &state_with(2.0, wave::saw()));
        assert_eq!(rx.begin(&mb), Handover::Unchanged);
        assert!(!rx.is_holding());
        rx.end();
        // A cancel always forces a reload of the shared state.
        mb.publish(2, RecordKind::Cancel, &state_with(1.0, wave::saw()));
        assert_eq!(rx.begin(&mb), Handover::Cancel);
        assert_eq!(rx.end(), None);
    }

    #[test]
    fn same_tolerates_host_round_trips_only() {
        assert!(same(ParamId::SweepTime, 180.0, 180.0001));
        assert!(!same(ParamId::SweepTime, 180.0, 181.0));
        assert!(same(ParamId::Chip, 1.0, 1.2));
        assert!(!same(ParamId::Chip, 1.0, 2.0));
    }

    // ----- scenario tests: the editor, nih-plug's queue and the audio thread together -----

    /// Emulates nih-plug: parameter values plus the queue of editor gestures that is applied
    /// right after each processing call (or by a parameter flush).
    struct EmuHost {
        values: RefCell<ParamValues>,
        queue: RefCell<VecDeque<(ParamId, f32)>>,
        log: RefCell<Vec<String>>,
    }

    impl EmuHost {
        fn new(values: ParamValues) -> Self {
            Self { values: RefCell::new(values), queue: RefCell::new(VecDeque::new()), log: RefCell::new(Vec::new()) }
        }
        /// nih-plug's `handle_out_events`.
        fn apply_queue(&self) {
            while let Some((id, v)) = self.queue.borrow_mut().pop_front() {
                self.values.borrow_mut().set(id, v);
            }
        }
        /// Apply only the first `n` queued changes (a pop racing the editor's pushes).
        fn apply_some(&self, n: usize) {
            for _ in 0..n {
                let Some((id, v)) = self.queue.borrow_mut().pop_front() else { break };
                self.values.borrow_mut().set(id, v);
            }
        }
    }

    impl HostValues for EmuHost {
        fn base_value(&self, id: ParamId) -> f32 {
            self.values.borrow().get(id)
        }
    }

    impl ParamSink for EmuHost {
        fn begin(&self, id: ParamId) {
            self.log.borrow_mut().push(format!("begin {}", id.key()));
        }
        fn set(&self, id: ParamId, plain: f32) {
            self.log.borrow_mut().push(format!("set {}", id.key()));
            self.queue.borrow_mut().push_back((id, plain));
        }
        fn end(&self, id: ParamId) {
            self.log.borrow_mut().push(format!("end {}", id.key()));
        }
    }

    /// One plugin instance: shared state, editor bookkeeping, emulated wrapper, processor.
    struct Rig {
        sh: SharedState,
        loads: EditorLoads,
        host: EmuHost,
        proc: Processor<MockEngine, MockFx>,
    }

    impl Rig {
        fn new() -> Self {
            let mut proc = Processor::with_parts(MockEngine::default(), MockFx::default());
            proc.prepare(48_000.0);
            Self {
                sh: SharedState::new(),
                loads: EditorLoads::new(),
                host: EmuHost::new(ParamValues::defaults()),
                proc,
            }
        }

        /// One processing call followed by nih-plug's queue handling; returns what the engine
        /// played: (chip parameter, P1 level, wave).
        fn audio(&mut self) -> (f32, f32, WaveRam) {
            let values = *self.host.values.borrow();
            let input = BlockInput {
                params: &values,
                wave: &self.sh.wave,
                macros: &self.sh.macros,
                transport: pixl_dsp::Transport::default(),
            };
            let (mut l, mut r) = ([0.0f32; 32], [0.0f32; 32]);
            self.proc.process_with(&input, Some(&self.sh.mailbox), [], &mut l, &mut r);
            let played = self.proc.engine().params_seen.last().copied().unwrap_or_default();
            let wave = self.proc.engine().wave.unwrap_or_default();
            self.host.apply_queue();
            (played.get(ParamId::Chip), played.get(ParamId::P1Level), wave)
        }

        fn request(&mut self, state: &PresetState, name: &str) {
            let mut meta = init_meta();
            meta.id = None;
            meta.name = name.to_string();
            meta.baseline = Some(state.clone());
            self.loads.request(&self.sh, &self.host, &self.host, state.clone(), meta);
        }

        fn pump(&mut self) {
            self.loads.pump(&self.sh, &self.host, &self.host);
        }

        /// What a project save would store right now: (chip, P1 level, wave, header name).
        fn save(&mut self) -> (f32, f32, WaveRam, String) {
            self.loads.promote(&self.sh, &self.host);
            let v = *self.host.values.borrow();
            (v.get(ParamId::Chip), v.get(ParamId::P1Level), self.sh.wave.load(), self.sh.preset.lock().name.clone())
        }
    }

    fn preset(chip: f32, level: f32, wave: WaveRam) -> PresetState {
        let mut s = state_with(chip, wave);
        s.values.set(ParamId::P1Level, level);
        s
    }

    #[test]
    fn preset_switches_atomically_and_saves_stay_consistent() {
        // SPEC §0/§8 (PLG-02/03/04): parameters, wave RAM and macros switch together at one
        // block boundary; a save at any moment stores either the old or the new preset whole.
        let a = preset(1.0, 12.0, wave::default_wave());
        let b = preset(0.0, 3.0, wave::square());
        for race in [false, true] {
            let mut rig = Rig::new();
            rig.audio();
            rig.request(&b, "B");
            // The editor shows B at once; nothing was applied yet, so a save stores A whole.
            assert_eq!(rig.loads.view_value(&rig.sh, &rig.host, ParamId::Chip), 0.0);
            assert_eq!(rig.loads.view_wave(&rig.sh), wave::square());
            assert_eq!(rig.save(), (1.0, 12.0, a.wave, "Hyrule Fanfare".to_string()));
            if race {
                // nih-plug pops part of the gestures right away (a pop racing the pushes).
                rig.host.apply_some(2);
            }
            let mut played = Vec::new();
            for _ in 0..4 {
                played.push(rig.audio());
                let saved = rig.save();
                let whole_a = saved == (1.0, 12.0, a.wave, "Hyrule Fanfare".to_string());
                let whole_b = saved == (0.0, 3.0, b.wave, "B".to_string());
                assert!(whole_a || whole_b, "inconsistent save {saved:?}");
                rig.pump();
            }
            // Every block plays one preset whole, and B from the first block on.
            for p in &played {
                assert_eq!(*p, (0.0, 3.0, b.wave), "race {race}: {played:?}");
            }
            assert_eq!(rig.save(), (0.0, 3.0, b.wave, "B".to_string()));
            assert!(rig.loads.is_settled());
            // Only the changed parameters were sent, each as a complete gesture.
            let log = rig.host.log.borrow();
            assert_eq!(*log, ["begin chip", "set chip", "end chip", "begin p1_level", "set p1_level", "end p1_level"]);
        }
    }

    #[test]
    fn loads_while_the_host_does_not_process_are_coalesced() {
        let b = preset(0.0, 3.0, wave::square());
        let c = preset(2.0, 7.0, wave::saw());
        let mut rig = Rig::new();
        rig.request(&b, "B");
        rig.request(&c, "C");
        rig.request(&b, "B2");
        rig.pump();
        // Only the first request's gestures are queued; the newest waits (bounded queue).
        assert_eq!(rig.host.queue.borrow().len(), 2);
        assert_eq!(rig.loads.view_meta(&rig.sh).name, "B2");
        assert_eq!(rig.save().3, "Hyrule Fanfare");
        // The host renders again: the newest request plays from the first block on.
        assert_eq!(rig.audio(), (0.0, 3.0, b.wave));
        rig.pump(); // B applied: B2's gestures go out (nothing differs from B)
        assert_eq!(rig.audio(), (0.0, 3.0, b.wave));
        rig.pump();
        assert_eq!(rig.audio(), (0.0, 3.0, b.wave));
        rig.pump();
        assert!(rig.loads.is_settled());
        assert_eq!(rig.save(), (0.0, 3.0, b.wave, "B2".to_string()));
    }

    #[test]
    fn edits_on_top_of_a_waiting_preset_reach_host_and_audio() {
        let b = preset(0.0, 3.0, wave::square());
        let c = preset(2.0, 7.0, wave::saw());
        let mut rig = Rig::new();
        rig.request(&b, "B");
        rig.request(&c, "C"); // waits behind B
        let sink = &rig.host;
        rig.loads.begin_edit(&rig.sh, &rig.host, sink, ParamId::P1Level);
        rig.loads.set_param(&rig.sh, sink, ParamId::P1Level, 9.0);
        rig.loads.end_edit(&rig.sh, &rig.host, sink, ParamId::P1Level);
        let mut w = wave::triangle();
        w[0] = 1;
        rig.loads.store_wave(&rig.sh, &rig.host, &w);
        assert_eq!(rig.loads.view_value(&rig.sh, &rig.host, ParamId::P1Level), 9.0);
        // The audio thread plays C with the edits as soon as it runs.
        assert_eq!(rig.audio(), (2.0, 9.0, w));
        for _ in 0..4 {
            rig.pump();
            assert_eq!(rig.audio(), (2.0, 9.0, w));
        }
        rig.pump();
        assert!(rig.loads.is_settled());
        assert_eq!(rig.save(), (2.0, 9.0, w, "C".to_string()));
        // No gesture was nested or left open.
        let log = rig.host.log.borrow();
        let begins = log.iter().filter(|l| l.starts_with("begin")).count();
        let ends = log.iter().filter(|l| l.starts_with("end")).count();
        assert_eq!(begins, ends);
    }

    #[test]
    fn host_state_load_cancels_and_outvotes_stale_editor_changes() {
        let b = preset(0.0, 3.0, wave::square());
        let mut rig = Rig::new();
        rig.request(&b, "B");
        // The host loads a project state before it processed the editor's gestures.
        let loaded = preset(2.0, 5.0, wave::saw());
        *rig.host.values.borrow_mut() = loaded.values;
        rig.sh.wave.store(&loaded.wave);
        rig.loads.cancel(&rig.sh, &rig.host, Some(&rig.host));
        // The loaded state plays from the first block on although the stale gestures are
        // applied after it; the compensating gestures win.
        for _ in 0..3 {
            assert_eq!(rig.audio(), (2.0, 5.0, loaded.wave));
            rig.pump();
        }
        assert_eq!(rig.save().0, 2.0);
        assert_eq!(rig.save().1, 5.0);
        assert!(rig.loads.is_settled());
        // Without pending editor changes a cancel simply returns to the shared state.
        let mut rig = Rig::new();
        rig.audio();
        rig.sh.wave.store(&wave::sine());
        rig.loads.cancel(&rig.sh, &rig.host, Some(&rig.host));
        assert_eq!(rig.audio().2, wave::sine());
        assert!(rig.host.log.borrow().is_empty());
    }

    #[test]
    fn a_released_preset_keeps_its_wave_until_the_editor_promotes_it() {
        // The audio thread releases the held preset one call after its gestures were queued;
        // the editor may promote the wave later (next frame). The processor must keep playing
        // the preset's wave, never the old shared one, and never re-read stale shared data
        // after a reset or re-prepare.
        let b = preset(0.0, 3.0, wave::square());
        let mut rig = Rig::new();
        rig.audio();
        rig.request(&b, "B");
        for _ in 0..3 {
            assert_eq!(rig.audio(), (0.0, 3.0, b.wave));
        }
        assert_eq!(rig.sh.wave.load(), wave::default_wave(), "not promoted yet");
        rig.proc.reset();
        rig.proc.prepare(96_000.0);
        assert_eq!(rig.audio(), (0.0, 3.0, b.wave));
        rig.pump();
        assert_eq!(rig.sh.wave.load(), b.wave);
        assert_eq!(rig.audio(), (0.0, 3.0, b.wave));
    }
}
