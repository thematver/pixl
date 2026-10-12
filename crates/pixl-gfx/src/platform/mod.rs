//! AppKit child-view hosting. All native state is confined to the main thread.
mod view;
pub use view::{ViewHandle, ViewStats, open_child_view, run_standalone};

/// The macOS general pasteboard. Calls must be made on the main thread.
pub mod clipboard {
    use crate::GfxError;
    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, msg_send};
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;

    /// Reads plain UTF-8 text from the general pasteboard.
    pub fn read() -> Option<String> {
        MainThreadMarker::new()?;
        unsafe {
            let board = NSPasteboard::generalPasteboard();
            let text: Option<Retained<NSString>> =
                msg_send![&board, stringForType: &*NSString::from_str("public.utf8-plain-text")];
            text.map(|s| s.to_string())
        }
    }
    /// Replaces the pasteboard contents with plain text.
    pub fn write(text: &str) -> Result<(), GfxError> {
        MainThreadMarker::new()
            .ok_or_else(|| GfxError::Platform("pasteboard write requires the main thread".into()))?;
        unsafe {
            let board = NSPasteboard::generalPasteboard();
            let _: isize = msg_send![&board, clearContents];
            let ok: bool = msg_send![&board, setString: &*NSString::from_str(text), forType: &*NSString::from_str("public.utf8-plain-text")];
            if ok { Ok(()) } else { Err(GfxError::Platform("pasteboard rejected text".into())) }
        }
    }
    /// Alias for [`read`].
    pub fn get_text() -> Option<String> {
        read()
    }
    /// Alias for [`write`].
    pub fn set_text(text: &str) -> Result<(), GfxError> {
        write(text)
    }
}
