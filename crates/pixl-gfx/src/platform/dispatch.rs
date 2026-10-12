//! Main-thread helpers built on libdispatch's main queue.

use std::ffi::c_void;

/// Opaque `dispatch_queue_s`.
#[repr(C)]
struct DispatchQueue {
    _private: [u8; 0],
}

#[link(name = "System", kind = "dylib")]
unsafe extern "C" {
    /// The main queue; `dispatch_get_main_queue()` is a macro for `&_dispatch_main_q`.
    static _dispatch_main_q: DispatchQueue;
    fn dispatch_async_f(queue: *const DispatchQueue, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn pthread_main_np() -> i32;
}

/// Whether the calling thread is the process's main (AppKit) thread.
pub(crate) fn is_main_thread() -> bool {
    // SAFETY: plain libSystem query without preconditions.
    unsafe { pthread_main_np() != 0 }
}

/// Runs `work` on the main thread **later**, from the main queue — never synchronously, so
/// it is safe to call from inside AppKit callbacks and from any thread. A panic in `work`
/// is caught and logged (it must not unwind into libdispatch).
pub(crate) fn run_on_main_later<F: FnOnce() + Send + 'static>(work: F) {
    extern "C" fn trampoline<F: FnOnce()>(context: *mut c_void) {
        // SAFETY: `context` is the box leaked below; libdispatch calls us exactly once.
        let work = unsafe { Box::from_raw(context.cast::<F>()) };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || work())).is_err() {
            log::error!("pixl-gfx: panic in a main-queue task");
        }
    }
    let context = Box::into_raw(Box::new(work)).cast::<c_void>();
    // SAFETY: the main queue lives for the whole process; the trampoline matches the
    // context type and consumes it exactly once.
    unsafe { dispatch_async_f(std::ptr::addr_of!(_dispatch_main_q), context, trampoline::<F>) };
}

/// A raw pointer that may be sent to the main thread. Only dereference it there.
pub(crate) struct MainThreadPtr<T>(pub(crate) *mut T);

// SAFETY: the pointer is only an address while it travels; it is dereferenced exclusively on
// the main thread (enforced by the users of this type).
unsafe impl<T> Send for MainThreadPtr<T> {}
