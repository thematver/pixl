//! Editor integration: the nih-plug [`Editor`](nih_plug::prelude::Editor) that hosts the
//! native PIXL GUI (`pixl-gui`, behind the `gui` cargo feature), plus the pieces it needs that
//! are independent of the GUI crate.
//!
//! - The editor is an `NSView` child of the host's view, created by `pixl_gui::open_editor`
//!   with a [`PluginBridge`](crate::bridge::PluginBridge).
//! - Its logical size is the 900 × 560 design size times the UI scale stored in the plugin
//!   state ([`editor_size`]). The bridge changes the scale through
//!   `GuiContext::request_resize`.
//! - The returned handle is wrapped in [`MainThreadBox`], which guarantees that AppKit
//!   objects are torn down on the main thread even if a host drops the handle elsewhere.
//!
//! Without the `gui` feature, `Plugin::editor` returns `None` and hosts show their generic
//! parameter UI.

use pixl_core::bridge::{EDITOR_HEIGHT, EDITOR_WIDTH};

/// Logical editor size (points) for a UI scale (clamped to the supported range).
pub fn editor_size(scale: f32) -> (u32, u32) {
    let scale = crate::state::sanitize_scale(scale);
    ((EDITOR_WIDTH * scale).round() as u32, (EDITOR_HEIGHT * scale).round() as u32)
}

#[cfg(feature = "gui")]
pub use native::PixlEditor;

#[cfg(feature = "gui")]
mod native {
    use super::{editor_size, MainThreadBox};
    use crate::bridge::PluginBridge;
    use crate::params::PixlParams;
    use crate::presets::PresetStore;
    use nih_plug::prelude::{Editor, GuiContext, ParentWindowHandle};
    use pixl_core::bridge::{EDITOR_HEIGHT, EDITOR_WIDTH};
    use pixl_core::Bridge;
    use std::any::Any;
    use std::sync::Arc;

    // The GUI's design size and the core's editor size must agree.
    const _: () =
        assert!(pixl_gui::DESIGN_SIZE.0 == EDITOR_WIDTH as u32 && pixl_gui::DESIGN_SIZE.1 == EDITOR_HEIGHT as u32);

    /// The PIXL editor factory handed to nih-plug.
    pub struct PixlEditor {
        params: Arc<PixlParams>,
        store: Arc<PresetStore>,
    }

    impl PixlEditor {
        /// An editor for the given instance state.
        pub fn new(params: Arc<PixlParams>, store: Arc<PresetStore>) -> Self {
            Self { params, store }
        }
    }

    impl Editor for PixlEditor {
        fn spawn(&self, parent: ParentWindowHandle, context: Arc<dyn GuiContext>) -> Box<dyn Any + Send> {
            let ParentWindowHandle::AppKitNsView(view) = parent else {
                nih_plug::nih_warn!("PIXL: unsupported parent window type");
                return Box::new(());
            };
            if view.is_null() {
                nih_plug::nih_warn!("PIXL: the host passed no parent view");
                return Box::new(());
            }
            let bridge: Arc<dyn Bridge> = Arc::new(PluginBridge::new(context, self.params.clone(), self.store.clone()));
            // SAFETY: nih-plug calls `spawn` on the main thread with the host's parent NSView,
            // which outlives the returned handle (the wrapper drops it before the view goes).
            let handle = unsafe { pixl_gui::open_editor(view, bridge) };
            if handle.is_none() {
                nih_plug::nih_warn!("PIXL: the editor could not be opened");
            }
            Box::new(MainThreadBox::new(handle))
        }

        fn size(&self) -> (u32, u32) {
            editor_size(self.params.shared.ui_scale())
        }

        fn set_scale_factor(&self, _factor: f32) -> bool {
            // Never called on macOS: AppKit works in logical points and the view follows the
            // window's `backingScaleFactor` itself.
            false
        }

        // The editor reads live values through the bridge every frame.
        fn param_value_changed(&self, _id: &str, _normalized_value: f32) {}

        fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {}

        fn param_values_changed(&self) {}
    }
}

/// Owns a value that must be dropped on the main thread (AppKit objects).
///
/// `Send` so nih-plug can store it; if it is dropped on another thread, the value is moved to
/// the main dispatch queue and dropped there.
pub struct MainThreadBox<T: 'static> {
    value: Option<T>,
}

impl<T: 'static> MainThreadBox<T> {
    /// Wrap a value created on the main thread.
    pub fn new(value: T) -> Self {
        Self { value: Some(value) }
    }
}

// SAFETY: the wrapped value is never accessed through the box; it is only dropped, and the
// drop happens on the main thread (directly, or dispatched to the main queue).
unsafe impl<T: 'static> Send for MainThreadBox<T> {}

impl<T: 'static> Drop for MainThreadBox<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            main_thread::drop_on_main(value);
        }
    }
}

#[cfg(target_os = "macos")]
mod main_thread {
    use std::ffi::c_void;

    #[repr(C)]
    struct DispatchQueue {
        _opaque: [u8; 0],
    }

    extern "C" {
        /// `dispatch_get_main_queue()` is a macro for `&_dispatch_main_q`.
        static _dispatch_main_q: DispatchQueue;
        fn dispatch_async_f(queue: *const DispatchQueue, context: *mut c_void, work: extern "C" fn(*mut c_void));
        fn pthread_main_np() -> i32;
    }

    /// Whether the calling thread is the process's main thread.
    pub fn is_main_thread() -> bool {
        // SAFETY: plain libSystem query without preconditions.
        unsafe { pthread_main_np() != 0 }
    }

    extern "C" fn drop_boxed<T>(context: *mut c_void) {
        // SAFETY: `context` is the `Box<T>` leaked in `drop_on_main`, consumed exactly once.
        unsafe { drop(Box::from_raw(context as *mut T)) }
    }

    /// Drop `value` on the main thread (immediately if already there).
    pub fn drop_on_main<T: 'static>(value: T) {
        if is_main_thread() {
            drop(value);
            return;
        }
        let context = Box::into_raw(Box::new(value)) as *mut c_void;
        // SAFETY: the main queue is a process-lifetime object; `drop_boxed::<T>` matches the
        // boxed type.
        unsafe { dispatch_async_f(&raw const _dispatch_main_q, context, drop_boxed::<T>) };
    }
}

#[cfg(not(target_os = "macos"))]
mod main_thread {
    /// Non-macOS builds have no AppKit objects; drop in place.
    pub fn drop_on_main<T: 'static>(value: T) {
        drop(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_follows_the_ui_scale() {
        assert_eq!(editor_size(1.0), (900, 560));
        assert_eq!(editor_size(0.75), (675, 420));
        assert_eq!(editor_size(1.25), (1125, 700));
        assert_eq!(editor_size(1.5), (1350, 840));
        assert_eq!(editor_size(2.0), (1800, 1120));
        assert_eq!(editor_size(5.0), (1800, 1120));
        assert_eq!(editor_size(0.1), (675, 420));
        assert_eq!(editor_size(f32::NAN), (900, 560));
        for scale in pixl_core::bridge::UI_SCALES {
            let (w, h) = editor_size(scale);
            assert_eq!((w as f32, h as f32), ((900.0 * scale).round(), (560.0 * scale).round()));
        }
    }

    #[test]
    fn main_thread_box_drops_inline_only_on_the_main_thread() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static DROPPED: AtomicBool = AtomicBool::new(false);
        struct Probe;
        impl Drop for Probe {
            fn drop(&mut self) {
                DROPPED.store(true, Ordering::SeqCst);
            }
        }
        fn assert_send<T: Send>(_: &T) {}
        let boxed = MainThreadBox::new(Probe);
        assert_send(&boxed);
        // Test threads are not the main thread: the drop is deferred to the main queue (which
        // the test harness never runs), so it must not happen inline.
        #[cfg(target_os = "macos")]
        {
            let on_main = main_thread::is_main_thread();
            drop(boxed);
            assert_eq!(DROPPED.load(Ordering::SeqCst), on_main);
        }
        #[cfg(not(target_os = "macos"))]
        {
            drop(boxed);
            assert!(DROPPED.load(Ordering::SeqCst));
        }
    }
}
