//! Per-instance notifications for edits that do not have a host parameter (wave/routes).
//! The host pointer is invalidated before nih-plug destroys the instance. GUI teardown
//! can be deferred, so a late edit must safely become a no-op.
use clap_sys::host::clap_host;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

pub struct HostNotifications {
    host: AtomicPtr<clap_host>,
    pending: AtomicBool,
}
impl HostNotifications {
    pub(crate) fn new(host: *const clap_host) -> Self {
        Self { host: AtomicPtr::new(host.cast_mut()), pending: AtomicBool::new(false) }
    }
    /// GUI thread only. Defer the notification until locks held by the edit are released.
    pub(crate) fn changed(&self) {
        if self.pending.swap(true, Ordering::AcqRel) { return; }
        let host = self.host.load(Ordering::Acquire);
        if !host.is_null() {
            // SAFETY: the host outlives the plugin; destroy invalidates this address.
            unsafe { if let Some(callback) = (*host).request_callback { callback(host); } }
        }
    }
    pub(crate) fn take(&self) -> bool { self.pending.swap(false, Ordering::AcqRel) }
    pub(crate) fn detach(&self) { self.host.store(std::ptr::null_mut(), Ordering::Release); }
}

/// Used once, synchronously during CLAP construction, never queued from the audio thread.
pub enum PluginTask { AttachHost(std::sync::Arc<HostNotifications>) }

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{c_char, c_void};
    use std::sync::atomic::AtomicU32;
    static CALLBACKS: AtomicU32 = AtomicU32::new(0);
    unsafe extern "C" fn callback(_: *const clap_host) { CALLBACKS.fetch_add(1, Ordering::SeqCst); }
    unsafe extern "C" fn extension(_: *const clap_host, _: *const c_char) -> *const c_void { std::ptr::null() }
    #[test]
    fn changes_coalesce_and_late_editor_edits_do_not_call_a_destroyed_host() {
        let host=clap_host {
            clap_version: clap_sys::version::CLAP_VERSION, host_data: std::ptr::null_mut(),
            name:c"test".as_ptr(),vendor:c"test".as_ptr(),url:c"".as_ptr(),version:c"1".as_ptr(),
            get_extension:Some(extension),request_restart:None,request_process:None,request_callback:Some(callback),
        };
        CALLBACKS.store(0,Ordering::SeqCst);
        let notifications=HostNotifications::new(&host);
        notifications.changed();notifications.changed();
        assert_eq!(CALLBACKS.load(Ordering::SeqCst),1);
        assert!(notifications.take());assert!(!notifications.take());
        notifications.changed();assert_eq!(CALLBACKS.load(Ordering::SeqCst),2);
        notifications.detach();notifications.take();notifications.changed();
        assert_eq!(CALLBACKS.load(Ordering::SeqCst),2);
    }
}
