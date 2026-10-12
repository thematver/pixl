//! # pixl-gfx — PIXL's native rendering platform
//!
//! A small, reusable foundation for a GPU-rendered plugin editor on macOS:
//!
//! * an `NSView` child view for the host's parent view (layer-backed `CAMetalLayer`,
//!   Retina-aware, flipped, first-click-through, hover tracking, trackpad scrolling,
//!   keyboard that stays out of the host's way, IME text input, native context menus,
//!   infinite drags with a hidden cursor) — [`open_child_view`];
//! * one shared GPU context per process (wgpu/Metal device + vello renderer) created on a
//!   background thread — [`prewarm`];
//! * an immediate-mode [`Canvas`] (shapes, paths, dashes, clips, opacity layers, transforms,
//!   text, icons) in **logical points** (the design's CSS pixels);
//! * text via parley with the embedded Geist / Geist Mono fonts, layout caching, wrapping,
//!   ellipsis, measurement and caret geometry — [`TextStyle`], [`TextSystem`];
//! * vector [`Icon`]s that stand in for symbols Geist lacks (no tofu, ever);
//! * offscreen rendering for golden screenshots — [`render_to_png`], [`Headless`].
//!
//! There is nothing PIXL-specific here: the editor (`pixl-gui`) is an [`App`].
//!
//! ## Writing an app
//!
//! ```
//! use pixl_gfx::kurbo::{Rect, Size, Stroke};
//! use pixl_gfx::{rgb, App, Canvas, Cursor, Event, EventCtx, MouseButton, TextAlign, TextStyle};
//!
//! const BG: pixl_gfx::Color = rgb(0xF7F6F3);
//! const INK: pixl_gfx::Color = rgb(0x161616);
//! const ACCENT: pixl_gfx::Color = rgb(0xFF5A1F);
//!
//! #[derive(Default)]
//! struct Knob {
//!     value: f64,
//!     hover: bool,
//! }
//!
//! impl App for Knob {
//!     fn size(&self) -> Size {
//!         Size::new(240.0, 120.0) // logical points; the view zooms this to fit
//!     }
//!
//!     fn event(&mut self, event: &Event, ctx: &mut EventCtx) {
//!         let hit = Rect::new(20.0, 40.0, 220.0, 80.0);
//!         match event {
//!             Event::MouseMove(e) => {
//!                 let hover = hit.contains(e.pos);
//!                 if hover != self.hover {
//!                     self.hover = hover;
//!                     ctx.request_redraw();
//!                 }
//!             }
//!             Event::MouseDown(e) if e.button == Some(MouseButton::Left) && hit.contains(e.pos) => {
//!                 ctx.begin_infinite_drag(); // hide the cursor, unlimited relative motion
//!             }
//!             Event::MouseDrag(e) if ctx.is_infinite_drag() => {
//!                 let speed = if e.mods.fine() { 0.0005 } else { 0.005 };
//!                 self.value = (self.value - e.delta.y * speed).clamp(0.0, 1.0);
//!                 ctx.request_redraw();
//!             }
//!             _ => {}
//!         }
//!     }
//!
//!     fn frame(&mut self, c: &mut Canvas, _dt: f64) {
//!         c.fill_rect(c.bounds(), BG);
//!         let track = Rect::new(20.0, 56.0, 220.0, 64.0);
//!         c.fill_rounded_rect(track, 4.0, rgb(0xE2DFD8));
//!         let fill = Rect::new(track.x0, track.y0, track.x0 + track.width() * self.value, track.y1);
//!         c.fill_rounded_rect(fill, 4.0, if self.hover { ACCENT } else { INK });
//!         c.stroke(&track.inflate(4.0, 4.0), &Stroke::new(1.0).with_dashes(0.0, [3.0, 2.0]), rgb(0xC9C5BC));
//!         let label = TextStyle::mono(10.0).weight(500.0).letter_spacing(0.06).align(TextAlign::Center);
//!         c.text_in(Rect::new(0.0, 10.0, 240.0, 30.0), &format!("DEPTH {:.0}%", self.value * 100.0), &label);
//!     }
//!
//!     fn cursor(&self) -> Cursor {
//!         if self.hover { Cursor::ResizeVertical } else { Cursor::Arrow }
//!     }
//! }
//! # let _ = Knob::default();
//! ```
//!
//! ## Hosting it
//!
//! ```ignore
//! // When the plugin instance is created (any thread):
//! pixl_gfx::prewarm();
//!
//! // In Editor::spawn (main thread), with the host's NSView*:
//! let handle = unsafe { pixl_gfx::open_child_view(parent_nsview, Box::new(Knob::default())) }?;
//! // ... dropping `handle` removes the view and frees the app.
//! ```
//!
//! The view fills the parent (and follows its resizes); the app's [`App::size`] is scaled
//! uniformly to fit and centred, so UI zoom (75–200 %) is simply "resize the host view".
//! Until the GPU context is ready the view shows [`App::background`] (`#F7F6F3` by default).
//!
//! ## Frames and redraws
//!
//! A 60 Hz timer on the main run loop (common modes, so it keeps running during menu
//! tracking and live resize) renders a frame when something asked for one:
//! [`EventCtx::request_redraw`], [`App::needs_redraw`] returning `true`, a resize or a scale
//! change. Otherwise nothing is rendered and the idle cost is a few microseconds per tick.
//! Presentation never waits for vsync.
//!
//! ## Text
//!
//! ```
//! use pixl_gfx::{TextStyle, TextAlign, TextOverflow};
//! // 10 px Geist Mono 500, letter-spacing .06em — the design's chip labels.
//! let chip = TextStyle::mono(10.0).weight(500.0).letter_spacing(0.06);
//! // Preset description wrapping at 300 px.
//! let desc = TextStyle::sans(12.0).wrap(300.0).line_height(1.45);
//! // Name truncated with an ellipsis.
//! let name = TextStyle::sans(13.0).weight(500.0).ellipsis(160.0);
//! assert_eq!(name.overflow, TextOverflow::Ellipsis);
//! # let _ = (chip, desc, TextAlign::Left);
//! ```
//!
//! [`Canvas::text`] anchors at the top of the first line box (CSS semantics),
//! [`Canvas::text_baseline`] at the baseline and [`Canvas::text_in`] centres vertically in
//! a rect like `align-items: center`. Layouts are cached by text + style (colour excluded),
//! so drawing the same label every frame costs no shaping. [`TextLayout`] exposes caret
//! rectangles, hit testing, selection rectangles and word boundaries for text fields.
//!
//! Characters missing from Geist (`✎ ╱ ∿ ⇅ ★ 🔒 ⏎ ✓ ⌘` …) are drawn as vector [`Icon`]s with
//! the text's colour, size and weight; see [`fonts::has_glyph`] and [`Icon::from_char`].
//!
//! ## Keyboard
//!
//! Without a focused text field the view never becomes first responder, so Space, ⌘S and
//! friends reach the host untouched. [`EventCtx::focus_text_input`] makes it first responder:
//! keys then arrive as [`Event::KeyDown`] first (call [`EventCtx::set_handled`] to consume,
//! e.g. ↑/↓/Esc in a list), and unhandled keys are interpreted by the system into
//! [`Event::TextInput`] (any layout, dead keys, IME) and [`Event::TextCommand`]
//! (`DeleteBackward`, `MoveLeft { select }`, …). ⌘-shortcuts not handled by the app go to the
//! host's menus. [`clipboard`] gives access to the pasteboard for copy/paste.
//!
//! ## Testing
//!
//! [`Headless`] renders any [`App`] offscreen ([`render_to_png`] for golden images) and
//! delivers synthetic events, returning the [`Effects`] the handler requested.
//!
//! ## Threading
//!
//! Everything except [`prewarm`] and offscreen rendering runs on the main thread. A
//! [`ViewHandle`] is `Send` (plugin frameworks require it) but must be dropped on the main
//! thread; dropping it elsewhere defers the teardown to the main thread.

#![warn(missing_docs)]

mod app;
mod canvas;
mod event;
pub mod fonts;
mod gpu;
mod headless;
pub mod icons;
mod render;
mod text;

#[cfg(target_os = "macos")]
mod platform;

pub use app::{App, Effects, EventCtx, DEFAULT_BACKGROUND};
pub use canvas::Canvas;
pub use event::{
    Cursor, Event, Key, KeyEvent, Menu, MenuItem, Modifiers, MouseButton, MouseButtons, MouseEvent, ScrollEvent,
    ScrollPhase, TextCommand,
};
pub use gpu::{gpu_error_count, prewarm, GpuContext};
#[cfg(feature = "png")]
pub use headless::render_to_png;
pub use headless::{render_to_image, FrameTiming, Headless, Image};
pub use icons::Icon;
pub use text::{FontFamily, TextAlign, TextLayout, TextMetrics, TextOverflow, TextStyle, TextSystem, INK};

#[cfg(target_os = "macos")]
pub use platform::{clipboard, open_child_view, run_standalone, ViewHandle, ViewStats};

/// Re-export of the geometry crate used throughout the API (`Point`, `Rect`, `BezPath`,
/// `Stroke`, `Affine`, …).
pub use vello::kurbo;
/// Re-export of the paint crate (`Color`, `Gradient`, `Brush`, …).
pub use vello::peniko;
/// Re-export of wgpu (for advanced use of [`GpuContext`]).
pub use vello::wgpu;

/// An sRGB colour with straight alpha (re-export of [`peniko::Color`]).
pub type Color = vello::peniko::Color;

/// Opaque colour from `0xRRGGBB`.
///
/// ```
/// let accent = pixl_gfx::rgb(0xFF5A1F);
/// assert_eq!(accent.to_rgba8().to_u8_array(), [0xFF, 0x5A, 0x1F, 0xFF]);
/// ```
pub const fn rgb(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// Colour from `0xRRGGBB` with `alpha` (0..1).
pub const fn rgba(hex: u32, alpha: f32) -> Color {
    rgb(hex).with_alpha(alpha)
}

/// Errors reported by pixl-gfx.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum GfxError {
    /// No usable GPU, device creation failed, or it timed out.
    Gpu(String),
    /// Rendering or readback failed.
    Render(String),
    /// File output failed.
    Io(String),
    /// Platform (AppKit) integration failed, e.g. a null parent view or a call off the main
    /// thread.
    Platform(String),
}

impl std::fmt::Display for GfxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GfxError::Gpu(m) => write!(f, "GPU error: {m}"),
            GfxError::Render(m) => write!(f, "render error: {m}"),
            GfxError::Io(m) => write!(f, "I/O error: {m}"),
            GfxError::Platform(m) => write!(f, "platform error: {m}"),
        }
    }
}

impl std::error::Error for GfxError {}
