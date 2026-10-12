//! Persistent plugin state beyond the host parameters, and its (de)serialization.
//!
//! nih-plug stores parameters itself; everything else is persisted through
//! [`Params::serialize_fields`](nih_plug::prelude::Params::serialize_fields) as four JSON
//! fields:
//!
//! | key | content |
//! |---|---|
//! | `wave` | the 32-step wave RAM, `[10, 11, …]` |
//! | `macros` | macro routings in the `pixl_core` preset format |
//! | `preset` | header metadata `{id, name, category, number, modified}` |
//! | `ui` | [`UiState`] (selected channel, open editor, scale, language, …) |
//!
//! Restoring is deliberately forgiving: a missing field takes its fresh-instance default
//! (see [`filter_state`]), a field that fails to parse falls back to the default, and every
//! value is clamped into its domain. Loading never panics and never leaves the plugin in a
//! half-applied state.
//!
//! "Modified" (the orange dot in the header) is not a sticky flag: the plugin remembers the
//! state the current preset was loaded or saved with (the *baseline*) and compares the live
//! state against it, so undoing a change also clears the dot.

use crate::handover::{ContextSink, EditorLoads, ParamSink, PresetMailbox};
use crate::params::PixlParams;
use crate::presets::{factory_library, init_meta, PresetMeta};
use nih_plug::prelude::{GuiContext, PluginState};
use nih_plug::wrapper::state::ParamValue;
use parking_lot::Mutex;
use pixl_core::bridge::CurrentPreset;
use pixl_core::preset::{json_to_plain, PresetState};
use pixl_core::sync::{AtomicMacros, AtomicTelemetry, AtomicWave};
use pixl_core::wave::{self, WaveRam, WAVE_LEN, WAVE_MAX};
use pixl_core::{Category, MacroAssignments, ParamId, ParamKind, ParamValues, UiState, MACRO_COUNT, PARAMS};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Weak;
use std::sync::atomic::{AtomicU64, Ordering};

/// Persistent field key: wave RAM.
pub const FIELD_WAVE: &str = "wave";
/// Persistent field key: macro routings.
pub const FIELD_MACROS: &str = "macros";
/// Persistent field key: current preset metadata.
pub const FIELD_PRESET: &str = "preset";
/// Persistent field key: editor state.
pub const FIELD_UI: &str = "ui";

/// Longest preset name kept in state (characters).
pub const MAX_NAME_CHARS: usize = 64;

/// Smallest and largest editor scale (relative to the 900 × 560 design size).
pub const MIN_UI_SCALE: f32 = 0.75;
/// See [`MIN_UI_SCALE`].
pub const MAX_UI_SCALE: f32 = 2.0;

/// State shared between the audio thread, the host callbacks and the editor.
///
/// Audio-thread data (wave, macros, telemetry, the preset mailbox) is lock-free. Preset
/// metadata, UI state and the editor's load bookkeeping are only touched from the main thread
/// (editor calls and host state save/load), so they use short `Mutex` critical sections that
/// never contain I/O and are never taken on the audio thread.
pub struct SharedState {
    /// Host recall generation: invalidates editor drag/history even for the same preset.
    pub(crate) restore_generation: AtomicU64,
    pub(crate) host_notifications: Mutex<Option<std::sync::Arc<crate::host::HostNotifications>>>,
    /// Wave RAM (GUI writes, audio reads by version).
    pub wave: AtomicWave,
    /// Macro routings (GUI writes, audio reads by version).
    pub macros: AtomicMacros,
    /// Meters and voice data published by the audio thread.
    pub telemetry: AtomicTelemetry,
    /// Editor preset loads on their way to the audio thread (see [`crate::handover`]).
    pub mailbox: PresetMailbox,
    /// What the header shows, plus the baseline used for the modified flag (the committed
    /// state: it always matches the parameters nih-plug holds).
    pub(crate) preset: Mutex<PresetMeta>,
    /// Editor state.
    pub(crate) ui: Mutex<UiState>,
    /// The editor's preset loads and parameter gestures in flight.
    pub(crate) loads: Mutex<EditorLoads>,
    /// The open editor's host context (weak: the instance must never keep it alive), used to
    /// queue parameter changes when a host state load overrides editor changes in flight.
    pub(crate) editor: Mutex<Option<Weak<dyn GuiContext>>>,
}

impl SharedState {
    /// Fresh-instance state: default wave, factory macro routings, init preset metadata.
    pub fn new() -> Self {
        let meta = init_meta();
        Self {
            restore_generation: AtomicU64::new(0),
            host_notifications: Mutex::new(None),
            wave: AtomicWave::new(wave::default_wave()),
            macros: AtomicMacros::new(&MacroAssignments::factory_default()),
            telemetry: AtomicTelemetry::default(),
            mailbox: PresetMailbox::new(),
            preset: Mutex::new(meta),
            ui: Mutex::new(UiState::default()),
            loads: Mutex::new(EditorLoads::new()),
            editor: Mutex::new(None),
        }
    }

    /// Current UI state (copy).
    pub fn ui_state(&self) -> UiState {
        self.ui.lock().clone()
    }

    pub(crate) fn notify_host(&self) {
        if let Some(host) = self.host_notifications.lock().as_ref() { host.changed(); }
    }

    /// Current UI scale, sanitized.
    pub fn ui_scale(&self) -> f32 {
        sanitize_scale(self.ui.lock().scale)
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------
// Snapshots and the modified flag
// ---------------------------------------------------------------------------------------

/// The committed sound-defining state of an instance (the unmodulated parameters nih-plug
/// holds, the shared wave RAM and macro routings) — what a project save stores. Editor
/// changes still in flight are not included (see [`crate::handover`]).
pub fn current_state(params: &PixlParams) -> PresetState {
    let mut macros = params.shared.macros.load();
    macros.sanitize();
    PresetState { values: params.base_values(), wave: params.shared.wave.load(), macros }
}

/// Whether two states sound the same (tolerant to float round-trips through the host's
/// normalized domain; macro routings compared as sets).
pub fn states_equivalent(a: &PresetState, b: &PresetState) -> bool {
    values_equivalent(&a.values, &b.values) && a.wave == b.wave && macros_equivalent(&a.macros, &b.macros)
}

fn values_equivalent(a: &ParamValues, b: &ParamValues) -> bool {
    PARAMS.iter().all(|spec| {
        let (x, y) = (a.get(spec.id), b.get(spec.id));
        match spec.kind {
            ParamKind::Float { min, max, .. } => (x - y).abs() <= 1e-5 * (max - min).abs().max(1.0),
            _ => (x - y).abs() < 0.5,
        }
    })
}

fn macros_equivalent(a: &MacroAssignments, b: &MacroAssignments) -> bool {
    (0..MACRO_COUNT).all(|m| {
        a.targets(m).count() == b.targets(m).count()
            && a.targets(m).all(|(t, d)| b.depth(m, t).is_some_and(|e| (e - d).abs() < 1e-4))
    })
}

impl PresetMeta {
    /// Whether `current` differs from the preset as loaded/saved.
    pub fn is_modified(&self, current: &PresetState) -> bool {
        match &self.baseline {
            Some(b) => !states_equivalent(b, current),
            None => self.modified_fallback,
        }
    }
}

/// What the header pill shows right now: the editor's view, i.e. a preset the editor asked to
/// load is shown at once, before the host has applied it.
pub fn current_preset(params: &PixlParams) -> CurrentPreset {
    let shared = &params.shared;
    let loads = shared.loads.lock();
    let meta = loads.view_meta(shared);
    let state = loads.view_state(shared, params);
    CurrentPreset {
        modified: meta.is_modified(&state),
        id: meta.id,
        name: meta.name,
        category: meta.category,
        number: meta.number,
    }
}

// ---------------------------------------------------------------------------------------
// Persistent fields
// ---------------------------------------------------------------------------------------

/// Serialized form of the `preset` field.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct PresetMetaRepr {
    pub id: Option<String>,
    pub name: String,
    pub category: Category,
    pub number: u32,
    pub modified: bool,
}

/// Serialize the persistent fields (called by nih-plug when the host saves state, right after
/// it read the parameter values).
///
/// Stores the committed state: the wave RAM, macros and header metadata that belong to the
/// parameter values nih-plug holds. An editor preset load whose parameter changes the host has
/// not applied yet is not part of it (the project then stores the previous preset, whole and
/// correctly labelled); one that has been applied is installed first.
pub fn serialize_fields(params: &PixlParams) -> BTreeMap<String, String> {
    let shared = &params.shared;
    shared.loads.lock().promote(shared, params);
    let current = current_state(params);
    let mut out = BTreeMap::new();
    out.insert(FIELD_WAVE.to_string(), wave_to_json(&current.wave));
    out.insert(FIELD_MACROS.to_string(), macros_to_json(&current.macros));
    let repr = {
        let meta = shared.preset.lock();
        meta.to_repr(meta.is_modified(&current))
    };
    out.insert(FIELD_PRESET.to_string(), meta_to_json(&repr));
    out.insert(FIELD_UI.to_string(), ui_to_json(&shared.ui_state()));
    out
}

/// Restore the persistent fields (called by nih-plug during a host state load, after the
/// parameters were restored; on the host's main thread — PIXL never asks nih-plug to restore a
/// state from the editor, so this never runs on the audio thread).
///
/// Missing keys leave the current value untouched (the wrapper always passes all keys
/// after [`filter_state`]); unparseable values fall back to the fresh-instance default.
/// Editor preset loads still in flight are cancelled ([`EditorLoads::cancel`]).
pub fn deserialize_fields(params: &PixlParams, fields: &BTreeMap<String, String>) {
    let shared = &params.shared;
    if let Some(json) = fields.get(FIELD_WAVE) {
        shared.wave.store(&wave_from_json(json).unwrap_or_else(wave::default_wave));
    }
    if let Some(json) = fields.get(FIELD_MACROS) {
        shared.macros.store(&macros_from_json(json).unwrap_or_else(MacroAssignments::factory_default));
    }
    if let Some(json) = fields.get(FIELD_UI) {
        let mut ui = ui_from_json(json).unwrap_or_default();
        sanitize_ui(&mut ui);
        *shared.ui.lock() = ui;
    }
    if let Some(json) = fields.get(FIELD_PRESET) {
        let repr = meta_from_json(json).unwrap_or_else(|| init_meta().to_repr(false));
        let current = current_state(params);
        let meta = PresetMeta::restored(repr, &current);
        *shared.preset.lock() = meta;
    }
    let context = shared.editor.lock().as_ref().and_then(Weak::upgrade);
    let sink = context.as_deref().map(|context| ContextSink { context, params });
    shared.loads.lock().cancel(shared, params, sink.as_ref().map(|s| s as &dyn ParamSink));
    shared.restore_generation.fetch_add(1, Ordering::Release);
}

impl PresetMeta {
    /// Metadata after a state restore. Factory presets get the library state as baseline
    /// (so "modified" is always truthful); other presets trust the stored flag, using the
    /// restored state as baseline when it was unmodified.
    pub(crate) fn restored(repr: PresetMetaRepr, current: &PresetState) -> Self {
        let factory = repr.id.as_deref().and_then(|id| factory_library().get(id));
        let baseline = match factory {
            Some(f) => Some(f.state.clone()),
            None if !repr.modified => Some(current.clone()),
            None => None,
        };
        PresetMeta {
            id: repr.id,
            name: repr.name,
            category: repr.category,
            number: repr.number,
            baseline,
            modified_fallback: repr.modified,
        }
    }

    /// Serializable view with an explicit modified flag.
    pub(crate) fn to_repr(&self, modified: bool) -> PresetMetaRepr {
        PresetMetaRepr {
            id: self.id.clone(),
            name: self.name.clone(),
            category: self.category,
            number: self.number,
            modified,
        }
    }
}

/// Normalize a state before nih-plug restores it (`Plugin::filter_state`).
///
/// - every PIXL parameter gets a value of the right type: missing ones take their default
///   (so restoring is independent of the instance's previous state), mismatched types are
///   converted (e.g. an enum stored by label or index, a bool stored as a number), NaN and
///   out-of-range values are sanitized;
/// - unknown parameter keys (from a newer version) are dropped;
/// - missing persistent fields are filled with fresh-instance defaults.
pub fn filter_state(state: &mut PluginState) {
    let mut params = BTreeMap::new();
    for spec in PARAMS.iter() {
        let plain = state.params.get(spec.key).and_then(|v| param_value_to_plain(spec.id, v)).unwrap_or(spec.default);
        params.insert(spec.key.to_string(), plain_to_param_value(spec.id, plain));
    }
    state.params = params;
    let defaults = default_fields();
    for (key, value) in defaults {
        state.fields.entry(key).or_insert(value);
    }
}

/// The persistent fields of a fresh instance.
pub fn default_fields() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert(FIELD_WAVE.to_string(), wave_to_json(&wave::default_wave()));
    out.insert(FIELD_MACROS.to_string(), macros_to_json(&MacroAssignments::factory_default()));
    out.insert(FIELD_PRESET.to_string(), meta_to_json(&init_meta().to_repr(false)));
    out.insert(FIELD_UI.to_string(), ui_to_json(&UiState::default()));
    out
}

/// Plain values stored in a nih-plug state (missing or invalid entries take defaults).
pub fn values_from_plugin_state(state: &PluginState) -> ParamValues {
    let mut values = ParamValues::defaults();
    for spec in PARAMS.iter() {
        if let Some(v) = state.params.get(spec.key).and_then(|v| param_value_to_plain(spec.id, v)) {
            values.set(spec.id, v);
        }
    }
    values
}

/// The nih-plug representation of a plain value for parameter `id`.
pub fn plain_to_param_value(id: ParamId, plain: f32) -> ParamValue {
    let v = id.sanitize(plain);
    match id.spec().kind {
        ParamKind::Float { .. } => ParamValue::F32(v),
        ParamKind::Int { .. } | ParamKind::Enum { .. } => ParamValue::I32(v.round() as i32),
        ParamKind::Bool => ParamValue::Bool(v >= 0.5),
    }
}

/// Plain value from any nih-plug representation (sanitized), `None` if unusable.
pub fn param_value_to_plain(id: ParamId, value: &ParamValue) -> Option<f32> {
    let raw = match value {
        ParamValue::F32(v) => *v,
        ParamValue::I32(v) => *v as f32,
        ParamValue::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        ParamValue::String(s) => json_to_plain(id, &Value::String(s.clone()))?,
    };
    raw.is_finite().then(|| id.sanitize(raw))
}

// ----- field codecs -----

fn wave_to_json(w: &WaveRam) -> String {
    serde_json::to_string(&w.to_vec()).unwrap_or_else(|_| "[]".to_string())
}

/// Parse a wave field: exactly 32 numbers, each rounded and clamped to 0..=15.
pub(crate) fn wave_from_json(json: &str) -> Option<WaveRam> {
    let values: Vec<Value> = serde_json::from_str(json).ok()?;
    if values.len() != WAVE_LEN {
        return None;
    }
    let mut out = [0u8; WAVE_LEN];
    for (dst, v) in out.iter_mut().zip(values.iter()) {
        let f = v.as_f64().filter(|f| f.is_finite())?;
        *dst = f.round().clamp(0.0, WAVE_MAX as f64) as u8;
    }
    Some(out)
}

fn macros_to_json(m: &MacroAssignments) -> String {
    serde_json::to_string(m).unwrap_or_else(|_| "[[],[],[],[]]".to_string())
}

pub(crate) fn macros_from_json(json: &str) -> Option<MacroAssignments> {
    let mut m: MacroAssignments = serde_json::from_str(json).ok()?;
    m.sanitize();
    Some(m)
}

fn ui_to_json(ui: &UiState) -> String {
    serde_json::to_string(ui).unwrap_or_else(|_| "{}".to_string())
}

pub(crate) fn ui_from_json(json: &str) -> Option<UiState> {
    // `UiState` is `#[serde(default)]`, so partial objects work; unknown enum values make
    // the whole object invalid, which falls back to defaults.
    serde_json::from_str(json).ok()
}

/// Clamp UI state into its valid domain.
pub fn sanitize_ui(ui: &mut UiState) {
    ui.scale = sanitize_scale(ui.scale);
    ui.macro_selected = ui.macro_selected.min((MACRO_COUNT - 1) as u8);
}

/// Clamp a UI scale (non-finite → 1.0).
pub fn sanitize_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(MIN_UI_SCALE, MAX_UI_SCALE)
    } else {
        1.0
    }
}

fn meta_to_json(m: &PresetMetaRepr) -> String {
    serde_json::to_string(m).unwrap_or_else(|_| "{}".to_string())
}

/// Parse the `preset` field leniently (each member individually validated).
pub(crate) fn meta_from_json(json: &str) -> Option<PresetMetaRepr> {
    let v: Value = serde_json::from_str(json).ok()?;
    let obj = v.as_object()?;
    let name = obj.get("name").and_then(Value::as_str).map(clean_name).filter(|n| !n.is_empty())?;
    let id = obj.get("id").and_then(Value::as_str).filter(|id| is_valid_preset_id(id)).map(str::to_string);
    let category =
        obj.get("category").and_then(|c| serde_json::from_value::<Category>(c.clone()).ok()).unwrap_or_default();
    let number = obj.get("number").and_then(Value::as_u64).filter(|n| *n <= 999).unwrap_or(0) as u32;
    let modified = obj.get("modified").and_then(Value::as_bool).unwrap_or(false);
    Some(PresetMetaRepr { id, name, category, number, modified })
}

/// Remove control characters, trim, and limit to [`MAX_NAME_CHARS`].
pub fn clean_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).collect();
    cleaned.trim().chars().take(MAX_NAME_CHARS).collect::<String>().trim_end().to_string()
}

/// Whether `id` has the `factory:NNN` or `user:<file stem>` form.
pub fn is_valid_preset_id(id: &str) -> bool {
    if let Some(n) = id.strip_prefix("factory:") {
        return !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit());
    }
    if let Some(stem) = id.strip_prefix("user:") {
        return crate::presets::is_valid_stem(stem);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixl_core::wave;
    use pixl_core::{Channel, Focus, Lang};

    #[test]
    fn fresh_instance_is_the_init_preset_unmodified() {
        let params = PixlParams::new();
        let current = current_state(&params);
        assert_eq!(current.values, ParamValues::defaults());
        assert_eq!(current.wave, wave::default_wave());
        assert!(macros_equivalent(&current.macros, &MacroAssignments::factory_default()));
        let header = current_preset(&params);
        assert_eq!(header.name, "Hyrule Fanfare");
        assert_eq!(header.category, Category::Lead);
        assert_eq!(header.number, 14);
        assert!(!header.modified);
        if factory_library().get("factory:014").is_some() {
            assert_eq!(header.id.as_deref(), Some("factory:014"));
        }
    }

    #[test]
    fn persistent_fields_roundtrip() {
        let a = PixlParams::new();
        a.shared.wave.store(&wave::saw());
        let mut m = MacroAssignments::empty();
        m.set(2, ParamId::EchoMix, 0.75);
        m.set(0, ParamId::P1Duty, -0.3);
        a.shared.macros.store(&m);
        {
            let mut ui = a.shared.ui.lock();
            ui.channel = Channel::Noise;
            ui.focus = Some(Focus::Echo);
            ui.scale = 1.5;
            ui.lang = Lang::En;
            ui.macro_selected = 3;
        }
        *a.shared.preset.lock() = PresetMeta {
            id: Some("user:My Lead".into()),
            name: "My Lead".into(),
            category: Category::Bass,
            number: 0,
            baseline: None,
            modified_fallback: true,
        };
        let fields = a.serialize_fields_for_test();
        assert_eq!(fields.len(), 4);

        let b = PixlParams::new();
        deserialize_fields(&b, &fields);
        assert_eq!(b.shared.wave.load(), wave::saw());
        assert!(macros_equivalent(&b.shared.macros.load(), &m));
        assert_eq!(b.shared.ui_state(), a.shared.ui_state());
        let header = current_preset(&b);
        assert_eq!(header.id.as_deref(), Some("user:My Lead"));
        assert_eq!(header.name, "My Lead");
        assert_eq!(header.category, Category::Bass);
        assert!(header.modified);
        // Saving again reproduces the same bytes (host state reproducibility).
        assert_eq!(b.serialize_fields_for_test(), fields);
    }

    #[test]
    fn garbage_fields_fall_back_to_defaults_without_panicking() {
        let garbage = [
            "",
            "null",
            "{",
            "[1,2,3]",
            "{\"name\": 5}",
            "[[{\"target\": 7}]]",
            "\"\u{0}\"",
            "[99999999999999999999999]",
            "{\"scale\": \"big\"}",
        ];
        for g in garbage {
            let p = PixlParams::new();
            p.shared.wave.store(&wave::square());
            let mut fields = BTreeMap::new();
            for key in [FIELD_WAVE, FIELD_MACROS, FIELD_PRESET, FIELD_UI] {
                fields.insert(key.to_string(), g.to_string());
            }
            deserialize_fields(&p, &fields);
            assert_eq!(p.shared.wave.load(), wave::default_wave(), "wave from {g:?}");
            let ui = p.shared.ui_state();
            assert!(ui.scale >= MIN_UI_SCALE && ui.scale <= MAX_UI_SCALE);
            let header = current_preset(&p);
            assert!(!header.name.is_empty());
        }
        // Partially valid UI keeps the valid parts; out-of-range values are clamped.
        let p = PixlParams::new();
        let mut fields = BTreeMap::new();
        fields.insert(FIELD_UI.to_string(), r#"{"scale": 9.0, "macro_selected": 200, "lang": "En"}"#.to_string());
        deserialize_fields(&p, &fields);
        let ui = p.shared.ui_state();
        assert_eq!(ui.scale, MAX_UI_SCALE);
        assert_eq!(ui.macro_selected, 3);
        assert_eq!(ui.lang, Lang::En);
        // Wave values are clamped, not rejected.
        assert_eq!(wave_from_json(&format!("[{}]", ["300"; 32].join(","))), Some([15; 32]));
        assert_eq!(wave_from_json(&format!("[{}]", ["-3.7"; 32].join(","))), Some([0; 32]));
        assert_eq!(wave_from_json("[1,2]"), None);
    }

    #[test]
    fn filter_state_fills_and_coerces() {
        let mut state = PluginState { version: "0.9.0".into(), params: BTreeMap::new(), fields: BTreeMap::new() };
        state.params.insert("chip".into(), ParamValue::String("nes".into()));
        state.params.insert("p1_on".into(), ParamValue::F32(1.0));
        state.params.insert("p1_level".into(), ParamValue::I32(7));
        state.params.insert("p1_duty".into(), ParamValue::F32(f32::NAN));
        state.params.insert("crush_bits".into(), ParamValue::F32(99.0));
        state.params.insert("uni_voices".into(), ParamValue::F32(2.6));
        state.params.insert("from_the_future".into(), ParamValue::F32(1.0));
        filter_state(&mut state);
        assert_eq!(state.params.len(), PARAMS.len());
        assert!(!state.params.contains_key("from_the_future"));
        assert!(matches!(state.params["chip"], ParamValue::I32(0)));
        assert!(matches!(state.params["p1_on"], ParamValue::Bool(true)));
        assert!(matches!(state.params["p1_level"], ParamValue::F32(v) if v == 7.0));
        assert!(matches!(state.params["p1_duty"], ParamValue::F32(v) if v == 25.0));
        assert!(matches!(state.params["crush_bits"], ParamValue::F32(v) if v == 16.0));
        assert!(matches!(state.params["uni_voices"], ParamValue::I32(3)));
        assert!(matches!(state.params["glide_time"], ParamValue::F32(v) if v == 90.0));
        for key in [FIELD_WAVE, FIELD_MACROS, FIELD_PRESET, FIELD_UI] {
            assert!(state.fields.contains_key(key), "{key}");
        }
        // Existing fields are kept as they are.
        let mut state2 = state.clone();
        state2.fields.insert(FIELD_WAVE.into(), "[0]".into());
        filter_state(&mut state2);
        assert_eq!(state2.fields[FIELD_WAVE], "[0]");
    }

    #[test]
    fn complete_host_state_restores_fields_and_filtering_is_the_identity() {
        let mut values = ParamValues::defaults();
        values.set(ParamId::Chip, 2.0);
        values.set(ParamId::P2On, 1.0);
        values.set(ParamId::SweepTime, 333.25);
        values.set(ParamId::ArpChord, 5.0);
        let mut macros = MacroAssignments::empty();
        macros.set(1, ParamId::CrushRate, -0.5);
        let meta = PresetMetaRepr {
            id: Some("factory:042".into()),
            name: "Test".into(),
            category: Category::Bass,
            number: 42,
            modified: true,
        };
        let ui = UiState { scale: 1.25, ..UiState::default() };
        let mut state = PluginState {
            version: pixl_core::VERSION.to_string(),
            params: PARAMS.iter().map(|s| (s.key.to_string(), plain_to_param_value(s.id, values.get(s.id)))).collect(),
            fields: BTreeMap::new(),
        };
        state.fields.insert(FIELD_WAVE.to_string(), wave_to_json(&wave::triangle()));
        state.fields.insert(FIELD_MACROS.to_string(), macros_to_json(&macros));
        state.fields.insert(FIELD_PRESET.to_string(), meta_to_json(&meta));
        state.fields.insert(FIELD_UI.to_string(), ui_to_json(&ui));
        assert_eq!(values_from_plugin_state(&state), values);
        let p = PixlParams::new();
        deserialize_fields(&p, &state.fields);
        assert_eq!(p.shared.wave.load(), wave::triangle());
        assert!(macros_equivalent(&p.shared.macros.load(), &macros));
        assert_eq!(p.shared.ui_scale(), 1.25);
        let header = current_preset(&p);
        assert_eq!(header.name, "Test");
        assert_eq!(header.number, 42);
        // Filtering a complete state is the identity.
        let mut filtered = state.clone();
        filter_state(&mut filtered);
        assert_eq!(values_from_plugin_state(&filtered), values);
        assert_eq!(filtered.fields, state.fields);
    }

    #[test]
    fn modified_flag_tracks_the_baseline() {
        let base = PresetState {
            values: ParamValues::defaults(),
            wave: wave::default_wave(),
            macros: MacroAssignments::factory_default(),
        };
        let meta = PresetMeta {
            id: None,
            name: "X".into(),
            category: Category::Lead,
            number: 0,
            baseline: Some(base.clone()),
            modified_fallback: false,
        };
        assert!(!meta.is_modified(&base));
        let mut changed = base.clone();
        changed.values.set(ParamId::P1Level, 3.0);
        assert!(meta.is_modified(&changed));
        let mut changed = base.clone();
        changed.wave[3] = 0;
        assert!(meta.is_modified(&changed));
        let mut changed = base.clone();
        changed.macros.set(0, ParamId::P1Level, 0.5);
        assert!(meta.is_modified(&changed));
        // Float round-trip noise is not a modification; slot order does not matter.
        let mut same = base.clone();
        same.values.0[ParamId::SweepTime.index()] += 1e-4;
        let mut reordered = MacroAssignments::empty();
        for m in (0..MACRO_COUNT).rev() {
            let targets: Vec<_> = base.macros.targets(m).collect();
            for (t, d) in targets.into_iter().rev() {
                reordered.set(m, t, d);
            }
        }
        same.macros = reordered;
        assert!(!meta.is_modified(&same));
    }

    #[test]
    fn meta_parsing_is_lenient_and_safe() {
        let m =
            meta_from_json(r#"{"id":"user:../../etc/passwd","name":"  A\u0007B  ","category":"NOPE","number":1e9}"#)
                .unwrap();
        assert_eq!(m.id, None);
        assert_eq!(m.name, "AB");
        assert_eq!(m.category, Category::Lead);
        assert_eq!(m.number, 0);
        assert!(!m.modified);
        assert!(meta_from_json(r#"{"name":""}"#).is_none());
        assert!(is_valid_preset_id("factory:014"));
        assert!(is_valid_preset_id("user:My Lead"));
        assert!(!is_valid_preset_id("factory:"));
        assert!(!is_valid_preset_id("user:a/b"));
        assert!(!is_valid_preset_id("other:1"));
        let long = "x".repeat(200);
        assert_eq!(clean_name(&long).chars().count(), MAX_NAME_CHARS);
    }

    /// Records gestures; never applies them (a host that does not process).
    struct IdleHost(std::cell::RefCell<Vec<String>>);

    impl ParamSink for IdleHost {
        fn begin(&self, id: ParamId) {
            self.0.borrow_mut().push(format!("begin {}", id.key()));
        }
        fn set(&self, id: ParamId, _plain: f32) {
            self.0.borrow_mut().push(format!("set {}", id.key()));
        }
        fn end(&self, id: ParamId) {
            self.0.borrow_mut().push(format!("end {}", id.key()));
        }
    }

    #[test]
    fn a_save_while_an_editor_load_is_pending_stores_the_previous_preset_whole() {
        // PLG-04 (SPEC §8): the user loads "Bass X" from the editor on a track the host does not
        // render and saves the project at once. The parameters are still the old ones, so the
        // project must store the old preset's sound *and* name, unmodified; the header already
        // shows the new preset.
        let p = PixlParams::new();
        let host = IdleHost(Default::default());
        let mut values = ParamValues::defaults();
        values.set(ParamId::Chip, 0.0);
        values.set(ParamId::P1On, 1.0);
        let target = PresetState { values, wave: wave::saw(), macros: MacroAssignments::empty() };
        let meta = PresetMeta {
            id: Some("user:Bass X".into()),
            name: "Bass X".into(),
            category: Category::Bass,
            number: 0,
            baseline: Some(target.clone()),
            modified_fallback: false,
        };
        p.shared.loads.lock().request(&p.shared, &p, &host, target, meta);
        assert_eq!(host.0.borrow().len(), 6, "chip and p1_on were sent to the host");
        let header = current_preset(&p);
        assert_eq!((header.name.as_str(), header.modified), ("Bass X", false));
        let saved = serialize_fields(&p);
        let repr: Value = serde_json::from_str(&saved[FIELD_PRESET]).unwrap();
        assert_eq!(repr["name"], "Hyrule Fanfare");
        assert_eq!(repr["modified"], false);
        assert_eq!(wave_from_json(&saved[FIELD_WAVE]), Some(wave::default_wave()));
        assert_eq!(macros_from_json(&saved[FIELD_MACROS]), Some(MacroAssignments::factory_default()));
        // Reopening that project: the old preset, unmodified, exactly as it sounded.
        let q = PixlParams::new();
        deserialize_fields(&q, &saved);
        let reopened = current_preset(&q);
        assert_eq!((reopened.name.as_str(), reopened.modified), ("Hyrule Fanfare", false));
    }

    impl PixlParams {
        fn serialize_fields_for_test(&self) -> BTreeMap<String, String> {
            serialize_fields(self)
        }
    }
}
