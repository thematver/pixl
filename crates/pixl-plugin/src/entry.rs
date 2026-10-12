//! The CLAP entry point (`clap_entry`) with a hardened `clap.state` extension.
//!
//! This is what `nih_export_clap!(Pixl)` generates, plus two fixes to nih-plug's CLAP state
//! handling and one to its note port:
//!
//! 1. **Damaged state cannot abort the host.** nih-plug's loader reads an 8-byte length
//!    prefix from the stream and reserves that many bytes up front, so a damaged project or
//!    preset chunk (or random data, as clap-validator's `state-invalid-random` test sends)
//!    makes it attempt a multi-exabyte allocation, which aborts the process. Here the stream
//!    is first read into memory in bounded chunks, the prefix is validated against
//!    [`MAX_STATE_BYTES`] and the data actually present, and only then is an in-memory copy
//!    passed to nih-plug's loader.
//! 2. **Hosts learn about the new values.** After a host-initiated `load` nih-plug does not
//!    tell the host that parameter values changed. CLAP's `CLAP_PARAM_RESCAN_VALUES` exists
//!    for exactly this ("after loading a preset"), so after a successful load the host's
//!    `clap.params` `rescan` is called, on the main thread (deferred through
//!    `request_callback` → `on_main_thread` if the load happened elsewhere).
//! 3. **The pitch wheel and channel pressure work in Logic.** nih-plug's note input port
//!    prefers the CLAP note dialect. clap-wrapper's AUv2 layer (and AUv3) honours that by
//!    turning MIDI pitch bend and channel pressure into channel-wide CLAP note expressions
//!    (tuning ±2 st, pressure, key −1), which nih-plug only forwards as per-note `PolyTuning`/
//!    `PolyPressure` events. The proxied `clap.note-ports` keeps nih-plug's port (supported:
//!    CLAP and MIDI) but prefers the **MIDI** dialect, so those layers forward raw MIDI and
//!    nih-plug delivers `MidiPitchBend`/`MidiChannelPressure`, exactly like the VST3 layer
//!    and MIDI-speaking CLAP hosts. (The MIDI mapping also understands the expression form,
//!    see [`crate::midi`].)
//!
//! Mechanism: every instance is handed to the host behind a thin proxy `clap_plugin`. The
//! proxy shares nih-plug's `plugin_data`, and nih-plug's callbacks only ever read
//! `(*plugin).plugin_data`, so all function pointers except `destroy`, `get_extension` and
//! `on_main_thread` are nih-plug's own and work unchanged with the proxy pointer.
//! `get_extension("clap.state")` returns [`HARDENED_STATE`] (`save` is forwarded as is) and
//! `get_extension("clap.note-ports")` returns [`MIDI_NOTE_PORTS`].
//!
//! clap-wrapper links this symbol into the CLAP, VST3 and AUv2 bundles (`wrapper/`), so the
//! fixes apply to every format.

use crate::Pixl;
use clap_sys::ext::note_ports::{
    clap_note_port_info, clap_plugin_note_ports, CLAP_EXT_NOTE_PORTS, CLAP_NOTE_DIALECT_MIDI,
};
use clap_sys::ext::params::{clap_host_params, CLAP_EXT_PARAMS, CLAP_PARAM_RESCAN_VALUES};
use clap_sys::ext::state::{clap_host_state, clap_plugin_state, CLAP_EXT_STATE};
use clap_sys::ext::thread_check::{clap_host_thread_check, CLAP_EXT_THREAD_CHECK};
use clap_sys::stream::{clap_istream, clap_ostream};
use nih_plug::wrapper::clap::{
    clap_host, clap_plugin, clap_plugin_descriptor, clap_plugin_entry, clap_plugin_factory, PluginDescriptor, Wrapper,
    CLAP_PLUGIN_FACTORY_ID, CLAP_VERSION,
};
use nih_plug::wrapper::setup_logger;
use std::ffi::{c_char, c_void, CStr};
use std::ptr::null;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

/// Largest serialized state accepted by `clap.state` `load` (bytes after the length prefix).
/// A PIXL state is about 10 KB; anything this large is not ours.
pub const MAX_STATE_BYTES: u64 = 16 << 20;

/// Chunk size used while reading a state stream (bounds the up-front allocation).
const READ_CHUNK: usize = 64 << 10;

/// The CLAP plugin entry point (the symbol clap-wrapper and CLAP hosts look up).
#[no_mangle]
#[used]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(init),
    deinit: Some(deinit),
    get_factory: Some(get_factory),
};

const FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(get_plugin_count),
    get_plugin_descriptor: Some(get_plugin_descriptor),
    create_plugin: Some(create_plugin),
};

/// The `clap.state` extension handed to hosts (see the module docs).
static HARDENED_STATE: clap_plugin_state = clap_plugin_state { save: Some(state_save), load: Some(state_load) };

/// The `clap.note-ports` extension handed to hosts: nih-plug's ports, input ports preferring
/// the MIDI dialect (see the module docs).
static MIDI_NOTE_PORTS: clap_plugin_note_ports =
    clap_plugin_note_ports { count: Some(note_ports_count), get: Some(note_ports_get) };

/// Private, versioned ABI shared with wrapper/pixl_factory_presets.h. All calls are
/// main-thread only. Preset numbers are permanent library numbers, never list indices.
#[repr(C)]
struct FactoryPresetInfo {
    number: i32,
    name: [c_char; 256],
}

#[repr(C)]
struct FactoryPresets {
    count: unsafe extern "C" fn(*const clap_plugin) -> u32,
    get: unsafe extern "C" fn(*const clap_plugin, u32, *mut FactoryPresetInfo) -> bool,
    load: unsafe extern "C" fn(*const clap_plugin, i32) -> bool,
    current: unsafe extern "C" fn(*const clap_plugin, *mut FactoryPresetInfo) -> bool,
}

static FACTORY_PRESETS: FactoryPresets = FactoryPresets {
    count: factory_preset_count,
    get: factory_preset_get,
    load: factory_preset_load,
    current: factory_preset_current,
};

fn preset_info(info: &mut FactoryPresetInfo, number: i32, name: &str) {
    info.number = number;
    info.name.fill(0);
    let mut len = name.len().min(info.name.len() - 1);
    while !name.is_char_boundary(len) { len -= 1; }
    for (dst, src) in info.name.iter_mut().zip(name.as_bytes()[..len].iter()) { *dst = *src as c_char; }
}

unsafe extern "C" fn factory_preset_current(plugin: *const clap_plugin, info: *mut FactoryPresetInfo) -> bool {
    let Some(proxy) = (unsafe { proxy_of(plugin) }) else { return false };
    let Some(info) = (unsafe { info.as_mut() }) else { return false };
    let state = unsafe { &*proxy.wrapper }.get_state_object();
    let Some(meta) = state.fields.get(crate::state::FIELD_PRESET)
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) else { return false };
    let Some(name) = meta["name"].as_str() else { return false };
    let number = if meta["id"].as_str().is_some_and(|id| id.starts_with("factory:")) && meta["modified"] == false {
        meta["number"].as_i64().unwrap_or(-1) as i32
    } else { -1 };
    preset_info(info, number, name);
    true
}

unsafe extern "C" fn factory_preset_count(_plugin: *const clap_plugin) -> u32 {
    crate::presets::factory_library().presets.len() as u32
}

unsafe extern "C" fn factory_preset_get(
    _plugin: *const clap_plugin,
    index: u32,
    info: *mut FactoryPresetInfo,
) -> bool {
    let Some(preset) = crate::presets::factory_library().presets.get(index as usize) else { return false };
    let Some(info) = (unsafe { info.as_mut() }) else { return false };
    // Truncate only on a UTF-8 boundary and always terminate the C string.
    preset_info(info, preset.preset.number as i32, &preset.preset.name);
    true
}

unsafe extern "C" fn factory_preset_load(plugin: *const clap_plugin, number: i32) -> bool {
    let Some(proxy) = (unsafe { proxy_of(plugin) }) else { return false };
    let Some(preset) = crate::presets::factory_library().presets.iter().find(|p| p.preset.number as i32 == number)
        else { return false };
    // Keep instance preferences (language, scale, selected channel), replace the entire
    // sound including wave RAM and macro routes through the same API as host recall.
    let wrapper = unsafe { &*proxy.wrapper };
    let mut state = wrapper.get_state_object();
    state.params = pixl_core::PARAMS.iter().map(|p| {
        (p.key.to_string(), crate::state::plain_to_param_value(p.id, preset.state.values.get(p.id)))
    }).collect();
    state.fields.insert(crate::state::FIELD_WAVE.into(), serde_json::to_string(&preset.state.wave).unwrap());
    state.fields.insert(crate::state::FIELD_MACROS.into(), serde_json::to_string(&preset.state.macros).unwrap());
    let meta = crate::state::PresetMetaRepr {
        id: Some(preset.id.clone()), name: preset.preset.name.clone(), category: preset.preset.category,
        number: preset.preset.number, modified: false,
    };
    state.fields.insert(crate::state::FIELD_PRESET.into(), serde_json::to_string(&meta).unwrap());
    let loaded = wrapper.set_state_inner(&mut state);
    if loaded { unsafe { notify_values_changed(proxy); } }
    loaded
}

fn descriptor() -> &'static PluginDescriptor {
    static DESCRIPTOR: OnceLock<PluginDescriptor> = OnceLock::new();
    DESCRIPTOR.get_or_init(PluginDescriptor::for_plugin::<Pixl>)
}

extern "C" fn init(_plugin_path: *const c_char) -> bool {
    setup_logger();
    true
}

extern "C" fn deinit() {}

unsafe extern "C" fn get_factory(factory_id: *const c_char) -> *const c_void {
    // SAFETY: the host passes a valid C string (checked for null).
    if !factory_id.is_null() && unsafe { CStr::from_ptr(factory_id) } == CLAP_PLUGIN_FACTORY_ID {
        &FACTORY as *const clap_plugin_factory as *const c_void
    } else {
        null()
    }
}

unsafe extern "C" fn get_plugin_count(_factory: *const clap_plugin_factory) -> u32 {
    1
}

unsafe extern "C" fn get_plugin_descriptor(
    _factory: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    if index == 0 {
        descriptor().clap_plugin_descriptor()
    } else {
        null()
    }
}

/// What the host sees as the plugin instance.
#[repr(C)]
struct PluginProxy {
    /// Must stay the first field: the host's `*const clap_plugin` is cast back to the proxy.
    plugin: clap_plugin,
    /// nih-plug's own `clap_plugin` (owned by its `Wrapper`, released in [`destroy`]).
    inner: *const clap_plugin,
    /// The host that created the instance (outlives it, per the CLAP contract).
    host: *const clap_host,
    /// A value rescan is due on the next `on_main_thread` call.
    rescan_pending: AtomicBool,
    /// Keeps access to nih-plug's state API for native AU factory preset selection.
    wrapper: *const Wrapper<Pixl>,
    notifications: Arc<crate::host::HostNotifications>,
}

unsafe extern "C" fn create_plugin(
    _factory: *const clap_plugin_factory,
    host: *const clap_host,
    plugin_id: *const c_char,
) -> *const clap_plugin {
    if plugin_id.is_null() || host.is_null() {
        return null();
    }
    // SAFETY: non-null C string from the host.
    if unsafe { CStr::from_ptr(plugin_id) } != descriptor().clap_id() {
        return null();
    }
    // The Arc is turned back into an owned value (and dropped) by nih-plug's `destroy`.
    let wrapper = Wrapper::<Pixl>::new(host);
    let notifications = Arc::new(crate::host::HostNotifications::new(host));
    (wrapper.task_executor.lock())(crate::host::PluginTask::AttachHost(notifications.clone()));
    let wrapper = Arc::into_raw(wrapper);
    // SAFETY: the pointer comes from the leaked Arc above and stays valid until `destroy`.
    let inner: *const clap_plugin = unsafe { (*wrapper).clap_plugin.as_ptr() };
    // SAFETY: `inner` points to nih-plug's fully initialized `clap_plugin`.
    let i = unsafe { &*inner };
    let proxy = Box::new(PluginProxy {
        plugin: clap_plugin {
            desc: i.desc,
            plugin_data: i.plugin_data,
            init: i.init,
            destroy: Some(destroy),
            activate: i.activate,
            deactivate: i.deactivate,
            start_processing: i.start_processing,
            stop_processing: i.stop_processing,
            reset: i.reset,
            process: i.process,
            get_extension: Some(get_extension),
            on_main_thread: Some(on_main_thread),
        },
        inner,
        host,
        rescan_pending: AtomicBool::new(false),
        wrapper,
        notifications,
    });
    Box::into_raw(proxy) as *const clap_plugin
}

/// The proxy behind a pointer handed out by [`create_plugin`].
///
/// # Safety
/// `plugin` must be null or a pointer returned by [`create_plugin`] that was not destroyed.
unsafe fn proxy_of<'a>(plugin: *const clap_plugin) -> Option<&'a PluginProxy> {
    // SAFETY: per the contract, `plugin` is the first field of a live `PluginProxy`.
    let proxy = unsafe { (plugin as *const PluginProxy).as_ref()? };
    (!proxy.inner.is_null()).then_some(proxy)
}

unsafe extern "C" fn destroy(plugin: *const clap_plugin) {
    if plugin.is_null() {
        return;
    }
    // SAFETY: `plugin` was created by `create_plugin` via `Box::into_raw`; the host destroys
    // each instance exactly once.
    let proxy = unsafe { Box::from_raw(plugin as *mut PluginProxy) };
    proxy.notifications.detach();
    // SAFETY: `inner` is nih-plug's live plugin; its `destroy` releases the wrapper.
    unsafe {
        if let Some(destroy_inner) = (*proxy.inner).destroy {
            destroy_inner(proxy.inner);
        }
    }
}

unsafe extern "C" fn on_main_thread(plugin: *const clap_plugin) {
    // SAFETY: host contract: `plugin` is our live proxy; called on the main thread.
    unsafe {
        let Some(proxy) = proxy_of(plugin) else { return };
        if proxy.rescan_pending.swap(false, Ordering::AcqRel) {
            rescan_values(proxy.host);
        }
        if let Some(f) = (*proxy.inner).on_main_thread {
            f(proxy.inner);
        }
        if proxy.notifications.take() {
            if proxy.notifications.take_preset() { rescan_values(proxy.host); }
            if let Some(mark_dirty) = host_extension::<clap_host_state>(proxy.host, CLAP_EXT_STATE).and_then(|s| s.mark_dirty) {
                mark_dirty(proxy.host);
            }
        }
    }
}

/// A host extension, if the host provides it.
///
/// # Safety
/// `host` must be a live host.
unsafe fn host_extension<T>(host: *const clap_host, id: &CStr) -> Option<&'static T> {
    // SAFETY: per the contract; hosts return null or a pointer to a static extension table.
    unsafe {
        let get = (*host).get_extension?;
        (get(host, id.as_ptr()) as *const T).as_ref()
    }
}

/// Tell the host that every parameter value may have changed. Main thread only.
///
/// # Safety
/// `host` must be a live host and this must run on its main thread.
unsafe fn rescan_values(host: *const clap_host) {
    // SAFETY: per the contract.
    unsafe {
        if let Some(rescan) = host_extension::<clap_host_params>(host, CLAP_EXT_PARAMS).and_then(|p| p.rescan) {
            rescan(host, CLAP_PARAM_RESCAN_VALUES);
        }
    }
}

/// Schedule [`rescan_values`]: immediately when on the host's main thread (CLAP's contract
/// for `state.load`; hosts without `clap.thread-check` are trusted on that), otherwise from
/// the next `on_main_thread` callback.
///
/// # Safety
/// `proxy` must be live.
unsafe fn notify_values_changed(proxy: &PluginProxy) {
    // SAFETY: the host outlives its plugin instances.
    unsafe {
        let on_main = host_extension::<clap_host_thread_check>(proxy.host, CLAP_EXT_THREAD_CHECK)
            .and_then(|t| t.is_main_thread)
            .is_none_or(|is_main_thread| is_main_thread(proxy.host));
        if on_main {
            rescan_values(proxy.host);
        } else {
            proxy.rescan_pending.store(true, Ordering::Release);
            if let Some(request_callback) = (*proxy.host).request_callback {
                request_callback(proxy.host);
            }
        }
    }
}

/// nih-plug's own `clap.state` extension of `inner` (a copy of its function table).
///
/// # Safety
/// `inner` must be a live nih-plug `clap_plugin`.
unsafe fn inner_state(inner: *const clap_plugin) -> Option<clap_plugin_state> {
    // SAFETY: per the contract; nih-plug returns null or a pointer into its live wrapper.
    unsafe {
        let get_extension = (*inner).get_extension?;
        let ext = get_extension(inner, CLAP_EXT_STATE.as_ptr()) as *const clap_plugin_state;
        ext.as_ref().copied()
    }
}

/// nih-plug's own `clap.note-ports` extension of `inner` (a copy of its function table).
///
/// # Safety
/// `inner` must be a live nih-plug `clap_plugin`.
unsafe fn inner_note_ports(inner: *const clap_plugin) -> Option<clap_plugin_note_ports> {
    // SAFETY: per the contract; nih-plug returns null or a pointer into its live wrapper.
    unsafe {
        let get_extension = (*inner).get_extension?;
        let ext = get_extension(inner, CLAP_EXT_NOTE_PORTS.as_ptr()) as *const clap_plugin_note_ports;
        ext.as_ref().copied()
    }
}

unsafe extern "C" fn note_ports_count(plugin: *const clap_plugin, is_input: bool) -> u32 {
    // SAFETY: host contract (our live proxy); forwarded unchanged.
    unsafe {
        let Some(proxy) = proxy_of(plugin) else { return 0 };
        match inner_note_ports(proxy.inner).and_then(|p| p.count) {
            Some(count) => count(proxy.inner, is_input),
            None => 0,
        }
    }
}

unsafe extern "C" fn note_ports_get(
    plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    // SAFETY: host contract (our live proxy, `info` valid for writes or null).
    unsafe {
        let Some(proxy) = proxy_of(plugin) else { return false };
        let Some(get) = inner_note_ports(proxy.inner).and_then(|p| p.get) else { return false };
        if info.is_null() || !get(proxy.inner, index, is_input, info) {
            return false;
        }
        let info = &mut *info;
        if is_input && info.supported_dialects & CLAP_NOTE_DIALECT_MIDI != 0 {
            info.preferred_dialect = CLAP_NOTE_DIALECT_MIDI;
        }
        true
    }
}

unsafe extern "C" fn get_extension(plugin: *const clap_plugin, id: *const c_char) -> *const c_void {
    // SAFETY: host contract: `plugin` is our proxy, `id` a C string or null.
    unsafe {
        let Some(proxy) = proxy_of(plugin) else { return null() };
        if id.is_null() {
            return null();
        }
        if CStr::from_ptr(id) == CLAP_EXT_STATE {
            return match inner_state(proxy.inner) {
                Some(_) => &HARDENED_STATE as *const clap_plugin_state as *const c_void,
                None => null(),
            };
        }
        if CStr::from_ptr(id) == c"audio.pixl.factory-presets/1" {
            return &FACTORY_PRESETS as *const FactoryPresets as *const c_void;
        }
        if CStr::from_ptr(id) == CLAP_EXT_NOTE_PORTS {
            return match inner_note_ports(proxy.inner) {
                Some(_) => &MIDI_NOTE_PORTS as *const clap_plugin_note_ports as *const c_void,
                None => null(),
            };
        }
        match (*proxy.inner).get_extension {
            Some(f) => f(proxy.inner, id),
            None => null(),
        }
    }
}

unsafe extern "C" fn state_save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    // SAFETY: host contract (our proxy, a valid stream); forwarded unchanged.
    unsafe {
        let Some(proxy) = proxy_of(plugin) else { return false };
        match inner_state(proxy.inner).and_then(|s| s.save) {
            Some(save) if !stream.is_null() => save(proxy.inner, stream),
            _ => false,
        }
    }
}

unsafe extern "C" fn state_load(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    // SAFETY: host contract (our proxy, a valid stream or null).
    unsafe {
        let Some(proxy) = proxy_of(plugin) else { return false };
        let Some(load) = inner_state(proxy.inner).and_then(|s| s.load) else { return false };
        let Some(stream) = stream.as_ref() else { return false };
        let Some(data) = read_state(|buf| read_some(stream, buf)) else {
            nih_plug::nih_warn!("PIXL: rejected a damaged or foreign plugin state");
            return false;
        };
        let mut reader = MemoryReader { data: &data, pos: 0 };
        let memory = clap_istream { ctx: &mut reader as *mut MemoryReader as *mut c_void, read: Some(memory_read) };
        let loaded = load(proxy.inner, &memory);
        if loaded {
            notify_values_changed(proxy);
        }
        loaded
    }
}

/// One `read` call on a host stream: bytes read (≤ `buf.len()`), 0 at the end, `None` on
/// error.
///
/// # Safety
/// `stream` must be a valid host stream.
unsafe fn read_some(stream: &clap_istream, buf: &mut [u8]) -> Option<usize> {
    let read = stream.read?;
    // SAFETY: `buf` is valid for `buf.len()` bytes of writes.
    let n = unsafe { read(stream, buf.as_mut_ptr() as *mut c_void, buf.len() as u64) };
    match n {
        n if n < 0 => None,
        n => Some((n as u64).min(buf.len() as u64) as usize),
    }
}

/// Read one nih-plug state record (`u64` little-endian length + that many bytes) with
/// `read`, which behaves like a stream `read` call. Returns the record (prefix included)
/// only if it is complete and its length is plausible; never allocates more than the data
/// actually delivered plus one chunk.
pub(crate) fn read_state(mut read: impl FnMut(&mut [u8]) -> Option<usize>) -> Option<Vec<u8>> {
    let mut prefix = [0u8; 8];
    let mut got = 0;
    while got < prefix.len() {
        match read(&mut prefix[got..])? {
            0 => return None,
            n => got += n,
        }
    }
    let len = u64::from_le_bytes(prefix);
    if len == 0 || len > MAX_STATE_BYTES {
        return None;
    }
    let total = 8 + len as usize;
    let mut data = Vec::with_capacity(total.min(8 + READ_CHUNK));
    data.extend_from_slice(&prefix);
    let mut chunk = vec![0u8; READ_CHUNK.min(total - 8)];
    while data.len() < total {
        let want = (total - data.len()).min(chunk.len());
        match read(&mut chunk[..want])? {
            0 => return None,
            n => data.extend_from_slice(&chunk[..n.min(want)]),
        }
    }
    Some(data)
}

/// In-memory `clap_istream` source.
struct MemoryReader<'a> {
    data: &'a [u8],
    pos: usize,
}

unsafe extern "C" fn memory_read(stream: *const clap_istream, buffer: *mut c_void, size: u64) -> i64 {
    // SAFETY: `ctx` is the `MemoryReader` set up in `state_load`, alive for the whole call;
    // `buffer` is valid for `size` bytes (caller contract).
    unsafe {
        if stream.is_null() || buffer.is_null() {
            return -1;
        }
        let reader = &mut *((*stream).ctx as *mut MemoryReader);
        let remaining = reader.data.len().saturating_sub(reader.pos);
        let n = remaining.min(size.min(usize::MAX as u64) as usize);
        std::ptr::copy_nonoverlapping(reader.data.as_ptr().add(reader.pos), buffer as *mut u8, n);
        reader.pos += n;
        n as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream over `data` that delivers at most `chunk` bytes per call.
    fn reader(data: &[u8], chunk: usize) -> impl FnMut(&mut [u8]) -> Option<usize> + '_ {
        let mut pos = 0;
        move |buf| {
            let n = buf.len().min(chunk).min(data.len() - pos);
            buf[..n].copy_from_slice(&data[pos..pos + n]);
            pos += n;
            Some(n)
        }
    }

    fn record(body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u64).to_le_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn valid_records_are_read_exactly() {
        let body = br#"{"version":"1.0.0","params":{},"fields":{}}"#;
        for chunk in [1, 3, 17, 4096] {
            let mut data = record(body);
            assert_eq!(read_state(reader(&data, chunk)).as_deref(), Some(data.as_slice()));
            // Trailing bytes after the record are left unread, like nih-plug does.
            data.extend_from_slice(b"trailing");
            assert_eq!(read_state(reader(&data, chunk)).map(|d| d.len()), Some(8 + body.len()));
        }
        let big = vec![b' '; 3 * READ_CHUNK + 5];
        assert_eq!(read_state(reader(&record(&big), 1000)).map(|d| d.len()), Some(8 + big.len()));
    }

    #[test]
    fn damaged_records_are_rejected_without_huge_allocations() {
        // Empty, truncated prefix, absurd length, zero length, length beyond the data.
        assert_eq!(read_state(reader(b"", 8)), None);
        assert_eq!(read_state(reader(b"\x05\x00\x00", 8)), None);
        let mut absurd = u64::MAX.to_le_bytes().to_vec();
        absurd.extend_from_slice(b"{}");
        assert_eq!(read_state(reader(&absurd, 8)), None);
        let mut too_big = (MAX_STATE_BYTES + 1).to_le_bytes().to_vec();
        too_big.extend_from_slice(&[b' '; 64]);
        assert_eq!(read_state(reader(&too_big, 8)), None);
        assert_eq!(read_state(reader(&0u64.to_le_bytes(), 8)), None);
        let mut short = 1000u64.to_le_bytes().to_vec();
        short.extend_from_slice(&[b'x'; 999]);
        assert_eq!(read_state(reader(&short, 64)), None);
        // Read errors abort.
        assert_eq!(read_state(|_| None), None);
        // Pseudo-random megabyte (what clap-validator sends).
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let random: Vec<u8> = (0..1 << 20)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        assert_eq!(read_state(reader(&random, 4096)), None);
    }

    #[test]
    fn memory_stream_delivers_everything() {
        let data: Vec<u8> = (0..=255).collect();
        let mut r = MemoryReader { data: &data, pos: 0 };
        let stream = clap_istream { ctx: &mut r as *mut MemoryReader as *mut c_void, read: Some(memory_read) };
        let mut out = Vec::new();
        let mut buf = [0u8; 100];
        loop {
            // SAFETY: valid stream and buffer.
            let n = unsafe { memory_read(&stream, buf.as_mut_ptr() as *mut c_void, buf.len() as u64) };
            assert!(n >= 0);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        assert_eq!(out, data);
    }
}
