//! Drives the real CLAP entry point (`pixl::clap_entry`, i.e. nih-plug's wrapper around
//! [`pixl::Pixl`]) with a minimal in-process host: parameter info/values/text conversions,
//! parameter changes through `flush`, state save/load including PIXL's persistent fields,
//! damaged states, and the parameter rescan the host gets after a state load.

mod common;

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    clap_event_header, clap_event_midi, clap_event_note, clap_event_note_expression, clap_event_param_value,
    clap_input_events, clap_output_events, CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI, CLAP_EVENT_NOTE_EXPRESSION,
    CLAP_EVENT_NOTE_ON, CLAP_EVENT_PARAM_VALUE, CLAP_NOTE_EXPRESSION_PRESSURE, CLAP_NOTE_EXPRESSION_TUNING,
};
use clap_sys::ext::note_ports::{
    clap_note_port_info, clap_plugin_note_ports, CLAP_EXT_NOTE_PORTS, CLAP_NOTE_DIALECT_CLAP, CLAP_NOTE_DIALECT_MIDI,
};
use clap_sys::ext::params::{
    clap_host_params, clap_param_info, clap_plugin_params, CLAP_EXT_PARAMS, CLAP_PARAM_IS_STEPPED,
    CLAP_PARAM_RESCAN_VALUES,
};
use clap_sys::ext::state::{clap_plugin_state, CLAP_EXT_STATE};
use clap_sys::ext::thread_check::{clap_host_thread_check, CLAP_EXT_THREAD_CHECK};
use clap_sys::factory::plugin_factory::{clap_plugin_factory, CLAP_PLUGIN_FACTORY_ID};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::clap_process;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use pixl_core::{ParamId, ParamKind, PARAMS, PARAM_COUNT};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::{c_char, c_void, CStr};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

/// The CLAP entry is process-global: serialize the tests that use it.
static ENTRY_LOCK: Mutex<()> = Mutex::new(());

/// `clap.params` rescans with `CLAP_PARAM_RESCAN_VALUES` the plugin requested.
static VALUE_RESCANS: AtomicU32 = AtomicU32::new(0);
/// `request_callback` calls.
static CALLBACK_REQUESTS: AtomicU32 = AtomicU32::new(0);
/// What the host's `clap.thread-check` answers for `is_main_thread`.
static ON_MAIN_THREAD: AtomicBool = AtomicBool::new(true);

unsafe extern "C" fn host_rescan(_host: *const clap_host, flags: u32) {
    if flags & CLAP_PARAM_RESCAN_VALUES != 0 {
        VALUE_RESCANS.fetch_add(1, Ordering::SeqCst);
    }
}
unsafe extern "C" fn host_clear(_host: *const clap_host, _id: u32, _flags: u32) {}
unsafe extern "C" fn host_noop(_host: *const clap_host) {}
unsafe extern "C" fn host_request_callback(_host: *const clap_host) {
    CALLBACK_REQUESTS.fetch_add(1, Ordering::SeqCst);
}
unsafe extern "C" fn host_is_main_thread(_host: *const clap_host) -> bool {
    ON_MAIN_THREAD.load(Ordering::SeqCst)
}
unsafe extern "C" fn host_is_audio_thread(_host: *const clap_host) -> bool {
    false
}

static HOST_PARAMS: clap_host_params =
    clap_host_params { rescan: Some(host_rescan), clear: Some(host_clear), request_flush: Some(host_noop) };
static HOST_THREAD_CHECK: clap_host_thread_check =
    clap_host_thread_check { is_main_thread: Some(host_is_main_thread), is_audio_thread: Some(host_is_audio_thread) };

unsafe extern "C" fn host_get_extension(_host: *const clap_host, id: *const c_char) -> *const c_void {
    let id = CStr::from_ptr(id);
    if id == CLAP_EXT_PARAMS {
        &HOST_PARAMS as *const clap_host_params as *const c_void
    } else if id == CLAP_EXT_THREAD_CHECK {
        &HOST_THREAD_CHECK as *const clap_host_thread_check as *const c_void
    } else {
        null()
    }
}

static HOST: clap_host = clap_host {
    clap_version: CLAP_VERSION,
    host_data: null_mut(),
    name: c"pixl-test-host".as_ptr(),
    vendor: c"PIXL".as_ptr(),
    url: c"".as_ptr(),
    version: c"1.0".as_ptr(),
    get_extension: Some(host_get_extension),
    request_restart: Some(host_noop),
    request_process: Some(host_noop),
    request_callback: Some(host_request_callback),
};

struct Instance {
    plugin: *const clap_plugin,
    params: *const clap_plugin_params,
    state: *const clap_plugin_state,
}

impl Instance {
    fn new() -> Self {
        unsafe {
            let entry = &pixl::clap_entry;
            assert!((entry.init.unwrap())(c"".as_ptr()));
            let factory = (entry.get_factory.unwrap())(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory;
            assert!(!factory.is_null());
            assert_eq!(((*factory).get_plugin_count.unwrap())(factory), 1);
            let desc = ((*factory).get_plugin_descriptor.unwrap())(factory, 0);
            assert_eq!(CStr::from_ptr((*desc).id).to_str().unwrap(), "audio.pixl.pixl");
            assert_eq!(CStr::from_ptr((*desc).vendor).to_str().unwrap(), "PIXL Audio");
            let plugin = ((*factory).create_plugin.unwrap())(factory, &HOST, c"audio.pixl.pixl".as_ptr());
            assert!(!plugin.is_null());
            assert!(((*plugin).init.unwrap())(plugin));
            let params =
                ((*plugin).get_extension.unwrap())(plugin, CLAP_EXT_PARAMS.as_ptr()) as *const clap_plugin_params;
            let state = ((*plugin).get_extension.unwrap())(plugin, CLAP_EXT_STATE.as_ptr()) as *const clap_plugin_state;
            assert!(!params.is_null() && !state.is_null());
            Self { plugin, params, state }
        }
    }

    fn infos(&self) -> Vec<clap_param_info> {
        unsafe {
            let n = ((*self.params).count.unwrap())(self.plugin);
            (0..n)
                .map(|i| {
                    let mut info: clap_param_info = std::mem::zeroed();
                    assert!(((*self.params).get_info.unwrap())(self.plugin, i, &mut info));
                    info
                })
                .collect()
        }
    }

    fn info(&self, key: &str) -> clap_param_info {
        let name = ParamId::from_key(key).unwrap().spec().name;
        self.infos().into_iter().find(|i| cstr(&i.name) == name).unwrap()
    }

    fn value(&self, id: u32) -> f64 {
        let mut v = 0.0;
        unsafe { assert!(((*self.params).get_value.unwrap())(self.plugin, id, &mut v)) };
        v
    }

    fn text(&self, id: u32, value: f64) -> String {
        let mut buf = [0 as c_char; 256];
        unsafe {
            assert!(((*self.params).value_to_text.unwrap())(self.plugin, id, value, buf.as_mut_ptr(), 256));
        }
        cstr(&buf)
    }

    fn parse(&self, id: u32, text: &str) -> Option<f64> {
        let c = std::ffi::CString::new(text).unwrap();
        let mut v = 0.0;
        unsafe { ((*self.params).text_to_value.unwrap())(self.plugin, id, c.as_ptr(), &mut v).then_some(v) }
    }

    /// Set CLAP-space parameter values through `flush` (as a host does while not processing).
    fn flush(&self, values: &[(u32, f64)]) {
        let events: Vec<clap_event_param_value> = values
            .iter()
            .map(|(id, v)| clap_event_param_value {
                header: clap_event_header {
                    size: std::mem::size_of::<clap_event_param_value>() as u32,
                    time: 0,
                    space_id: CLAP_CORE_EVENT_SPACE_ID,
                    type_: CLAP_EVENT_PARAM_VALUE,
                    flags: 0,
                },
                param_id: *id,
                cookie: null_mut(),
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value: *v,
            })
            .collect();
        unsafe extern "C" fn size(list: *const clap_input_events) -> u32 {
            (*((*list).ctx as *const Vec<clap_event_param_value>)).len() as u32
        }
        unsafe extern "C" fn get(list: *const clap_input_events, index: u32) -> *const clap_event_header {
            let events: &Vec<clap_event_param_value> = &*((*list).ctx as *const Vec<clap_event_param_value>);
            &events[index as usize].header
        }
        unsafe extern "C" fn push(_list: *const clap_output_events, _e: *const clap_event_header) -> bool {
            true
        }
        let input = clap_input_events { ctx: &events as *const _ as *mut c_void, size: Some(size), get: Some(get) };
        let output = clap_output_events { ctx: null_mut(), try_push: Some(push) };
        unsafe { ((*self.params).flush.unwrap())(self.plugin, &input, &output) };
    }

    fn on_main_thread(&self) {
        unsafe { ((*self.plugin).on_main_thread.unwrap())(self.plugin) };
    }

    fn save(&self) -> Vec<u8> {
        unsafe extern "C" fn write(stream: *const clap_ostream, buffer: *const c_void, size: u64) -> i64 {
            let out = &mut *((*stream).ctx as *mut Vec<u8>);
            out.extend_from_slice(std::slice::from_raw_parts(buffer as *const u8, size as usize));
            size as i64
        }
        let mut data = Vec::new();
        let stream = clap_ostream { ctx: &mut data as *mut _ as *mut c_void, write: Some(write) };
        unsafe { assert!(((*self.state).save.unwrap())(self.plugin, &stream)) };
        data
    }

    fn load(&self, data: &[u8]) -> bool {
        struct Reader<'a> {
            data: &'a [u8],
            pos: usize,
        }
        unsafe extern "C" fn read(stream: *const clap_istream, buffer: *mut c_void, size: u64) -> i64 {
            let r = &mut *((*stream).ctx as *mut Reader);
            // Deliver in small chunks to exercise buffered reads.
            let n = (size as usize).min(r.data.len() - r.pos).min(17);
            std::ptr::copy_nonoverlapping(r.data[r.pos..].as_ptr(), buffer as *mut u8, n);
            r.pos += n;
            n as i64
        }
        let mut reader = Reader { data, pos: 0 };
        let stream = clap_istream { ctx: &mut reader as *mut _ as *mut c_void, read: Some(read) };
        unsafe { ((*self.state).load.unwrap())(self.plugin, &stream) }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            ((*self.plugin).destroy.unwrap())(self.plugin);
            (pixl::clap_entry.deinit.unwrap())();
        }
    }
}

fn cstr(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8(bytes).unwrap()
}

/// nih-plug's CLAP state: a little-endian `u64` length followed by that many bytes of JSON.
fn state_json(data: &[u8]) -> Value {
    let (len, json) = data.split_at(8);
    let len = u64::from_le_bytes(len.try_into().unwrap()) as usize;
    assert_eq!(len, json.len(), "state length prefix");
    serde_json::from_slice(json).expect("nih-plug state is JSON")
}

/// Inverse of [`state_json`].
fn state_bytes(json: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(json).unwrap();
    let mut out = (body.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    out
}

#[test]
fn parameter_info_matches_the_table() {
    let _guard = ENTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let inst = Instance::new();
    let infos = inst.infos();
    assert_eq!(infos.len(), PARAM_COUNT);
    for (info, spec) in infos.iter().zip(PARAMS.iter()) {
        assert_eq!(cstr(&info.name), spec.name);
        assert_eq!(cstr(&info.module), spec.group);
        let steps = spec.step_count();
        assert_eq!(info.flags & CLAP_PARAM_IS_STEPPED != 0, steps.is_some(), "{}", spec.key);
        let scale = steps.unwrap_or(1) as f64;
        assert_eq!(info.max_value, scale);
        assert!((info.default_value - spec.normalize(spec.default) as f64 * scale).abs() < 1e-6, "{}", spec.key);
        // Fresh instance: every value is the default, displayed with the table formatter
        // (canonical form; the default crush rate 11025 Hz lies exactly between two display
        // steps, so either neighbour is acceptable).
        let v = inst.value(info.id);
        assert!((v - info.default_value).abs() < 1e-6, "{}", spec.key);
        let text = inst.text(info.id, v);
        let shown = spec.parse(&text).unwrap_or_else(|| panic!("{}: '{text}'", spec.key));
        assert!((shown - spec.default).abs() <= 0.006 * (1.0 + spec.default.abs()), "{}: '{text}'", spec.key);
        assert_eq!(pixl::params::display_text(spec.id, shown), text, "{}", spec.key);
        let parsed = inst.parse(info.id, &spec.format(spec.default)).unwrap();
        assert!((parsed - v).abs() < 1e-3 * scale.max(1.0), "{}: {parsed} vs {v}", spec.key);
    }
}

#[test]
fn state_roundtrip_through_the_wrapper() {
    let _guard = ENTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = Instance::new();
    let chip = a.info("chip");
    let duty = a.info("p1_duty");
    let p1_on = a.info("p1_on");
    let sweep = a.info("sweep_time");
    a.flush(&[(chip.id, 2.0), (duty.id, 0.25), (p1_on.id, 1.0), (sweep.id, 0.6)]);
    assert_eq!(a.text(chip.id, a.value(chip.id)), "FREE");
    let saved = a.save();
    let json = state_json(&saved);
    let params = &json["params"];
    assert_eq!(params.as_object().unwrap().len(), PARAM_COUNT);
    assert_eq!(params["chip"], serde_json::json!({ "i32": 2 }));
    assert_eq!(params["p1_on"], serde_json::json!({ "bool": true }));
    let fields = json["fields"].as_object().unwrap();
    for key in ["wave", "macros", "preset", "ui"] {
        assert!(fields.contains_key(key), "missing persistent field {key}");
    }
    let preset: Value = serde_json::from_str(fields["preset"].as_str().unwrap()).unwrap();
    assert_eq!(preset["name"], "Hyrule Fanfare");
    assert_eq!(preset["modified"], true, "parameters changed since the init preset");

    // Edit the persistent fields the way a newer session would have saved them.
    let mut edited = json.clone();
    let wave: Vec<u8> = (0..32).map(|i| (i / 2) as u8).collect();
    edited["fields"]["wave"] = Value::String(serde_json::to_string(&wave).unwrap());
    edited["fields"]["macros"] = Value::String(r#"[[{"target":"p1_duty","depth":0.5}],[],[],[]]"#.into());
    edited["fields"]["ui"] = Value::String(r#"{"channel":"Noise","scale":1.25,"lang":"En"}"#.into());
    let edited_bytes = state_bytes(&edited);

    let b = Instance::new();
    assert!(b.load(&edited_bytes));
    for info in a.infos() {
        assert!((a.value(info.id) - b.value(info.id)).abs() < 1e-6, "{}", cstr(&info.name));
    }
    let resaved = state_json(&b.save());
    assert_eq!(resaved["params"], json["params"]);
    let f = &resaved["fields"];
    assert_eq!(serde_json::from_str::<Vec<u8>>(f["wave"].as_str().unwrap()).unwrap(), wave);
    let macros: Value = serde_json::from_str(f["macros"].as_str().unwrap()).unwrap();
    assert_eq!(macros[0][0]["target"], "p1_duty");
    let ui: Value = serde_json::from_str(f["ui"].as_str().unwrap()).unwrap();
    assert_eq!(ui["channel"], "Noise");
    assert_eq!(ui["scale"], 1.25);

    // Byte-identical state after load → save.
    let c = Instance::new();
    assert!(c.load(&saved));
    assert_eq!(c.save(), saved);
}

#[test]
fn foreign_and_damaged_states() {
    let _guard = ENTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = Instance::new();
    let fresh = state_json(&a.save());
    assert!(!a.load(b""));
    assert!(!a.load(b"\x00\x01garbage"));
    // What clap-validator's `state-invalid-random` sends: must be rejected, not abort.
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let random: Vec<u8> = (0..1 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect();
    assert!(!a.load(&random));
    // A truncated state and one whose length prefix lies are rejected; the instance keeps
    // its state.
    let saved = a.save();
    assert!(!a.load(&saved[..saved.len() - 10]));
    let mut lying = saved.clone();
    lying[..8].copy_from_slice(&(saved.len() as u64 * 4).to_le_bytes());
    assert!(!a.load(&lying));
    let mut not_json = saved.clone();
    not_json[8] = b'#';
    assert!(!a.load(&not_json));
    assert_eq!(a.save(), saved);
    // Trailing bytes after a valid record are ignored.
    let mut padded = saved.clone();
    padded.extend_from_slice(&[0xAB; 100]);
    assert!(a.load(&padded));
    assert_eq!(a.save(), saved);

    // A sparse state from another version: missing parameters take defaults, unknown ones
    // are ignored, typed mismatches are converted, persistent fields fall back to defaults.
    let chip = a.info("chip");
    a.flush(&[(chip.id, 0.0)]);
    let sparse = serde_json::json!({
        "version": "0.1.0",
        "params": {
            "p1_level": { "f32": 7.0 },
            "uni_voices": { "f32": 2.0 },
            "echo_fir": { "string": "CAVE" },
            "removed_param": { "f32": 1.0 }
        },
        "fields": { "wave": "not json", "ui": "{\"scale\": 99}" }
    });
    assert!(a.load(&state_bytes(&sparse)));
    let state = state_json(&a.save());
    let p = &state["params"];
    assert_eq!(p["chip"], serde_json::json!({ "i32": 1 }), "missing → default (GB), not the previous NES");
    assert_eq!(p["p1_level"], serde_json::json!({ "f32": 7.0 }));
    assert_eq!(p["uni_voices"], serde_json::json!({ "i32": 2 }));
    assert_eq!(p["echo_fir"], serde_json::json!({ "i32": 3 }));
    assert!(p.get("removed_param").is_none());
    let f = &state["fields"];
    assert_eq!(f["wave"], fresh["fields"]["wave"]);
    assert_eq!(f["macros"], fresh["fields"]["macros"]);
    let ui: Value = serde_json::from_str(f["ui"].as_str().unwrap()).unwrap();
    assert_eq!(ui["scale"], 2.0);
    let preset: Value = serde_json::from_str(f["preset"].as_str().unwrap()).unwrap();
    assert_eq!(preset["name"], "Hyrule Fanfare");
    assert_eq!(preset["modified"], true);
}

#[test]
fn text_conversions_are_stable_for_every_parameter() {
    let _guard = ENTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let inst = Instance::new();
    let mut failures = BTreeMap::new();
    for (info, spec) in inst.infos().iter().zip(PARAMS.iter()) {
        let steps = 50;
        for k in 0..=steps {
            let v = info.min_value + (info.max_value - info.min_value) * k as f64 / steps as f64;
            let text = inst.text(info.id, v);
            let Some(back) = inst.parse(info.id, &text) else {
                failures.insert(spec.key, format!("cannot parse '{text}'"));
                continue;
            };
            let text2 = inst.text(info.id, back);
            if text2 != text || inst.parse(info.id, &text2) != Some(back) {
                failures.insert(spec.key, format!("'{text}' -> {back} -> '{text2}'"));
            }
        }
        if let ParamKind::Enum { variants } = spec.kind {
            for (i, label) in variants.iter().enumerate() {
                let v = inst.parse(info.id, &label.to_lowercase()).unwrap();
                assert!((v - i as f64).abs() < 1e-4, "{}: '{label}' -> {v}", spec.key);
                assert_eq!(inst.text(info.id, v), *label);
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn host_is_told_to_rescan_values_after_a_state_load() {
    let _guard = ENTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let a = Instance::new();
    let chip = a.info("chip");
    a.flush(&[(chip.id, 0.0)]);
    let saved = a.save();

    // On the main thread: the rescan happens during `load`.
    let b = Instance::new();
    let before = VALUE_RESCANS.load(Ordering::SeqCst);
    assert!(b.load(&saved));
    assert_eq!(VALUE_RESCANS.load(Ordering::SeqCst), before + 1);
    assert_eq!(b.text(chip.id, b.value(chip.id)), "NES");
    // A rejected state changes nothing and triggers nothing.
    assert!(!b.load(b"junk"));
    assert_eq!(VALUE_RESCANS.load(Ordering::SeqCst), before + 1);

    // Off the main thread: deferred to the next `on_main_thread`.
    ON_MAIN_THREAD.store(false, Ordering::SeqCst);
    let requests = CALLBACK_REQUESTS.load(Ordering::SeqCst);
    let loaded = b.load(&saved);
    ON_MAIN_THREAD.store(true, Ordering::SeqCst);
    assert!(loaded);
    assert_eq!(VALUE_RESCANS.load(Ordering::SeqCst), before + 1, "not from the wrong thread");
    assert!(CALLBACK_REQUESTS.load(Ordering::SeqCst) > requests);
    b.on_main_thread();
    assert_eq!(VALUE_RESCANS.load(Ordering::SeqCst), before + 2);
    b.on_main_thread();
    assert_eq!(VALUE_RESCANS.load(Ordering::SeqCst), before + 2, "exactly once");
}

/// One input event of a processing block (each variant starts with its `clap_event_header`).
enum BlockEvent {
    Note(clap_event_note),
    Expression(clap_event_note_expression),
    Midi(clap_event_midi),
}

impl BlockEvent {
    fn header(&self) -> *const clap_event_header {
        match self {
            BlockEvent::Note(e) => &e.header,
            BlockEvent::Expression(e) => &e.header,
            BlockEvent::Midi(e) => &e.header,
        }
    }
}

fn header(size: usize, time: u32, type_: u16) -> clap_event_header {
    clap_event_header { size: size as u32, time, space_id: CLAP_CORE_EVENT_SPACE_ID, type_, flags: 0 }
}

/// A note-on exactly as clap-wrapper's AUv2 layer emits it in the CLAP dialect.
fn wrapper_note_on(time: u32, key: i16, velocity: f64) -> BlockEvent {
    BlockEvent::Note(clap_event_note {
        header: header(std::mem::size_of::<clap_event_note>(), time, CLAP_EVENT_NOTE_ON),
        note_id: -1,
        port_index: 0,
        channel: 0,
        key,
        velocity,
    })
}

/// A note expression as clap-wrapper's AUv2 layer emits it: pitch bend → TUNING with
/// `(bend14 − 8192) / 8192 × 2` semitones, channel pressure → PRESSURE `value / 127`, both with
/// key −1 and note id −1 (`key` ≥ 0 makes it per-note, like poly pressure).
fn wrapper_expression(time: u32, expression_id: i32, key: i16, value: f64) -> BlockEvent {
    BlockEvent::Expression(clap_event_note_expression {
        header: header(std::mem::size_of::<clap_event_note_expression>(), time, CLAP_EVENT_NOTE_EXPRESSION),
        expression_id,
        note_id: -1,
        port_index: 0,
        channel: 0,
        key,
        value,
    })
}

fn wrapper_bend(time: u32, bend14: i32) -> BlockEvent {
    wrapper_expression(time, CLAP_NOTE_EXPRESSION_TUNING, -1, (bend14 - 8192) as f64 / 8192.0 * 2.0)
}

fn midi(time: u32, data: [u8; 3]) -> BlockEvent {
    BlockEvent::Midi(clap_event_midi {
        header: header(std::mem::size_of::<clap_event_midi>(), time, CLAP_EVENT_MIDI),
        port_index: 0,
        data,
    })
}

impl Instance {
    fn note_ports(&self) -> *const clap_plugin_note_ports {
        unsafe {
            ((*self.plugin).get_extension.unwrap())(self.plugin, CLAP_EXT_NOTE_PORTS.as_ptr())
                as *const clap_plugin_note_ports
        }
    }

    fn activate(&self, sample_rate: f64, max_frames: u32) {
        unsafe {
            assert!(((*self.plugin).activate.unwrap())(self.plugin, sample_rate, 1, max_frames));
            assert!(((*self.plugin).start_processing.unwrap())(self.plugin));
        }
    }

    fn deactivate(&self) {
        unsafe {
            ((*self.plugin).stop_processing.unwrap())(self.plugin);
            ((*self.plugin).deactivate.unwrap())(self.plugin);
        }
    }

    /// One stereo `process` call of `frames` frames with `events`; returns the left channel.
    fn process(&self, frames: u32, events: &[BlockEvent]) -> Vec<f32> {
        unsafe extern "C" fn size(list: *const clap_input_events) -> u32 {
            let events: &&[BlockEvent] = &*((*list).ctx as *const &[BlockEvent]);
            events.len() as u32
        }
        unsafe extern "C" fn get(list: *const clap_input_events, index: u32) -> *const clap_event_header {
            let events: &&[BlockEvent] = &*((*list).ctx as *const &[BlockEvent]);
            events[index as usize].header()
        }
        unsafe extern "C" fn push(_list: *const clap_output_events, _e: *const clap_event_header) -> bool {
            true
        }
        let list: &[BlockEvent] = events;
        let input = clap_input_events { ctx: &list as *const _ as *mut c_void, size: Some(size), get: Some(get) };
        let output = clap_output_events { ctx: null_mut(), try_push: Some(push) };
        let (mut l, mut r) = (vec![0.0f32; frames as usize], vec![0.0f32; frames as usize]);
        let mut channels = [l.as_mut_ptr(), r.as_mut_ptr()];
        let mut out = clap_audio_buffer {
            data32: channels.as_mut_ptr(),
            data64: null_mut(),
            channel_count: 2,
            latency: 0,
            constant_mask: 0,
        };
        let process = clap_process {
            steady_time: -1,
            frames_count: frames,
            transport: null(),
            audio_inputs: null(),
            audio_outputs: &mut out,
            audio_inputs_count: 0,
            audio_outputs_count: 1,
            in_events: &input,
            out_events: &output,
        };
        unsafe { ((*self.plugin).process.unwrap())(self.plugin, &process) };
        l
    }
}

#[test]
fn pitch_wheel_and_channel_pressure_work_through_clap_wrappers_auv2_translation() {
    // PLG-01 / SPEC §8: Logic (AUv2 via clap-wrapper) must bend and add aftertouch vibrato.
    let _guard = ENTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let inst = Instance::new();
    // The note port still speaks both dialects but prefers raw MIDI, so clap-wrapper forwards
    // bend and pressure as MIDI instead of translating them into channel-wide expressions.
    unsafe {
        let ports = inst.note_ports();
        assert!(!ports.is_null());
        assert_eq!(((*ports).count.unwrap())(inst.plugin, true), 1);
        let mut info: clap_note_port_info = std::mem::zeroed();
        assert!(((*ports).get.unwrap())(inst.plugin, 0, true, &mut info));
        assert_eq!(info.preferred_dialect, CLAP_NOTE_DIALECT_MIDI);
        assert_eq!(info.supported_dialects & (CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI), 3);
    }
    // A clean FREE patch (exact pitch): wave channel only, no pitch effects, ±2 st bend range.
    let set = |key: &str, plain: f64| (inst.info(key).id, plain);
    inst.flush(&[
        set("chip", 2.0),
        set("sweep_on", 0.0),
        set("vib_on", 0.0),
        set("glide_on", 0.0),
        set("uni_on", 0.0),
        set("arp_on", 0.0),
        set("crush_on", 0.0),
        set("echo_on", 0.0),
    ]);
    assert_eq!(inst.text(inst.info("pb_range").id, inst.value(inst.info("pb_range").id)), "+2 st");
    const SR: f32 = 48_000.0;
    const BLOCK: u32 = 480;
    inst.activate(SR as f64, BLOCK);
    // Renders `blocks` blocks, `first` delivered in the first one.
    let run = |blocks: usize, first: Vec<BlockEvent>| {
        let mut out = inst.process(BLOCK, &first);
        for _ in 1..blocks {
            out.extend(inst.process(BLOCK, &[]));
        }
        out
    };
    // Pitch of the steady last two thirds of a segment.
    let pitch_of = |x: &[f32]| common::pitch(&x[x.len() / 3..], SR, 450.0);
    // A4, then the wheel fully up as a channel-wide TUNING expression (CLAP dialect).
    let mut segments = vec![("A4", run(30, vec![wrapper_note_on(0, 69, 0.8)]), 440.0)];
    segments.push(("wheel up (expression)", run(30, vec![wrapper_bend(17, 16383)]), 493.883));
    // The same wheel position delivered both as raw MIDI and as an expression: one bend, not two.
    let both = vec![midi(3, [0xE0, 0x7F, 0x7F]), wrapper_bend(3, 16383)];
    segments.push(("wheel up (MIDI + expression)", run(30, both), 493.883));
    segments.push(("wheel centre (MIDI)", run(30, vec![midi(0, [0xE0, 0x00, 0x40])]), 440.0));
    // A per-note tuning expression is not the channel's wheel.
    let per_note = vec![wrapper_expression(0, CLAP_NOTE_EXPRESSION_TUNING, 69, 1.0)];
    segments.push(("per-note tuning", run(30, per_note), 440.0));
    for (name, x, hz) in &segments {
        let f = pitch_of(x);
        assert!(common::cents(f, *hz).abs() < 3.0, "{name}: {f:.2} Hz, expected {hz} Hz");
    }
    // Channel pressure (expression and raw MIDI) adds the +50 c aftertouch vibrato.
    let spread = |x: &[f32]| {
        let w = (SR * 0.03) as usize;
        let f: Vec<f32> = x.chunks(w).filter(|c| c.len() == w).map(|c| common::acf_pitch(c, SR, 440.0)).collect();
        f.iter().cloned().fold(f32::MIN, f32::max) / f.iter().cloned().fold(f32::MAX, f32::min)
    };
    let calm = spread(&segments[4].1);
    let pressed = spread(&run(60, vec![wrapper_expression(0, CLAP_NOTE_EXPRESSION_PRESSURE, -1, 1.0)])[9_600..]);
    let released = spread(&run(60, vec![wrapper_expression(0, CLAP_NOTE_EXPRESSION_PRESSURE, -1, 0.0)])[9_600..]);
    let pressed_midi = spread(&run(60, vec![midi(0, [0xD0, 0x7F, 0x00])])[9_600..]);
    println!(
        "pitch spread: calm {calm:.4}, pressure {pressed:.4}, released {released:.4}, MIDI pressure {pressed_midi:.4}"
    );
    assert!(calm < 1.003 && released < 1.003, "no vibrato without pressure: {calm} {released}");
    assert!(pressed > 1.02 && pressed_midi > 1.02, "aftertouch vibrato: {pressed} {pressed_midi}");
    inst.deactivate();
}
