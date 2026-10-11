//! [`PluginBridge`]: the real [`Bridge`] the editor talks to.
//!
//! - Parameter gestures go through nih-plug's [`GuiContext`] (`begin`/`set`/`end`), so the
//!   host records automation and undo exactly like for its own controls.
//! - Wave RAM and macro routings are written to the lock-free containers the audio thread
//!   reads by version.
//! - Presets are loaded with [`GuiContext::set_state`], so the host gets a parameter rescan
//!   (and the load is undoable / recorded where the host supports it). The state contains
//!   every parameter, the wave, the macros, the preset metadata and the *current* UI state.
//! - `modified` is computed by comparing against the preset's baseline (see
//!   [`crate::state`]).
//!
//! ## Why preset loads may be applied asynchronously
//!
//! While the host has the plugin in its processing state, nih-plug applies a GUI-initiated
//! `set_state` at the end of the next `process()` call and blocks the calling thread until
//! then. Hosts may stop calling `process()` without leaving that state (Logic Pro does this
//! for idle, non-live tracks), which would freeze the host's main thread. The bridge
//! therefore calls `set_state` directly only when that is guaranteed to return promptly
//! (plugin inactive, or audio ran within the last [`AUDIO_ALIVE_WINDOW_MS`]); otherwise it
//! hands the state to a small loader thread that applies it as soon as the host processes
//! again. Until then the header already shows the new preset.

use crate::params::PixlParams;
use crate::presets::PresetStore;
use crate::state::{self, current_state, plugin_state_for_preset, PresetMetaRepr};
use nih_plug::prelude::{GuiContext, PluginState};
use parking_lot::Mutex;
use pixl_core::bridge::CurrentPreset;
use pixl_core::wave;
use pixl_core::{
    Bridge, Category, MacroAssignments, ParamId, PresetId, PresetInfo, TelemetrySnapshot, UiState, WaveRam, MACRO_COUNT,
};
use std::sync::mpsc;
use std::sync::Arc;

/// Audio counts as running if `process()` was called within this many milliseconds.
pub const AUDIO_ALIVE_WINDOW_MS: u64 = 250;

/// The editor's view of the plugin.
pub struct PluginBridge {
    context: Arc<dyn GuiContext>,
    params: Arc<PixlParams>,
    store: Arc<PresetStore>,
    loader: Mutex<Option<mpsc::Sender<PluginState>>>,
}

impl PluginBridge {
    /// Create a bridge for an open editor.
    pub fn new(context: Arc<dyn GuiContext>, params: Arc<PixlParams>, store: Arc<PresetStore>) -> Self {
        Self { context, params, store, loader: Mutex::new(None) }
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

    /// Apply a full plugin state through the wrapper (see the module docs for the timing).
    fn apply_state(&self, state: PluginState) {
        let runtime = &self.params.shared.runtime;
        if !runtime.is_active() || runtime.processed_within(AUDIO_ALIVE_WINDOW_MS) {
            self.context.set_state(state);
            return;
        }
        let mut loader = self.loader.lock();
        let state = match loader.as_ref() {
            Some(tx) => match tx.send(state) {
                Ok(()) => return,
                Err(mpsc::SendError(state)) => state,
            },
            None => state,
        };
        // (Re)start the loader thread.
        let (tx, rx) = mpsc::channel::<PluginState>();
        let context = self.context.clone();
        let spawned = std::thread::Builder::new().name("PIXL preset loader".into()).spawn(move || {
            while let Ok(mut state) = rx.recv() {
                // Coalesce: only the most recent request matters.
                while let Ok(newer) = rx.try_recv() {
                    state = newer;
                }
                context.set_state(state);
            }
        });
        match spawned {
            Ok(_) => {
                if let Err(mpsc::SendError(state)) = tx.send(state) {
                    self.context.set_state(state);
                } else {
                    *loader = Some(tx);
                }
            }
            // No thread available: fall back to the synchronous path.
            Err(_) => {
                drop(loader);
                self.context.set_state(state);
            }
        }
    }

    /// Effective value of `id` given base values and macro routings (same math as
    /// [`pixl_core::apply_macros`], evaluated for a single parameter).
    fn modulated_value(&self, id: ParamId) -> f32 {
        let base = self.params.value(id);
        let assignments = self.params.shared.macros.load();
        let mut offset = 0.0f32;
        let mut touched = false;
        for m in 0..MACRO_COUNT {
            let mv = self.params.value(ParamId::macro_value(m)) / 100.0;
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

impl Bridge for PluginBridge {
    fn param(&self, id: ParamId) -> f32 {
        self.params.base_value(id)
    }

    fn param_modulated(&self, id: ParamId) -> f32 {
        self.modulated_value(id)
    }

    fn begin_edit(&self, id: ParamId) {
        // SAFETY: the pointer belongs to `self.params`, which outlives this call.
        unsafe { self.context.raw_begin_set_parameter(self.params.ptr(id)) };
    }

    fn set_param(&self, id: ParamId, plain: f32) {
        let plain = id.sanitize(plain);
        let ptr = self.params.ptr(id);
        // SAFETY: as above. `preview_normalized` matches the table's normalization.
        unsafe {
            let normalized = ptr.preview_normalized(plain);
            self.context.raw_set_parameter_normalized(ptr, normalized);
        }
    }

    fn end_edit(&self, id: ParamId) {
        // SAFETY: as above.
        unsafe { self.context.raw_end_set_parameter(self.params.ptr(id)) };
    }

    fn wave(&self) -> WaveRam {
        self.params.shared.wave.load()
    }

    fn set_wave(&self, wave: WaveRam) {
        self.params.shared.wave.store(&wave::sanitize(&wave));
    }

    fn macro_assignments(&self) -> MacroAssignments {
        let mut a = self.params.shared.macros.load();
        a.sanitize();
        a
    }

    fn set_macro_assignment(&self, macro_index: usize, target: ParamId, depth: f32) -> bool {
        let _guard = self.params.shared.macro_edit.lock();
        let mut a = self.params.shared.macros.load();
        a.sanitize();
        let ok = a.set(macro_index, target, depth);
        if ok {
            self.params.shared.macros.store(&a);
        }
        ok
    }

    fn preset_list(&self) -> Vec<PresetInfo> {
        self.store.list()
    }

    fn current_preset(&self) -> CurrentPreset {
        state::current_preset(&self.params)
    }

    fn load_preset(&self, id: &str) -> Result<(), String> {
        let resolved = self.store.resolve(id).map_err(|e| e.message(self.lang()))?;
        let meta = PresetMetaRepr {
            id: Some(resolved.id.clone()),
            name: resolved.name.clone(),
            category: resolved.category,
            number: resolved.number,
            modified: false,
        };
        let ui = self.params.shared.ui_state();
        let plugin_state = plugin_state_for_preset(&resolved.state, &meta, &ui);
        *self.params.shared.pending_preset.lock() = Some(CurrentPreset {
            id: Some(resolved.id),
            name: resolved.name,
            category: resolved.category,
            number: resolved.number,
            modified: false,
        });
        self.apply_state(plugin_state);
        Ok(())
    }

    fn save_user_preset(&self, name: &str, category: Category) -> Result<PresetId, String> {
        let current = current_state(&self.params);
        let saved = self.store.save_user(name, category, &current).map_err(|e| e.message(self.lang()))?;
        *self.params.shared.pending_preset.lock() = None;
        let mut meta = self.params.shared.preset.lock();
        meta.id = Some(saved.id.clone());
        meta.name = saved.name;
        meta.category = saved.category;
        meta.number = 0;
        meta.baseline = Some(saved.state);
        meta.modified_fallback = false;
        Ok(saved.id)
    }

    fn delete_user_preset(&self, id: &str) -> Result<(), String> {
        self.store.delete_user(id).map_err(|e| e.message(self.lang()))?;
        let mut meta = self.params.shared.preset.lock();
        if meta.id.as_deref() == Some(id) {
            // The sound stays; it just no longer has a stored preset behind it.
            meta.id = None;
        }
        Ok(())
    }

    fn set_favorite(&self, id: &str, favorite: bool) {
        if let Err(e) = self.store.set_favorite(id, favorite) {
            nih_plug::nih_warn!("PIXL: could not update favourites: {e:?}");
        }
    }

    fn telemetry(&self) -> TelemetrySnapshot {
        self.params.shared.telemetry.load()
    }

    fn ui_state(&self) -> UiState {
        self.params.shared.ui_state()
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
    use crate::presets::tests::TempDir;
    use crate::presets::{factory_library, init_state};
    use crate::state::{values_from_plugin_state, FIELD_UI};
    use nih_plug::prelude::{ParamPtr, Params, PluginApi};
    use pixl_core::{Channel, Lang};

    /// Records everything the bridge asks of the wrapper. `set_state` emulates the wrapper's
    /// field restore (parameter values cannot be set from outside nih-plug, so they are
    /// checked on the recorded state instead).
    pub(crate) struct MockContext {
        pub(crate) params: Arc<PixlParams>,
        pub(crate) calls: Mutex<Vec<String>>,
        pub(crate) states: Mutex<Vec<PluginState>>,
        pub(crate) accept_resize: bool,
    }

    impl MockContext {
        pub(crate) fn new(params: Arc<PixlParams>) -> Arc<Self> {
            Arc::new(Self {
                params,
                calls: Mutex::new(Vec::new()),
                states: Mutex::new(Vec::new()),
                accept_resize: true,
            })
        }
        fn key(&self, ptr: ParamPtr) -> String {
            self.params.param_map().into_iter().find(|(_, p, _)| *p == ptr).map(|(k, _, _)| k).unwrap_or_default()
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
            PluginState { version: String::new(), params: Default::default(), fields: self.params.serialize_fields() }
        }
        fn set_state(&self, mut state: PluginState) {
            crate::state::filter_state(&mut state);
            self.params.deserialize_fields(&state.fields);
            self.states.lock().push(state);
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
    fn loading_a_factory_preset_sets_complete_state_and_keeps_the_ui() {
        let tmp = TempDir::new("bridge-load");
        let (b, ctx, params) = bridge(&tmp);
        params.shared.ui.lock().channel = Channel::Noise;
        params.shared.ui.lock().scale = 1.5;
        b.set_wave(wave::square());
        assert!(b.current_preset().modified);

        let lib = factory_library();
        let target = lib.presets.last().expect("factory library is not empty");
        b.load_preset(&target.id).unwrap();
        let states = ctx.states.lock();
        assert_eq!(states.len(), 1, "inactive plugin: applied synchronously");
        let s = &states[0];
        assert_eq!(values_from_plugin_state(s), target.state.values);
        assert_eq!(s.params.len(), pixl_core::PARAM_COUNT);
        assert_eq!(params.shared.wave.load(), target.state.wave);
        assert_eq!(params.shared.ui_state().channel, Channel::Noise);
        assert_eq!(params.shared.ui_state().scale, 1.5);
        assert!(s.fields[FIELD_UI].contains("Noise"));
        let header = b.current_preset();
        assert_eq!(header.id.as_deref(), Some(target.id.as_str()));
        assert_eq!(header.name, target.preset.name);
        assert_eq!(header.number, target.preset.number);
        assert!(params.shared.pending_preset.lock().is_none());

        let err = b.load_preset("factory:999").unwrap_err();
        assert_eq!(err, "Пресет не найден");
        params.shared.ui.lock().lang = Lang::En;
        assert_eq!(b.load_preset("user:nope").unwrap_err(), "Preset not found");
    }

    #[test]
    fn save_then_load_user_preset() {
        let tmp = TempDir::new("bridge-save");
        let (b, ctx, params) = bridge(&tmp);
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
        assert_eq!(params.shared.wave.load(), wave::triangle());
        assert_eq!(params.shared.macros.load().depth(2, ParamId::ArpRate), Some(0.5));
        assert!(!b.current_preset().modified);
        let s = ctx.states.lock().last().cloned().unwrap();
        assert_eq!(values_from_plugin_state(&s), init_state().values);

        assert!(b.save_user_preset("   ", Category::Arp).is_err());
        b.set_favorite(&id, true);
        assert!(b.preset_list().iter().any(|p| p.id == id && p.favorite));
        b.delete_user_preset(&id).unwrap();
        assert_eq!(b.current_preset().id, None);
        assert_eq!(b.current_preset().name, "Tri Arp");
        assert!(b.delete_user_preset("factory:014").is_err());
    }

    #[test]
    fn deferred_loading_when_audio_is_stalled() {
        let tmp = TempDir::new("bridge-defer");
        let (b, ctx, params) = bridge(&tmp);
        // Active but no recent process() call: the load goes to the loader thread.
        params.shared.runtime.set_active(true);
        let lib = factory_library();
        let target = lib.presets.last().unwrap();
        b.load_preset(&target.id).unwrap();
        // The header shows the new preset immediately.
        assert_eq!(b.current_preset().name, target.preset.name);
        for _ in 0..200 {
            if !ctx.states.lock().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(ctx.states.lock().len(), 1, "loader thread applied the state");
        // With audio running the load is synchronous again.
        params.shared.runtime.mark_processed();
        b.load_preset("factory:014").ok();
        if lib.get("factory:014").is_some() {
            assert_eq!(ctx.states.lock().len(), 2);
        }
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
            states: Mutex::new(Vec::new()),
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
