//! The [`App`] trait implemented by UI code, and the [`EventCtx`] it receives with events.

use crate::canvas::Canvas;
use crate::event::{Cursor, Event, Menu};
use crate::text::{TextLayout, TextMetrics, TextStyle, TextSystem};
use std::sync::Arc;
use vello::kurbo::{Point, Rect, Size};
use vello::peniko::Color;

/// The design's background colour `#F7F6F3`: shown before the GPU is ready and around the
/// content when the view's aspect ratio differs from [`App::size`].
pub const DEFAULT_BACKGROUND: Color = Color::from_rgb8(0xF7, 0xF6, 0xF3);

/// A user interface hosted by a pixl-gfx view (or rendered offscreen).
///
/// Everything is called on the main thread, never concurrently. The platform calls
/// [`App::event`] for input, [`App::needs_redraw`] on every 60 Hz tick, and [`App::frame`]
/// when a new frame is needed (after input that requested a redraw, while `needs_redraw`
/// returns `true`, after resizes and scale changes).
pub trait App {
    /// Logical size of the content in points (e.g. the 900 × 560 design size). The view
    /// scales this uniformly to fit its bounds (UI zoom) and centres it.
    fn size(&self) -> Size;

    /// Handles one input event. Use `ctx` to request a redraw, change the cursor, capture
    /// the mouse, focus text input, show menus, or mark keys as handled.
    fn event(&mut self, event: &Event, ctx: &mut EventCtx<'_>);

    /// Draws one frame. `dt` is the time since the previous frame in seconds (clamped to
    /// `0..=0.1`; `0` for the first frame and offscreen renders).
    fn frame(&mut self, canvas: &mut Canvas<'_>, dt: f64);

    /// Polled at 60 Hz: return `true` to get a frame (animations, live meters, state changed
    /// by the host). Keep it cheap. Defaults to `false` (redraw only on request).
    fn needs_redraw(&mut self) -> bool {
        false
    }

    /// The cursor to show while the pointer is over the view. Queried after every mouse
    /// event (unless the event handler called [`EventCtx::set_cursor`]).
    fn cursor(&self) -> Cursor {
        Cursor::Arrow
    }

    /// Clear colour behind the content (also the letterbox colour).
    fn background(&self) -> Color {
        DEFAULT_BACKGROUND
    }
}

/// Requests an event handler made through [`EventCtx`]. The platform applies them after
/// [`App::event`] returns; tests can inspect them directly (see
/// [`Headless::event`](crate::Headless::event)).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Effects {
    /// A frame was requested.
    pub redraw: bool,
    /// Request a subtle alignment pulse on a supported trackpad.
    pub haptic: bool,
    /// Cursor override for this event.
    pub cursor: Option<Cursor>,
    /// `Some(true)` = begin infinite drag, `Some(false)` = end it.
    pub infinite_drag: Option<bool>,
    /// `Some(true)` = focus text input, `Some(false)` = release it.
    pub text_focus: Option<bool>,
    /// Caret rectangle for IME candidate windows (logical points).
    pub text_input_area: Option<Rect>,
    /// Context menu to show at a logical position.
    pub context_menu: Option<(Menu, Point)>,
    /// The key event was consumed (not forwarded to the host).
    pub handled: bool,
}

/// Context passed to [`App::event`].
pub struct EventCtx<'a> {
    text: &'a mut TextSystem,
    scale_factor: f64,
    text_focused: bool,
    infinite_drag: bool,
    effects: Effects,
}

impl<'a> EventCtx<'a> {
    /// Creates a context. `text_focused`/`infinite_drag` describe the current platform state.
    pub fn new(text: &'a mut TextSystem, scale_factor: f64, text_focused: bool, infinite_drag: bool) -> Self {
        Self { text, scale_factor, text_focused, infinite_drag, effects: Effects::default() }
    }

    /// Requests a new frame.
    pub fn request_redraw(&mut self) {
        self.effects.redraw = true;
    }

    /// Request tactile feedback for a meaningful control detent.
    pub fn haptic_alignment(&mut self) { self.effects.haptic = true; }

    /// Sets the cursor now (overrides [`App::cursor`] for this event).
    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.effects.cursor = Some(cursor);
    }

    /// Starts an *infinite drag* for the current mouse gesture: the cursor is hidden and
    /// frozen, further [`Event::MouseDrag`]s report relative motion without screen-edge
    /// limits, and on release (or [`EventCtx::end_infinite_drag`]) the cursor reappears
    /// where the drag started. Only valid while a mouse button is held (call it from
    /// `MouseDown` or `MouseDrag`); ignored otherwise.
    pub fn begin_infinite_drag(&mut self) {
        self.effects.infinite_drag = Some(true);
        self.infinite_drag = true;
    }

    /// Ends an infinite drag early (it always ends on mouse release).
    pub fn end_infinite_drag(&mut self) {
        self.effects.infinite_drag = Some(false);
        self.infinite_drag = false;
    }

    /// Whether an infinite drag is active (including one requested during this event).
    pub fn is_infinite_drag(&self) -> bool {
        self.infinite_drag
    }

    /// Makes the view the keyboard first responder: keys now arrive as
    /// [`Event::KeyDown`] → [`Event::TextInput`] / [`Event::TextCommand`].
    pub fn focus_text_input(&mut self) {
        self.effects.text_focus = Some(true);
        self.text_focused = true;
    }

    /// Gives keyboard focus back to the host (keys go up the responder chain again).
    pub fn blur_text_input(&mut self) {
        self.effects.text_focus = Some(false);
        self.text_focused = false;
    }

    /// Whether a text field currently has keyboard focus.
    pub fn has_text_focus(&self) -> bool {
        self.text_focused
    }

    /// Tells input methods where the caret is (logical points) so candidate windows appear
    /// next to it.
    pub fn set_text_input_area(&mut self, caret: Rect) {
        self.effects.text_input_area = Some(caret);
    }

    /// Shows a native context menu with its top-left corner at `at` (logical points) after
    /// this event handler returns. The result arrives as [`Event::MenuItemSelected`] or
    /// [`Event::MenuDismissed`]. Frames keep rendering while the menu is open.
    pub fn show_context_menu(&mut self, menu: Menu, at: Point) {
        self.effects.context_menu = Some((menu, at));
    }

    /// Marks a key event as consumed so it is not forwarded to the host.
    pub fn set_handled(&mut self) {
        self.effects.handled = true;
    }

    /// Device pixels per logical point (backing scale × UI zoom).
    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    /// The text system (for measurement and hit-testing in event handlers).
    pub fn text(&mut self) -> &mut TextSystem {
        self.text
    }

    /// Measures `text`.
    pub fn measure(&mut self, text: &str, style: &TextStyle) -> TextMetrics {
        self.text.measure(text, style)
    }

    /// Shapes `text` (cached; same cache as drawing), e.g. to hit-test a click in a field.
    pub fn layout(&mut self, text: &str, style: &TextStyle) -> Arc<TextLayout> {
        self.text.layout(text, style)
    }

    /// The requests made so far.
    pub fn effects(&self) -> &Effects {
        &self.effects
    }

    /// Consumes the context, returning the requests.
    pub fn into_effects(self) -> Effects {
        self.effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Menu;

    #[test]
    fn ctx_records_effects() {
        let mut text = TextSystem::new();
        let mut ctx = EventCtx::new(&mut text, 2.0, false, false);
        assert_eq!(ctx.scale_factor(), 2.0);
        ctx.request_redraw();
        ctx.set_cursor(Cursor::Hand);
        ctx.begin_infinite_drag();
        assert!(ctx.is_infinite_drag());
        ctx.focus_text_input();
        assert!(ctx.has_text_focus());
        ctx.show_context_menu(Menu::new().item(1, "x"), Point::new(3.0, 4.0));
        ctx.set_handled();
        let w = ctx.measure("abc", &TextStyle::sans(10.0)).width;
        assert!(w > 0.0);
        let e = ctx.into_effects();
        assert!(e.redraw && e.handled);
        assert_eq!(e.cursor, Some(Cursor::Hand));
        assert_eq!(e.infinite_drag, Some(true));
        assert_eq!(e.text_focus, Some(true));
        assert_eq!(e.context_menu.map(|(m, p)| (m.items.len(), p)), Some((1, Point::new(3.0, 4.0))));
    }
}
