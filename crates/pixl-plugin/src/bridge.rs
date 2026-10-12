//! [`PluginBridge`]: the real [`Bridge`] the editor talks to.
//!
//! - Parameter gestures go through nih-plug's [`GuiContext`] (`begin`/`set`/`end`), so the
//!   host records automation and undo exactly like for its own controls.
//! - Wave RAM and macro routings are written to the lock-free containers the audio thread
//!   reads by version.
//! - Presets are loaded without `GuiContext::set_state` (which blocks the calling thread until
//!   the next render while the plugin processes — always, in AUv2 — and restores the state on
//!   the audio thread): every changed parameter is sent as an ordinary gesture, and the audio
//!   thread switches parameters, wave RAM and macros together at one block boundary. See
//!   [`crate::handover`]. No call blocks, no thread is started, and nothing outlives the
//!   editor.
//! - Everything the editor reads is its *view*: a preset it asked to load shows at once, even
//!   while the host has not processed it yet; the persistent state only changes once the host
//!   has applied it (so a project saved meanwhile stays self-consistent).
//! - `modified` is computed by comparing against the preset's baseline (see
//!   [`crate::state`]).

use crate::handover::{ContextSink, EditorLoads};
use crate::params::PixlParams;
use crate::presets::{PresetMeta, PresetStore};
use crate::state;
use nih_plug::prelude::GuiContext;
use parking_lot::MutexGuard;
use pixl_core::bridge::CurrentPreset;
use pixl_core::wave;
use pixl_core::{
    Bridge, Category, MacroAssignments, ParamId, PresetId, PresetInfo, TelemetrySnapshot, UiState, WaveRam, MACRO_COUNT,
};
use std::sync::Arc;

/// The editor's view of the plugin.
pub struct PluginBridge {
    context: Arc<dyn GuiContext>,
    params: Arc<PixlParams>,
    store: Arc<PresetStore>,
}

impl PluginBridge {
    /// Create a bridge for an open editor. The instance keeps a weak reference to `context`
    /// while the bridge lives (for host state loads that override editor changes in flight).
    pub fn new(context: Arc<dyn GuiContext>, params: Arc<PixlParams>, store: Arc<PresetStore>) -> Self {
        *params.shared.editor.lock() = Some(Arc::downgrade(&context));
        Self { context, params, store }
    }

    /// The parameters this bridge edits.
    pub fn params(&self) -> &Arc<PixlParams> {
        &self.params
    }

    /// The preset store this bridge uses.
    pub fn store(&self) -> &Arc<PresetStore> {
        &self.store
    }

    fn lang(&self) -> pixl_core::Lang {
        self.params.shared.ui.lock().lang
    }

    fn loads(&self) -> MutexGuard<'_, EditorLoads> {
        self.params.shared.loads.lock()
    }

    fn sink(&self) -> ContextSink<'_> {
        ContextSink { context: self.context.as_ref(), params: &self.params }
    }

    /// Effective value of `id` given base values and macro routings (same math as
    /// [`pixl_core::apply_macros`], evaluated for a single parameter).
    fn modulated_value(&self, id: ParamId) -> f32 {
        let shared = &self.params.shared;
        let loads = self.loads();
        // While nothing is in flight the host's values include its own modulation (CLAP).
        let settled = loads.is_settled();
        let value = |p: ParamId| {
            if settled {
                self.params.value(p)
            } else {
                loads.view_value(shared, self.params.as_ref(), p)
            }
        };
        let base = value(id);
        let assignments = loads.view_macros(shared);
        let mut offset = 0.0f32;
        let mut touched = false;
        for m in 0..MACRO_COUNT {
            let mv = value(ParamId::macro_value(m)) / 100.0;
            if !mv.is_finite() || mv == 0.0 {
                continue;
            }
            if let Some(depth) = assignments.depth(m, id) {
                offset += mv.clamp(0.0, 1.0) * depth;
                touched = true;
            }
        }
        if touched {
            id.denormalize(id.normalize(base) + offset)
        } else {
            base
        }
    }
}

impl Drop for PluginBridge {
    /// The editor closes: end open gestures, send what still waits, and forget the context.
    fn drop(&mut self) {
        let shared = &self.params.shared;
        shared.loads.lock().close(shared, self.params.as_ref(), &self.sink());
        let mut editor = shared.editor.lock();
        let ours = editor.as_ref().is_some_and(|w| std::ptr::addr_eq(w.as_ptr(), Arc::as_ptr(&self.context)));
        if ours {
            *editor = None;
        }
    }
}

impl Bridge for PluginBridge {
    fn param(&self, id: ParamId) -> f32 {
        self.loads().view_value(&self.params.shared, self.params.as_ref(), id)
    }

    fn param_modulated(&self, id: ParamId) -> f32 {
        self.modulated_value(id)
    }

    fn begin_edit(&self, id: ParamId) {
        let shared = &self.params.shared;
        self.loads().begin_edit(shared, self.params.as_ref(), &self.sink(), id);
    }

    fn set_param(&self, id: ParamId, plain: f32) {
        self.loads().set_param(&self.params.shared, &self.sink(), id, plain);
    }

    fn end_edit(&self, id: ParamId) {
        let shared = &self.params.shared;
        self.loads().end_edit(shared, self.params.as_ref(), &self.sink(), id);
    }

    fn wave(&self) -> WaveRam {
        self.loads().view_wave(&self.params.shared)
    }

    fn set_wave(&self, wave: WaveRam) {
        let shared = &self.params.shared;
        let wave = wave::sanitize(&wave);
        let mut loads = self.loads();
        if loads.view_wave(shared) == wave { return; }
        loads.store_wave(shared, self.params.as_ref(), &wave);
        drop(loads);
        shared.notify_host();
    }

    fn macro_assignments(&self) -> MacroAssignments {
        self.loads().view_macros(&self.params.shared)
    }

    fn set_macro_assignment(&self, macro_index: usize, target: ParamId, depth: f32) -> bool {
        let shared = &self.params.shared;
        let mut loads = self.loads();
        let mut a = loads.view_macros(shared);
        let before = a;
        let ok = a.set(macro_index, target, depth);
        if ok {
            loads.store_macros(shared, self.params.as_ref(), &a);
        }
        drop(loads);
        if ok && a != before { shared.notify_host(); }
        ok
    }

    fn preset_list(&self) -> Vec<PresetInfo> {
        self.store.list()
    }

    /// Also drives the preset handover (the header asks every frame).
    fn current_preset(&self) -> CurrentPreset {
        let shared = &self.params.shared;
        self.loads().pump(shared, self.params.as_ref(), &self.sink());
        state::current_preset(&self.params)
    }

    fn load_preset(&self, id: &str) -> Result<(), String> {
        let resolved = self.store.resolve(id).map_err(|e| e.message(self.lang()))?;
        let meta = PresetMeta {
            id: Some(resolved.id),
            name: resolved.name,
            category: resolved.category,
            number: resolved.number,
            baseline: Some(resolved.state.clone()),
            modified_fallback: false,
        };
        let shared = &self.params.shared;
        self.loads().request(shared, self.params.as_ref(), &self.sink(), resolved.state, meta);
        Ok(())
    }

    fn save_user_preset(&self, name: &str, category: Category) -> Result<PresetId, String> {
        let shared = &self.params.shared;
        let current = self.loads().view_state(shared, self.params.as_ref());
        let saved = self.store.save_user(name, category, &current).map_err(|e| e.message(self.lang()))?;
        let id = saved.id.clone();
        self.loads().edit_meta(shared, false, |meta| {
            meta.id = Some(saved.id.clone());
            meta.name = saved.name.clone();
            meta.category = saved.category;
            meta.number = 0;
            meta.baseline = Some(saved.state.clone());
            meta.modified_fallback = false;
        });
        Ok(id)
    }

    fn delete_user_preset(&self, id: &str) -> Result<(), String> {
        self.store.delete_user(id).map_err(|e| e.message(self.lang()))?;
        self.loads().edit_meta(&self.params.shared, true, |meta| {
            if meta.id.as_deref() == Some(id) {
                // The sound stays; it just no longer has a stored preset behind it.
                meta.id = None;
            }
        });
        Ok(())
    }

    fn set_favorite(&self, id: &str, favorite: bool) {
        if let Err(e) = self.store.set_favorite(id, favorite) {
            nih_plug::nih_warn!("PIXL: could not update favourites: {e:?}");
        }
    }

    /// Also drives the preset handover (the editor asks for telemetry every frame).
    fn telemetry(&self) -> TelemetrySnapshot {
        let shared = &self.params.shared;
        self.loads().pump(shared, self.params.as_ref(), &self.sink());
        shared.telemetry.load()
    }

    fn ui_state(&self) -> UiState {
        self.params.shared.ui_state()
    }

    fn state_generation(&self) -> u64 {
        self.params.shared.restore_generation.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Stores everything except `scale`, which only changes through
    /// [`Bridge::request_resize`] (the host must agree to the new size).
    fn set_ui_state(&self, ui: UiState) {
        let mut ui = ui;
        state::sanitize_ui(&mut ui);
        let mut current = self.params.shared.ui.lock();
        ui.scale = current.scale;
        *current = ui;
    }

    fn request_resize(&self, scale: f32) -> bool {
        let scale = state::sanitize_scale(scale);
        let old = {
            let mut ui = self.params.shared.ui.lock();
            std::mem::replace(&mut ui.scale, scale)
        };
        if (old - scale).abs() < 1e-6 {
            return true;
        }
        // `request_resize` asks the editor for its size again, which locks `ui`: the lock
        // must not be held here.
        if self.context.request_resize() {
            true
        } else {
            self.params.shared.ui.lock().scale = old;
            false
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::presets::factory_library;
    use crate::presets::tests::TempDir;
    use nih_plug::prelude::{ParamPtr, Params, PluginApi, PluginState};
    use parking_lot::Mutex;
    use pixl_core::{Channel, Lang};

    /// Records everything the bridge asks of the wrapper. nih-plug's parameter values cannot be
    /// set from outside the wrapper, so tests check the recorded gestures and the editor's view.
    pub(crate) struct MockContext {
        pub(crate) params: Arc<PixlParams>,
        pub(crate) calls: Mutex<Vec<String>>,
        /// `set_state` / `get_state` calls (the bridge must never make them).
        pub(crate) state_calls: Mutex<usize>,
        pub(crate) accept_resize: bool,
    }

    impl MockContext {
        pub(crate) fn new(params: Arc<PixlParams>) -> Arc<Self> {
            Arc::new(Self { params, calls: Mutex::new(Vec::new()), state_calls: Mutex::new(0), accept_resize: true })
        }
        fn key(&self, ptr: ParamPtr) -> String {
            self.params.param_map().into_iter().find(|(_, p, _)| *p == ptr).map(|(k, _, _)| k).unwrap_or_default()
        }
        fn gestures(&self) -> Vec<String> {
            self.calls.lock().iter().filter(|c| !c.starts_with("resize")).cloned().collect()
        }
    }

    impl GuiContext for MockContext {
        fn plugin_api(&self) -> PluginApi {
            PluginApi::Clap
        }
        fn request_resize(&self) -> bool {
            self.calls.lock().push(format!("resize {}", self.params.shared.ui_scale()));
            self.accept_resize
        }
        unsafe fn raw_begin_set_parameter(&self, param: ParamPtr) {
            let k = self.key(param);
            self.calls.lock().push(format!("begin {k}"));
        }
        unsafe fn raw_set_parameter_normalized(&self, param: ParamPtr, normalized: f32) {
            let k = self.key(param);
            self.calls.lock().push(format!("set {k} {normalized:.4}"));
        }
        unsafe fn raw_end_set_parameter(&self, param: ParamPtr) {
            let k = self.key(param);
            self.calls.lock().push(format!("end {k}"));
        }
        fn get_state(&self) -> PluginState {
            *self.state_calls.lock() += 1;
            PluginState { version: String::new(), params: Default::default(), fields: self.params.serialize_fields() }
        }
        fn set_state(&self, _state: PluginState) {
            // nih-plug blocks here until the next render while the plugin processes (always in
            // AUv2) and restores the state on the audio thread: the bridge must never call it.
            *self.state_calls.lock() += 1;
        }
    }

    fn bridge(tmp: &TempDir) -> (PluginBridge, Arc<MockContext>, Arc<PixlParams>) {
        let params = Arc::new(PixlParams::new());
        let ctx = MockContext::new(params.clone());
        let store = Arc::new(PresetStore::new(Some(tmp.0.clone())));
        (PluginBridge::new(ctx.clone(), params.clone(), store), ctx, params)
    }

    #[test]
    fn parameter_gestures_go_through_the_host() {
        let tmp = TempDir::new("bridge-params");
        let (b, ctx, _) = bridge(&tmp);
        b.begin_edit(ParamId::SweepAmt);
        b.set_param(ParamId::SweepAmt, 24.0);
        b.set_param(ParamId::SweepAmt, 999.0);
        b.end_edit(ParamId::SweepAmt);
        b.set_param_once(ParamId::Chip, 2.0);
        b.reset_param(ParamId::P1Coarse);
        let calls = ctx.calls.lock().clone();
        assert_eq!(
            calls,
            [
                "begin sweep_amt",
                "set sweep_amt 0.5000",
                "set sweep_amt 1.0000",
                "end sweep_amt",
                "begin chip",
                "set chip 1.0000",
                "end chip",
                "begin p1_coarse",
                "set p1_coarse 0.5000",
                "end p1_coarse",
            ]
        );
        assert_eq!(b.param(ParamId::GlideTime), 90.0);
        // The editor sees its own changes before the host has applied them.
        assert_eq!(b.param(ParamId::SweepAmt), 48.0);
        assert_eq!(b.param(ParamId::Chip), 2.0);
    }

    #[test]
    fn modulated_values_follow_the_macros() {
        let tmp = TempDir::new("bridge-mod");
        let (b, _, _) = bridge(&tmp);
        // Factory routing: macro 4 (20 %) → echo_mix +0.5 → 25 % + 10 % = 35 %.
        assert!((b.param_modulated(ParamId::EchoMix) - 35.0).abs() < 1e-3);
        assert_eq!(b.param(ParamId::EchoMix), 25.0);
        assert_eq!(b.param_modulated(ParamId::P1Level), b.param(ParamId::P1Level));
        // Same result as the audio thread's apply_macros.
        let base = pixl_core::ParamValues::defaults();
        let mut out = base;
        let mv = std::array::from_fn(|m| base.get(ParamId::macro_value(m)) / 100.0);
        pixl_core::apply_macros(&base, mv, &b.macro_assignments(), &mut out);
        for id in ParamId::ALL {
            assert!((b.param_modulated(*id) - out.get(*id)).abs() < 1e-4, "{}", id.key());
        }
        // An edit in flight is part of the modulated view too.
        b.set_param_once(ParamId::Macro4, 100.0);
        assert!((b.param_modulated(ParamId::EchoMix) - 75.0).abs() < 1e-3);
    }

    #[test]
    fn wave_and_macro_edits_are_shared_and_mark_modified() {
        let tmp = TempDir::new("bridge-wave");
        let (b, _, params) = bridge(&tmp);
        assert!(!b.current_preset().modified);
        let v0 = params.shared.wave.version();
        let mut w = wave::sine();
        w[0] = 200; // sanitized to 15
        b.set_wave(w);
        assert!(params.shared.wave.version() > v0);
        assert_eq!(b.wave()[0], 15);
        assert!(b.current_preset().modified);
        b.set_wave(wave::default_wave());
        assert!(!b.current_preset().modified, "undoing the edit clears the flag");

        assert!(b.set_macro_assignment(0, ParamId::P1Duty, 0.4));
        assert_eq!(b.macro_assignments().depth(0, ParamId::P1Duty), Some(0.4));
        assert!(b.current_preset().modified);
        assert!(b.set_macro_assignment(0, ParamId::P1Duty, 0.0));
        assert!(!b.current_preset().modified);
        assert!(!b.set_macro_assignment(0, ParamId::Chip, 0.5), "enums are not modulatable");
        assert!(!b.set_macro_assignment(7, ParamId::P1Level, 0.5));
    }

    #[test]
    fn loading_a_factory_preset_sends_gestures_and_keeps_the_ui() {
        // PLG-02/03 (SPEC §0, §8): no `set_state`, so nothing blocks and no state restore runs
        // on the audio thread; every changed parameter reaches the host as a gesture.
        let tmp = TempDir::new("bridge-load");
        let (b, ctx, params) = bridge(&tmp);
        params.shared.ui.lock().channel = Channel::Noise;
        params.shared.ui.lock().scale = 1.5;
        b.set_wave(wave::square());
        assert!(b.current_preset().modified);

        let lib = factory_library();
        let target = lib.presets.last().expect("factory library is not empty");
        b.load_preset(&target.id).unwrap();
        assert_eq!(*ctx.state_calls.lock(), 0, "GuiContext::set_state is never used");
        // Every parameter that differs was sent as a complete gesture, nothing else.
        let calls = ctx.gestures();
        let defaults = pixl_core::ParamValues::defaults();
        let differs = |s: &pixl_core::ParamSpec| {
            let d = (target.state.values.get(s.id) - defaults.get(s.id)).abs();
            match s.kind {
                pixl_core::ParamKind::Float { min, max, .. } => d > 1e-5 * (max - min).abs().max(1.0),
                _ => d >= 0.5,
            }
        };
        let changed: Vec<&str> = pixl_core::PARAMS.iter().filter(|s| differs(s)).map(|s| s.key).collect();
        assert_eq!(calls.len(), 3 * changed.len());
        for (k, key) in changed.iter().enumerate() {
            assert_eq!(calls[3 * k], format!("begin {key}"));
            assert!(calls[3 * k + 1].starts_with(&format!("set {key} ")));
            assert_eq!(calls[3 * k + 2], format!("end {key}"));
        }
        // The editor shows the new preset at once, the UI layout stays.
        for id in ParamId::ALL {
            assert!((b.param(*id) - target.state.values.get(*id)).abs() < 1e-4, "{}", id.key());
        }
        assert_eq!(b.wave(), target.state.wave);
        assert_eq!(params.shared.ui_state().channel, Channel::Noise);
        assert_eq!(params.shared.ui_state().scale, 1.5);
        let header = b.current_preset();
        assert_eq!(header.id.as_deref(), Some(target.id.as_str()));
        assert_eq!(header.name, target.preset.name);
        assert_eq!(header.number, target.preset.number);
        assert!(!header.modified);
        // The audio thread gets the complete preset through the mailbox.
        let (_, record) = params.shared.mailbox.read(0).expect("published");
        assert_eq!(record.values, target.state.values);
        assert_eq!(record.wave, target.state.wave);

        let err = b.load_preset("factory:999").unwrap_err();
        assert_eq!(err, "Пресет не найден");
        params.shared.ui.lock().lang = Lang::En;
        assert_eq!(b.load_preset("user:nope").unwrap_err(), "Preset not found");
    }

    #[test]
    fn save_then_load_user_preset() {
        let tmp = TempDir::new("bridge-save");
        let (b, _, params) = bridge(&tmp);
        b.set_wave(wave::triangle());
        b.set_macro_assignment(2, ParamId::ArpRate, 0.5);
        assert!(b.current_preset().modified);
        let id = b.save_user_preset("Tri Arp", Category::Arp).unwrap();
        assert_eq!(id, "user:Tri Arp");
        let header = b.current_preset();
        assert_eq!(header.id.as_deref(), Some("user:Tri Arp"));
        assert_eq!(header.name, "Tri Arp");
        assert_eq!(header.category, Category::Arp);
        assert_eq!(header.number, 0);
        assert!(!header.modified);
        assert!(b.preset_list().iter().any(|p| p.id == id && !p.is_factory));

        // Change things, then load the saved preset back.
        b.set_wave(wave::saw());
        b.set_macro_assignment(2, ParamId::ArpRate, 0.0);
        assert!(b.current_preset().modified);
        b.load_preset(&id).unwrap();
        assert_eq!(b.wave(), wave::triangle());
        assert_eq!(b.macro_assignments().depth(2, ParamId::ArpRate), Some(0.5));
        assert!(!b.current_preset().modified);
        let (_, record) = params.shared.mailbox.read(0).expect("published");
        assert_eq!(record.values, crate::presets::init_state().values);

        assert!(b.save_user_preset("   ", Category::Arp).is_err());
        b.set_favorite(&id, true);
        assert!(b.preset_list().iter().any(|p| p.id == id && p.favorite));
        b.delete_user_preset(&id).unwrap();
        assert_eq!(b.current_preset().id, None);
        assert_eq!(b.current_preset().name, "Tri Arp");
        assert!(b.delete_user_preset("factory:014").is_err());
    }

    /// Emulates nih-plug's `GuiContext::set_state` while the plugin is in the processing state
    /// (always the case in AUv2): the call would block until the audio thread runs.
    struct ProcessingContext {
        inner: Arc<MockContext>,
    }

    impl GuiContext for ProcessingContext {
        fn plugin_api(&self) -> PluginApi {
            PluginApi::Clap
        }
        fn request_resize(&self) -> bool {
            true
        }
        unsafe fn raw_begin_set_parameter(&self, param: ParamPtr) {
            unsafe { self.inner.raw_begin_set_parameter(param) }
        }
        unsafe fn raw_set_parameter_normalized(&self, param: ParamPtr, normalized: f32) {
            unsafe { self.inner.raw_set_parameter_normalized(param, normalized) }
        }
        unsafe fn raw_end_set_parameter(&self, param: ParamPtr) {
            unsafe { self.inner.raw_end_set_parameter(param) }
        }
        fn get_state(&self) -> PluginState {
            self.inner.get_state()
        }
        fn set_state(&self, _state: PluginState) {
            panic!("set_state would block the calling thread until the host renders");
        }
    }

    #[test]
    fn editor_preset_loads_never_block_or_outlive_the_editor() {
        // PLG-03: with the plugin processing but the host not rendering (Logic, idle track), a
        // preset load must return at once, start no thread, and leave nothing that keeps the
        // host's context alive once the editor is gone.
        let tmp = TempDir::new("bridge-plg03");
        let params = Arc::new(PixlParams::new());
        let ctx = Arc::new(ProcessingContext { inner: MockContext::new(params.clone()) });
        let store = Arc::new(PresetStore::new(Some(tmp.0.clone())));
        let b = PluginBridge::new(ctx.clone(), params.clone(), store);
        let lib = factory_library();
        let t0 = std::time::Instant::now();
        for p in lib.presets.iter().rev().take(8) {
            b.load_preset(&p.id).unwrap();
        }
        // Generous for a loaded machine: blocking means waiting for a render (nih-plug's 1 s
        // timeout loop), and the mock's `set_state` panics outright.
        assert!(t0.elapsed() < std::time::Duration::from_millis(500), "the editor thread never blocks");
        assert_eq!(b.current_preset().name, lib.presets[lib.presets.len() - 8].preset.name);
        // Coalesced: one preset's gestures in flight, the newest waits (nih-plug's queue is
        // bounded).
        let sent = ctx.inner.gestures().len();
        assert!(sent <= 3 * pixl_core::PARAM_COUNT, "{sent} events queued");
        drop(b);
        assert_eq!(Arc::strong_count(&ctx), 1, "nothing keeps the host context alive after the editor is gone");
        assert!(params.shared.editor.lock().is_none());
        // Closing the editor sent the waiting preset; at most two presets' gestures are queued.
        let sent = ctx.inner.gestures().len();
        assert!(sent <= 6 * pixl_core::PARAM_COUNT, "{sent} events queued");
        let calls = ctx.inner.gestures();
        let begins = calls.iter().filter(|c| c.starts_with("begin")).count();
        let ends = calls.iter().filter(|c| c.starts_with("end")).count();
        assert_eq!(begins, ends, "every gesture is closed");
    }

    #[test]
    fn host_state_load_while_a_load_is_pending_wins() {
        // A host state load (project/AU preset) while editor gestures wait in nih-plug's queue:
        // the loaded values are queued after them so they win, and the editor shows them.
        let tmp = TempDir::new("bridge-cancel");
        let (b, ctx, params) = bridge(&tmp);
        let lib = factory_library();
        let target = lib.presets.last().unwrap();
        b.load_preset(&target.id).unwrap();
        let sent = ctx.gestures().len();
        assert!(sent > 0);
        // nih-plug restores the parameters (unchanged here) and then the fields.
        let mut fields = crate::state::default_fields();
        fields.insert(
            crate::state::FIELD_WAVE.into(),
            "[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1]".into(),
        );
        params.deserialize_fields(&fields);
        let calls = ctx.gestures();
        assert_eq!(calls.len(), 2 * sent, "the loaded value of every pending parameter follows");
        for id in ParamId::ALL {
            assert_eq!(b.param(*id), params.base_value(*id), "{}", id.key());
        }
        assert_eq!(b.wave(), [1; 32]);
        assert_eq!(b.current_preset().name, "Hyrule Fanfare");
        let (_, record) = params.shared.mailbox.read(0).unwrap();
        assert_eq!(record.wave, [1; 32], "the audio thread holds the loaded state");
    }

    #[test]
    fn ui_state_and_resizing() {
        let tmp = TempDir::new("bridge-ui");
        let (b, ctx, params) = bridge(&tmp);
        let ui = UiState { channel: Channel::P2, scale: 2.0, macro_selected: 9, ..UiState::default() };
        b.set_ui_state(ui);
        let got = b.ui_state();
        assert_eq!(got.channel, Channel::P2);
        assert_eq!(got.scale, 1.0, "scale only changes through request_resize");
        assert_eq!(got.macro_selected, 3);

        assert!(b.request_resize(1.5));
        assert_eq!(params.shared.ui_scale(), 1.5);
        assert_eq!(ctx.calls.lock().last().map(String::as_str), Some("resize 1.5"));
        assert!(b.request_resize(1.5), "no-op resize succeeds without asking the host");
        assert!(b.request_resize(f32::NAN));
        assert_eq!(params.shared.ui_scale(), 1.0);

        let params2 = Arc::new(PixlParams::new());
        let ctx2 = Arc::new(MockContext {
            params: params2.clone(),
            calls: Mutex::new(Vec::new()),
            state_calls: Mutex::new(0),
            accept_resize: false,
        });
        let b2 = PluginBridge::new(ctx2, params2.clone(), Arc::new(PresetStore::new(None)));
        assert!(!b2.request_resize(2.0));
        assert_eq!(params2.shared.ui_scale(), 1.0, "rejected resize reverts the scale");
    }

    #[test]
    fn bridge_is_a_thread_safe_trait_object() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PluginBridge>();
        let tmp = TempDir::new("bridge-dyn");
        let (b, _, _) = bridge(&tmp);
        let dynamic: Arc<dyn Bridge> = Arc::new(b);
        assert_eq!(dynamic.param(ParamId::Chip), 1.0);
    }

    #[test]
    fn telemetry_is_forwarded() {
        let tmp = TempDir::new("bridge-tele");
        let (b, _, params) = bridge(&tmp);
        let t = TelemetrySnapshot { active_voices: 3, last_note: Some(64), frame: 99, ..Default::default() };
        params.shared.telemetry.publish(&t);
        assert_eq!(b.telemetry(), t);
    }
}
