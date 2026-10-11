//! PIXL editor (native, Metal-rendered).
//!
//! STUB: public API frozen by docs/SPEC.md §7. The implementation agent replaces the
//! internals; signatures below must stay source-compatible.

use pixl_core::Bridge;
use std::ffi::c_void;
use std::sync::Arc;

/// Logical design size in points.
pub const DESIGN_SIZE: (u32, u32) = (900, 560);

/// An open editor attached to a host-provided parent view. Dropping it detaches and frees
/// the editor (must happen on the main thread).
pub struct EditorHandle {
    _private: (),
}

/// Open the editor as a child of `parent_nsview` (an `NSView*` from the host).
///
/// # Safety
/// `parent_nsview` must be a valid `NSView*` that outlives the returned handle, and this
/// must be called on the main thread.
pub unsafe fn open_editor(parent_nsview: *mut c_void, bridge: Arc<dyn Bridge>) -> Option<EditorHandle> {
    let _ = (parent_nsview, bridge);
    None
}

/// Start initializing the shared GPU context in the background so the first editor opens
/// instantly. Safe to call many times from any thread.
pub fn prewarm() {}
