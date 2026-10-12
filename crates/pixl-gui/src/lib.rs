//! PIXL's native Bridge-driven editor and deterministic standalone test harness.
mod editor;
mod mock;
mod paint;
pub use editor::{EditorApp, BrowserState, Control, Action};
pub use mock::{MockBridge, EditEvent};
use pixl_core::Bridge;
use std::{ffi::c_void, sync::Arc};
/// Logical design size in points.
pub const DESIGN_SIZE: (u32, u32) = (900, 560);
/// Owns the native child view; drop on the host's main thread to detach it.
pub struct EditorHandle {
    #[cfg(target_os = "macos")]
    _view: pixl_gfx::ViewHandle,
}
/// Attach the editor to a host view.
///
/// # Safety
/// `parent_nsview` must be a live NSView on the main thread and outlive this handle.
pub unsafe fn open_editor(parent_nsview: *mut c_void, bridge: Arc<dyn Bridge>) -> Option<EditorHandle> {
    #[cfg(target_os = "macos")]
    { unsafe { pixl_gfx::open_child_view(parent_nsview, Box::new(EditorApp::new(bridge))) }.ok().map(|view| EditorHandle { _view: view }) }
    #[cfg(not(target_os = "macos"))]
    { let _ = (parent_nsview, bridge); None }
}
/// Initialize the shared GPU in the background.
pub fn prewarm() { pixl_gfx::prewarm(); }
