#![allow(non_snake_case)] // NSTextInputClient follows AppKit protocol method names.
use crate::kurbo::{Point, Rect, Size, Vec2};
use crate::render::{FrameBuilder, MAX_TARGET_EDGE, Placement, RenderTarget, render_scene};
use crate::wgpu;
use crate::{
    App, Cursor, Effects, Event, EventCtx, GfxError, GpuContext, Key, KeyEvent, Menu, MenuItem, Modifiers, MouseButton,
    MouseButtons, MouseEvent, ScrollEvent, ScrollPhase, TextCommand, TextSystem,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSCursor, NSEvent, NSEventMask, NSEventModifierFlags, NSMenu, NSMenuItem, NSTextInputClient,
    NSTrackingArea, NSView, NSWindow, NSWindowDidResignKeyNotification,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSNotification, NSNotificationCenter, NSObjectProtocol, NSPoint, NSRange, NSRect,
    NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer,
};
use objc2_quartz_core::CAMetalLayer;
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

/// Thread-safe counters for a native view.
#[derive(Clone, Default, Debug)]
pub struct ViewStats {
    frames: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
    ticks: Arc<AtomicU64>,
}
impl ViewStats {
    /// Successfully presented frames.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
    /// Rendering/surface failures (timeouts and occlusion are excluded).
    pub fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }
    /// Main-run-loop timer invocations.
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }
}

/// Owns the attached child view. Dropping on a worker schedules main-thread teardown.
pub struct ViewHandle {
    view: usize,
    stats: ViewStats,
}
// Only an opaque address crosses threads; it is dereferenced exclusively on the main queue.
unsafe impl Send for ViewHandle {}
impl ViewHandle {
    /// A snapshot source that remains valid after the view is closed.
    pub fn stats(&self) -> ViewStats {
        self.stats.clone()
    }
}
#[link(name = "System")]
unsafe extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(queue: *mut c_void, context: *mut c_void, work: unsafe extern "C" fn(*mut c_void));
}
unsafe extern "C" fn dispose(ptr: *mut c_void) {
    let view = unsafe { Retained::<PixlView>::from_raw(ptr.cast()).unwrap() };
    view.detach();
}
impl Drop for ViewHandle {
    fn drop(&mut self) {
        unsafe {
            if MainThreadMarker::new().is_some() {
                dispose(self.view as *mut c_void);
            } else {
                dispatch_async_f(std::ptr::addr_of!(_dispatch_main_q).cast_mut(), self.view as *mut c_void, dispose);
            }
        }
    }
}

struct SurfaceState {
    gpu: Arc<GpuContext>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    target: RenderTarget,
}
struct State {
    app: Box<dyn App>,
    text: TextSystem,
    builder: FrameBuilder,
    placement: Placement,
    surface: Option<SurfaceState>,
    dirty: bool,
    last: Option<Instant>,
    buttons: MouseButtons,
    position: Point,
    control_click: bool,
    caret: Rect,
}
struct Ivars {
    resign_observer: RefCell<Option<Retained<AnyObject>>>,
    monitor: RefCell<Option<Retained<AnyObject>>>,
    hovered: Cell<bool>,
    state: RefCell<Option<State>>,
    timer: RefCell<Option<Retained<NSTimer>>>,
    tracking: RefCell<Option<Retained<NSTrackingArea>>>,
    layer: Retained<CAMetalLayer>,
    focused: Cell<bool>,
    infinite: Cell<bool>,
    hidden: Cell<bool>,
    anchor: Cell<NSPoint>,
    marked: RefCell<String>,
    selection: Cell<NSRange>,
    selected_menu: Cell<Option<u32>>,
    stats: ViewStats,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct PixlView;
    unsafe impl NSObjectProtocol for PixlView {}
    impl PixlView {
        #[unsafe(method(isFlipped))] fn flipped(&self) -> bool { true }
        #[unsafe(method(acceptsFirstMouse:))] fn first_mouse(&self, _event: Option<&NSEvent>) -> bool { true }
        #[unsafe(method(acceptsFirstResponder))] fn accepts_focus(&self) -> bool { self.ivars().focused.get() }
        #[unsafe(method(resignFirstResponder))] fn resign(&self) -> bool {
            if self.ivars().focused.replace(false) { self.emit(Event::TextFocusLost); }
            self.end_drag(); self.clear_marked(); true
        }
        #[unsafe(method(viewDidChangeBackingProperties))] fn backing(&self) { self.invalidate(); }
        #[unsafe(method(viewDidMoveToWindow))] fn moved_to_window(&self) {
            if self.window().is_none() { self.end_drag(); }
            self.invalidate();
        }
        #[unsafe(method(setFrameSize:))] fn frame_size(&self, size: NSSize) {
            unsafe { let _: () = msg_send![super(self), setFrameSize: size]; }
            self.invalidate();
        }
        #[unsafe(method(pixlTick:))] fn tick(&self, _timer: &NSTimer) { self.render(); }
        #[unsafe(method(mouseDown:))] fn down(&self, e: &NSEvent) { self.mouse(e, 0, Some(MouseButton::Left)); }
        #[unsafe(method(rightMouseDown:))] fn right_down(&self, e: &NSEvent) { self.mouse(e, 0, Some(MouseButton::Right)); }
        #[unsafe(method(otherMouseDown:))] fn other_down(&self, e: &NSEvent) { self.mouse(e, 0, Some(other_button(e))); }
        #[unsafe(method(mouseUp:))] fn up(&self, e: &NSEvent) { self.mouse(e, 1, Some(MouseButton::Left)); }
        #[unsafe(method(rightMouseUp:))] fn right_up(&self, e: &NSEvent) { self.mouse(e, 1, Some(MouseButton::Right)); }
        #[unsafe(method(otherMouseUp:))] fn other_up(&self, e: &NSEvent) { self.mouse(e, 1, Some(other_button(e))); }
        #[unsafe(method(mouseMoved:))] fn moved(&self, e: &NSEvent) { self.mouse(e, 2, None); }
        #[unsafe(method(mouseDragged:))] fn dragged(&self, e: &NSEvent) { self.mouse(e, 3, Some(MouseButton::Left)); }
        #[unsafe(method(rightMouseDragged:))] fn right_dragged(&self, e: &NSEvent) { self.mouse(e, 3, Some(MouseButton::Right)); }
        #[unsafe(method(otherMouseDragged:))] fn other_dragged(&self, e: &NSEvent) { self.mouse(e, 3, Some(other_button(e))); }
        #[unsafe(method(mouseEntered:))] fn entered(&self, _e: &NSEvent) { self.ivars().hovered.set(true); self.emit(Event::MouseEnter); }
        #[unsafe(method(mouseExited:))] fn exited(&self, _e: &NSEvent) { self.ivars().hovered.set(false); self.emit(Event::MouseLeave); if !self.ivars().infinite.get() { self.hide_cursor(false); } }
        #[unsafe(method(flagsChanged:))] fn flags(&self, e: &NSEvent) { self.emit(Event::ModifiersChanged(modifiers(e))); }
        #[unsafe(method(scrollWheel:))] fn scroll(&self, e: &NSEvent) {
            let event = {
                let Ok(state) = self.ivars().state.try_borrow() else { return }; let Some(s) = state.as_ref() else { return };
                let pos = self.logical_position(e, &s.placement);
                let precise = e.hasPreciseScrollingDeltas();
                let raw = Vec2::new(e.scrollingDeltaX(), e.scrollingDeltaY());
                let phase = e.phase().0;
                Event::Scroll(ScrollEvent { pos, delta: raw * (if precise { 1.0 } else { ScrollEvent::LINE_POINTS }) / s.placement.zoom,
                    lines: if precise { Vec2::ZERO } else { raw }, precise,
                    phase: if phase & 1 != 0 { ScrollPhase::Began } else if phase & (8 | 16) != 0 { ScrollPhase::Ended } else if phase != 0 { ScrollPhase::Changed } else { ScrollPhase::None },
                    momentum: e.momentumPhase().0 != 0, mods: modifiers(e) })
            }; self.emit(event);
        }
        #[unsafe(method(keyDown:))] fn key_down(&self, e: &NSEvent) {
            if !self.ivars().focused.get() { unsafe { let _: () = msg_send![super(self), keyDown: e]; } return; }
            if self.emit(Event::KeyDown(key_event(e))) { return; }
            if modifiers(e).cmd { unsafe { let _: () = msg_send![super(self), keyDown: e]; } }
            else { unsafe { let _: () = msg_send![self, interpretKeyEvents: &*NSArray::from_slice(&[e])]; } }
        }
        #[unsafe(method(keyUp:))] fn key_up(&self, e: &NSEvent) {
            if !self.ivars().focused.get() || !self.emit(Event::KeyUp(key_event(e))) {
                unsafe { let _: () = msg_send![super(self), keyUp: e]; }
            }
        }
        #[unsafe(method(pixlMenuAction:))] fn menu_action(&self, item: &NSMenuItem) {
            let tag: isize = unsafe { msg_send![item, tag] }; self.ivars().selected_menu.set(Some(tag as u32));
        }
    }
    unsafe impl NSTextInputClient for PixlView {
        #[unsafe(method(insertText:replacementRange:))]
        unsafe fn insertText_replacementRange(&self, text: &AnyObject, _range: NSRange) { self.clear_marked(); self.emit(Event::TextInput(native_string(text))); }
        #[unsafe(method(doCommandBySelector:))]
        unsafe fn doCommandBySelector(&self, selector: Sel) { self.emit(Event::TextCommand(TextCommand::from_selector(&selector.name().to_string_lossy()))); }
        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        unsafe fn setMarkedText_selectedRange_replacementRange(&self, text: &AnyObject, selection: NSRange, _replacement: NSRange) {
            let text = native_string(text); *self.ivars().marked.borrow_mut() = text.clone(); self.ivars().selection.set(selection); self.emit(Event::TextPreedit(text));
        }
        #[unsafe(method(unmarkText))] fn unmarkText(&self) { self.clear_marked(); }
        #[unsafe(method(selectedRange))] fn selectedRange(&self) -> NSRange { self.ivars().selection.get() }
        #[unsafe(method(markedRange))] fn markedRange(&self) -> NSRange {
            let text = self.ivars().marked.borrow(); if text.is_empty() { NSRange::new(isize::MAX as usize, 0) } else { NSRange::new(0, text.encode_utf16().count()) }
        }
        #[unsafe(method(hasMarkedText))] fn hasMarkedText(&self) -> bool { !self.ivars().marked.borrow().is_empty() }
        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        unsafe fn attributedSubstringForProposedRange_actualRange(&self, _range: NSRange, actual: *mut NSRange) -> Option<Retained<NSAttributedString>> {
            if !actual.is_null() { unsafe { *actual = NSRange::new(isize::MAX as usize, 0); } } None
        }
        #[unsafe(method_id(validAttributesForMarkedText))] fn validAttributesForMarkedText(&self) -> Retained<NSArray<NSString>> { NSArray::new() }
        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        unsafe fn firstRectForCharacterRange_actualRange(&self, range: NSRange, actual: *mut NSRange) -> NSRect {
            if !actual.is_null() { unsafe { *actual = range; } }
            let Ok(state) = self.ivars().state.try_borrow() else { return NSRect::ZERO };
            let Some(s) = state.as_ref() else { return NSRect::ZERO };
            let p = s.placement; let r = s.caret;
            let rect = NSRect::new(NSPoint::new(r.x0 * p.zoom + p.offset.x, r.y0 * p.zoom + p.offset.y), NSSize::new(r.width()*p.zoom, r.height()*p.zoom));
            unsafe {
                let rect: NSRect = msg_send![self, convertRect: rect, toView: Option::<&NSView>::None];
                if let Some(window) = self.window() { msg_send![&window, convertRectToScreen: rect] } else { rect }
            }
        }
        #[unsafe(method(characterIndexForPoint:))] fn characterIndexForPoint(&self, _p: NSPoint) -> usize { isize::MAX as usize }
    }
);

fn native_string(value: &AnyObject) -> String {
    unsafe {
        if msg_send![value, isKindOfClass: NSString::class()] {
            let s: &NSString = &*(value as *const AnyObject).cast();
            s.to_string()
        } else {
            let s: Retained<NSString> = msg_send![value, string];
            s.to_string()
        }
    }
}
fn modifiers(e: &NSEvent) -> Modifiers {
    let f = e.modifierFlags().0;
    Modifiers {
        caps_lock: f & (1 << 16) != 0,
        shift: f & (1 << 17) != 0,
        ctrl: f & (1 << 18) != 0,
        alt: f & (1 << 19) != 0,
        cmd: f & (1 << 20) != 0,
    }
}
fn other_button(e: &NSEvent) -> MouseButton {
    match e.buttonNumber() {
        2 => MouseButton::Middle,
        n => MouseButton::Other(n.clamp(3, 255) as u8),
    }
}
fn key(code: u16, text: &str) -> Key {
    match code {
        36 | 76 => Key::Enter,
        48 => Key::Tab,
        49 => Key::Space,
        51 => Key::Backspace,
        53 => Key::Escape,
        117 => Key::Delete,
        123 => Key::ArrowLeft,
        124 => Key::ArrowRight,
        125 => Key::ArrowDown,
        126 => Key::ArrowUp,
        115 => Key::Home,
        119 => Key::End,
        116 => Key::PageUp,
        121 => Key::PageDown,
        122 => Key::F(1),
        120 => Key::F(2),
        99 => Key::F(3),
        118 => Key::F(4),
        96 => Key::F(5),
        97 => Key::F(6),
        98 => Key::F(7),
        100 => Key::F(8),
        101 => Key::F(9),
        109 => Key::F(10),
        103 => Key::F(11),
        111 => Key::F(12),
        105 => Key::F(13),
        107 => Key::F(14),
        113 => Key::F(15),
        106 => Key::F(16),
        64 => Key::F(17),
        79 => Key::F(18),
        80 => Key::F(19),
        90 => Key::F(20),
        _ => text.chars().next().and_then(|c| c.to_lowercase().next()).map(Key::Character).unwrap_or(Key::Unknown),
    }
}
fn key_event(e: &NSEvent) -> KeyEvent {
    let text = e.characters().map(|s| s.to_string()).unwrap_or_default();
    // Preserve the keyboard layout's command equivalents (e.g. Latin Cmd-C on
    // Cyrillic layouts), without Option or Control changing the logical shortcut key.
    let plain = if modifiers(e).cmd {
        e.charactersByApplyingModifiers(NSEventModifierFlags::Command)
    } else {
        e.charactersIgnoringModifiers()
    }
    .map(|s| s.to_string())
    .unwrap_or_default();
    KeyEvent { key: key(e.keyCode(), &plain), code: e.keyCode(), mods: modifiers(e), repeat: e.isARepeat(), text }
}

impl PixlView {
    fn invalidate(&self) {
        // Host resize callbacks can re-enter while App::event is running. The timer also
        // compares placement every tick, so a busy state cannot lose a size change.
        if let Ok(mut state) = self.ivars().state.try_borrow_mut() {
            if let Some(s) = state.as_mut() {
                s.dirty = true;
            }
        }
    }
    fn clear_marked(&self) {
        let had = !self.ivars().marked.borrow().is_empty();
        self.ivars().marked.borrow_mut().clear();
        self.ivars().selection.set(NSRange::new(0, 0));
        if had {
            self.emit(Event::TextPreedit(String::new()));
        }
    }
    fn logical_position(&self, e: &NSEvent, p: &Placement) -> Point {
        let point: NSPoint =
            unsafe { msg_send![self, convertPoint: e.locationInWindow(), fromView: Option::<&NSView>::None] };
        p.view_to_logical(Point::new(point.x, point.y))
    }
    fn mouse(&self, e: &NSEvent, kind: u8, mut button: Option<MouseButton>) {
        let event = {
            let Ok(mut state) = self.ivars().state.try_borrow_mut() else { return };
            let Some(s) = state.as_mut() else { return };
            if button == Some(MouseButton::Left) {
                if kind == 0 {
                    s.control_click = modifiers(e).ctrl;
                }
                if s.control_click {
                    button = Some(MouseButton::Right);
                }
            }
            let delta = Vec2::new(e.deltaX(), e.deltaY()) / s.placement.zoom;
            let pos =
                if self.ivars().infinite.get() { s.position + delta } else { self.logical_position(e, &s.placement) };
            s.position = pos;
            if let Some(b) = button {
                if kind == 0 {
                    s.buttons.insert(b);
                } else if kind == 1 {
                    s.buttons.remove(b);
                }
            }
            let ev = MouseEvent {
                pos,
                delta: if kind == 0 { Vec2::ZERO } else { delta },
                button,
                buttons: s.buttons,
                mods: modifiers(e),
                click_count: if kind == 0 { e.clickCount().clamp(0, 255) as u8 } else { 0 },
            };
            match kind {
                0 => Event::MouseDown(ev),
                1 => Event::MouseUp(ev),
                2 => Event::MouseMove(ev),
                _ => Event::MouseDrag(ev),
            }
        };
        self.emit(event);
        if kind == 1 {
            self.end_drag();
        }
    }
    fn emit(&self, event: Event) -> bool {
        let (effects, cursor) = {
            let Ok(mut state) = self.ivars().state.try_borrow_mut() else { return false };
            let Some(s) = state.as_mut() else { return false };
            let mut ctx = EventCtx::new(
                &mut s.text,
                s.placement.scale_factor(),
                self.ivars().focused.get(),
                self.ivars().infinite.get(),
            );
            s.app.event(&event, &mut ctx);
            let effects = ctx.into_effects();
            s.dirty |= effects.redraw;
            (effects, s.app.cursor())
        };
        let handled = effects.handled;
        self.apply(effects, cursor);
        handled
    }
    fn apply(&self, effects: Effects, cursor: Cursor) {
        if let Some(rect) = effects.text_input_area {
            if let Some(s) = self.ivars().state.borrow_mut().as_mut() {
                s.caret = rect;
            }
        }
        if let Some(focus) = effects.text_focus {
            self.ivars().focused.set(focus);
            if let Some(window) = self.window() {
                unsafe {
                    if focus {
                        let ok: bool = msg_send![&window, makeFirstResponder: self];
                        if !ok {
                            self.ivars().focused.set(false);
                        }
                    } else {
                        let responder: Option<Retained<AnyObject>> = msg_send![&window, firstResponder];
                        if responder
                            .as_ref()
                            .is_some_and(|r| std::ptr::eq(&**r, self as *const Self as *const AnyObject))
                        {
                            let _: bool = msg_send![&window, makeFirstResponder: self.superview().as_deref()];
                        }
                    }
                }
            }
            if !focus {
                self.clear_marked();
            }
        }
        if let Some(infinite) = effects.infinite_drag {
            if infinite {
                let held = self.ivars().state.borrow().as_ref().is_some_and(|s| !s.buttons.is_empty());
                if held && !self.ivars().infinite.get() {
                    {
                        let event = objc2_core_graphics::CGEvent::new(None);
                        if let Some(event) = event {
                            let pos = objc2_core_graphics::CGEvent::location(Some(&event));
                            self.ivars().anchor.set(NSPoint::new(pos.x, pos.y));
                        } else {
                            return;
                        }
                        if objc2_core_graphics::CGAssociateMouseAndMouseCursorPosition(false).0 == 0 {
                            self.ivars().infinite.set(true);
                        }
                    }
                }
            } else {
                self.end_drag();
            }
        }
        self.cursor(effects.cursor.unwrap_or(cursor));
        if let Some((menu, at)) = effects.context_menu {
            self.menu(menu, at);
        }
    }
    fn hide_cursor(&self, hide: bool) {
        if hide != self.ivars().hidden.replace(hide) {
            if hide {
                NSCursor::hide();
            } else {
                NSCursor::unhide();
            }
        }
    }
    #[allow(deprecated)] // These cursors are available on every supported macOS version.
    fn cursor(&self, cursor: Cursor) {
        self.hide_cursor(self.ivars().infinite.get() || cursor == Cursor::Hidden);
        let cursor = match cursor {
            Cursor::Arrow | Cursor::Hidden => NSCursor::arrowCursor(),
            Cursor::Hand => NSCursor::pointingHandCursor(),
            Cursor::IBeam => NSCursor::IBeamCursor(),
            Cursor::Crosshair => NSCursor::crosshairCursor(),
            Cursor::ResizeHorizontal => NSCursor::resizeLeftRightCursor(),
            Cursor::ResizeVertical => NSCursor::resizeUpDownCursor(),
            Cursor::Grab => NSCursor::openHandCursor(),
            Cursor::Grabbing => NSCursor::closedHandCursor(),
            Cursor::NotAllowed => NSCursor::operationNotAllowedCursor(),
        };
        cursor.set();
    }
    fn end_drag(&self) {
        if self.ivars().infinite.replace(false) {
            let anchor = self.ivars().anchor.get();
            // CGEvent supplies global display coordinates (top-left origin), including
            // displays with negative coordinates; no primary-screen height assumption.
            objc2_core_graphics::CGWarpMouseCursorPosition(objc2_core_foundation::CGPoint::new(anchor.x, anchor.y));
            objc2_core_graphics::CGAssociateMouseAndMouseCursorPosition(true);
        }
        self.hide_cursor(false);
    }
    fn menu(&self, description: Menu, at: Point) {
        let menu = self.build_menu(description);
        let point = {
            let state = self.ivars().state.borrow();
            let Some(s) = state.as_ref() else { return };
            NSPoint::new(at.x * s.placement.zoom + s.placement.offset.x, at.y * s.placement.zoom + s.placement.offset.y)
        };
        self.end_drag();
        self.ivars().selected_menu.set(None);
        unsafe {
            let _: bool = msg_send![&menu, popUpMenuPositioningItem: Option::<&NSMenuItem>::None, atLocation: point, inView: self];
        }
        self.emit(self.ivars().selected_menu.take().map(Event::MenuItemSelected).unwrap_or(Event::MenuDismissed));
    }
    fn build_menu(&self, description: Menu) -> Retained<NSMenu> {
        let mtm = self.mtm();
        let menu = NSMenu::new(mtm);
        unsafe {
            let _: () = msg_send![&menu, setAutoenablesItems: false];
        }
        for entry in description.items {
            let item = match entry {
                MenuItem::Separator => NSMenuItem::separatorItem(mtm),
                MenuItem::Action { id, label, checked, enabled } => unsafe {
                    let item: Retained<NSMenuItem> = msg_send![NSMenuItem::alloc(mtm), initWithTitle: &*NSString::from_str(&label), action: Some(sel!(pixlMenuAction:)), keyEquivalent: &*NSString::from_str("")];
                    let _: () = msg_send![&item, setTarget: self];
                    let _: () = msg_send![&item, setTag: id as isize];
                    let _: () = msg_send![&item, setState: if checked { 1isize } else { 0isize }];
                    let _: () = msg_send![&item, setEnabled: enabled];
                    item
                },
                MenuItem::Submenu { label, menu, enabled } => unsafe {
                    let item: Retained<NSMenuItem> = msg_send![NSMenuItem::alloc(mtm), initWithTitle: &*NSString::from_str(&label), action: Option::<Sel>::None, keyEquivalent: &*NSString::from_str("")];
                    let _: () = msg_send![&item, setSubmenu: &*self.build_menu(menu)];
                    let _: () = msg_send![&item, setEnabled: enabled];
                    item
                },
            };
            unsafe {
                let _: () = msg_send![&menu, addItem: &*item];
            }
        }
        menu
    }
    fn render(&self) {
        self.ivars().stats.ticks.fetch_add(1, Ordering::Relaxed);
        // A modal menu may run a nested timer; skip ticks while an app callback is active.
        let Ok(mut state) = self.ivars().state.try_borrow_mut() else { return };
        let Some(s) = state.as_mut() else { return };
        let bounds = self.bounds();
        let backing = self.window().map(|w| w.backingScaleFactor()).unwrap_or(1.0);
        let placement = Placement::fit(s.app.size(), Size::new(bounds.size.width, bounds.size.height), backing);
        if placement != s.placement {
            s.placement = placement;
            s.dirty = true;
        }
        s.dirty |= s.app.needs_redraw() || s.surface.as_ref().is_some_and(|surface| surface.gpu.is_lost());
        if !s.dirty || bounds.size.width <= 0.0 || bounds.size.height <= 0.0 || self.window().is_none() {
            return;
        }
        let Some(gpu) = GpuContext::try_get() else { return };
        let width = (bounds.size.width * backing).round().clamp(1.0, MAX_TARGET_EDGE as f64) as u32;
        let height = (bounds.size.height * backing).round().clamp(1.0, MAX_TARGET_EDGE as f64) as u32;
        unsafe {
            let _: () = msg_send![&self.ivars().layer, setContentsScale: backing];
            let _: () = msg_send![&self.ivars().layer, setDrawableSize: NSSize::new(width as f64, height as f64)];
        }
        if s.surface.as_ref().is_some_and(|s| s.gpu.generation() != gpu.generation()) {
            s.surface = None;
        }
        if s.surface.is_none() {
            // The child and its installed CAMetalLayer outlive this surface: detach
            // drops State (and the surface) before removing the view or releasing its layer.
            let window = wgpu::rwh::AppKitWindowHandle::new(std::ptr::NonNull::from(self).cast());
            let surface = unsafe {
                gpu.instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(wgpu::rwh::AppKitDisplayHandle::new().into()),
                    raw_window_handle: window.into(),
                })
            };
            match surface {
                Ok(surface) => {
                    let Some(mut config) = surface.get_default_config(&gpu.adapter, width, height) else {
                        self.record_error("Metal surface is incompatible with adapter");
                        return;
                    };
                    // Vello's output contains encoded sRGB channel values. An sRGB
                    // attachment would encode them again during the blit.
                    let Some(format) = surface_format(&surface.get_capabilities(&gpu.adapter).formats) else {
                        self.record_error("Metal surface has no non-sRGB RGBA/BGRA format");
                        return;
                    };
                    config.format = format;
                    config.present_mode = wgpu::PresentMode::AutoNoVsync;
                    config.desired_maximum_frame_latency = 2;
                    surface.configure(&gpu.device, &config);
                    let target = RenderTarget::new(&gpu, width, height, wgpu::TextureUsages::empty());
                    s.surface = Some(SurfaceState { gpu: gpu.clone(), surface, config, target });
                }
                Err(e) => {
                    self.record_error(&e.to_string());
                    return;
                }
            }
        }
        let surface = s.surface.as_mut().unwrap();
        if !surface.target.matches(&gpu, width, height) {
            surface.config.width = width;
            surface.config.height = height;
            surface.surface.configure(&gpu.device, &surface.config);
            surface.target = RenderTarget::new(&gpu, width, height, wgpu::TextureUsages::empty());
        }
        let (frame, suboptimal) = match surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => (t, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => (t, true),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Outdated => {
                surface.surface.configure(&gpu.device, &surface.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                s.surface = None;
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.record_error("surface validation failed");
                s.surface = None;
                return;
            }
        };
        let now = Instant::now();
        let dt = s.last.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        let background = s.builder.build(&mut *s.app, &mut s.text, &s.placement, dt);
        if let Err(e) = render_scene(&gpu, s.builder.scene(), &surface.target, background) {
            self.record_error(&e.to_string());
            return;
        }
        let destination = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pixl-gfx present") });
        gpu.blitter(surface.config.format).copy(&gpu.device, &mut encoder, &surface.target.view, &destination);
        gpu.queue.submit([encoder.finish()]);
        gpu.queue.present(frame);
        if suboptimal {
            surface.surface.configure(&gpu.device, &surface.config);
        }
        s.last = Some(now);
        s.dirty = false;
        self.ivars().stats.frames.fetch_add(1, Ordering::Relaxed);
    }
    fn record_error(&self, error: &str) {
        let count = self.ivars().stats.errors.fetch_add(1, Ordering::Relaxed);
        if count < 5 || count.is_power_of_two() {
            log::error!("pixl-gfx view: {error}");
        }
    }
    fn detach(&self) {
        // Hosts may destroy their editor synchronously from a callback into the app.
        // Keep an owning retain until that callback unwinds, then dispose on the queue.
        if self.ivars().state.try_borrow_mut().is_err() {
            let retained = unsafe { Retained::retain(self as *const Self as *mut Self).unwrap() };
            unsafe {
                dispatch_async_f(
                    std::ptr::addr_of!(_dispatch_main_q).cast_mut(),
                    Retained::into_raw(retained).cast(),
                    dispose,
                );
            }
            return;
        }
        if let Some(observer) = self.ivars().resign_observer.borrow_mut().take() {
            unsafe {
                NSNotificationCenter::defaultCenter().removeObserver(&observer);
            }
        }
        if let Some(monitor) = self.ivars().monitor.borrow_mut().take() {
            unsafe {
                NSEvent::removeMonitor(&monitor);
            }
        }
        if let Some(timer) = self.ivars().timer.borrow_mut().take() {
            timer.invalidate();
        }
        self.end_drag();
        if let Some(window) = self.window() {
            if self.ivars().focused.replace(false) {
                unsafe {
                    let _: bool = msg_send![&window, makeFirstResponder: self.superview().as_deref()];
                }
            }
        }
        // Release the GPU surface before AppKit releases its layer.
        self.ivars().state.borrow_mut().take();
        if let Some(tracking) = self.ivars().tracking.borrow_mut().take() {
            self.removeTrackingArea(&tracking);
        }
        self.removeFromSuperview();
    }
}

/// Attaches an editor to an AppKit parent view, without waiting for GPU initialization.
///
/// # Safety
/// `parent` must be a live `NSView` pointer. Call on the AppKit main thread. The host must
/// retain its parent until this function returns; the child follows parent resizes.
pub unsafe fn open_child_view(parent: *mut c_void, app: Box<dyn App>) -> Result<ViewHandle, GfxError> {
    let mtm =
        MainThreadMarker::new().ok_or_else(|| GfxError::Platform("opening a view requires the main thread".into()))?;
    if parent.is_null() {
        return Err(GfxError::Platform("null parent NSView".into()));
    }
    let parent = unsafe { &*parent.cast::<NSView>() };
    let bounds = parent.bounds();
    let stats = ViewStats::default();
    let layer = CAMetalLayer::new();
    let color = app.background().to_rgba8().to_u8_array();
    let cg_color = objc2_core_graphics::CGColor::new_generic_rgb(
        color[0] as f64 / 255.0,
        color[1] as f64 / 255.0,
        color[2] as f64 / 255.0,
        color[3] as f64 / 255.0,
    );
    unsafe {
        let _: () = msg_send![&layer, setBackgroundColor: &*cg_color];
        let _: () = msg_send![&layer, setOpaque: color[3] == 255];
        let _: () = msg_send![&layer, setDisplaySyncEnabled: false];
    }
    let placement = Placement::fit(app.size(), Size::new(bounds.size.width, bounds.size.height), 1.0);
    let ivars = Ivars {
        resign_observer: RefCell::new(None),
        monitor: RefCell::new(None),
        hovered: Cell::new(false),
        state: RefCell::new(Some(State {
            app,
            text: TextSystem::new(),
            builder: FrameBuilder::default(),
            placement,
            surface: None,
            dirty: true,
            last: None,
            buttons: MouseButtons::NONE,
            position: Point::ZERO,
            control_click: false,
            caret: Rect::ZERO,
        })),
        timer: RefCell::new(None),
        tracking: RefCell::new(None),
        layer,
        focused: Cell::new(false),
        infinite: Cell::new(false),
        hidden: Cell::new(false),
        anchor: Cell::new(NSPoint::ZERO),
        marked: RefCell::new(String::new()),
        selection: Cell::new(NSRange::new(0, 0)),
        selected_menu: Cell::new(None),
        stats: stats.clone(),
    };
    let view = PixlView::alloc(mtm).set_ivars(ivars);
    let view: Retained<PixlView> = unsafe { msg_send![super(view), initWithFrame: bounds] };
    unsafe {
        let _: () = msg_send![&view, setWantsLayer: true];
        let _: () = msg_send![&view, setLayer: &*view.ivars().layer];
        let _: () = msg_send![&view, setAutoresizingMask: 18usize];
        let tracking: Retained<NSTrackingArea> = msg_send![NSTrackingArea::alloc(), initWithRect: NSRect::ZERO, options: (1usize | 2 | 0x80 | 0x200 | 0x400), owner: &*view, userInfo: Option::<&AnyObject>::None];
        view.addTrackingArea(&tracking);
        *view.ivars().tracking.borrow_mut() = Some(tracking);
        parent.addSubview(&view);
        let timer: Retained<NSTimer> = msg_send![NSTimer::class(), timerWithTimeInterval: 1.0f64/60.0, target: &*view, selector: sel!(pixlTick:), userInfo: Option::<&AnyObject>::None, repeats: true];
        NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        *view.ivars().timer.borrow_mut() = Some(timer);
    }
    // A local monitor observes modifiers while hovering without taking keyboard focus
    // or consuming host input. Its pointer remains valid until detach removes the monitor.
    let address = Retained::as_ptr(&view) as usize;
    let monitor = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        let view = unsafe { &*(address as *const PixlView) };
        if view.ivars().hovered.get() && !view.ivars().focused.get() {
            view.emit(Event::ModifiersChanged(modifiers(unsafe { event.as_ref() })));
        }
        event.as_ptr()
    });
    *view.ivars().monitor.borrow_mut() =
        unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::FlagsChanged, &monitor) };
    let resign = block2::RcBlock::new(move |notification: std::ptr::NonNull<NSNotification>| {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let view = unsafe { &*(address as *const PixlView) };
        let object = unsafe { notification.as_ref().object() };
        if let (Some(object), Some(window)) = (object, view.window()) {
            if Retained::as_ptr(&object).cast::<c_void>() == Retained::as_ptr(&window).cast::<c_void>() {
                view.end_drag();
                if let Ok(mut state) = view.ivars().state.try_borrow_mut() {
                    if let Some(s) = state.as_mut() {
                        s.buttons = MouseButtons::NONE;
                    }
                }
                if view.ivars().focused.replace(false) {
                    view.clear_marked();
                    view.emit(Event::TextFocusLost);
                }
            }
        }
    });
    let observer: Retained<AnyObject> = unsafe {
        msg_send![&*NSNotificationCenter::defaultCenter(), addObserverForName: NSWindowDidResignKeyNotification, object: Option::<&AnyObject>::None, queue: Option::<&AnyObject>::None, usingBlock: &*resign]
    };
    *view.ivars().resign_observer.borrow_mut() = Some(observer);
    crate::prewarm();
    Ok(ViewHandle { view: Retained::into_raw(view) as usize, stats })
}

/// Runs an AppKit development window until the window closes. Call on the main thread,
/// outside a plugin host; a plugin must use [`open_child_view`] instead.
/// Set `PIXL_HARNESS_SECONDS` to a positive number for an automated bounded smoke run.
/// A bounded run requires at least one presented frame and zero rendering errors.
pub fn run_standalone(app: Box<dyn App>) -> Result<(), GfxError> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| GfxError::Platform("standalone harness requires the main thread".into()))?;
    let timeout = harness_timeout(std::env::var("PIXL_HARNESS_SECONDS").ok().as_deref())?;
    let started = Instant::now();
    let size = app.size();
    unsafe {
        let application = NSApplication::sharedApplication(mtm);
        let _: bool = msg_send![&application, setActivationPolicy: 0isize];
        let window: Retained<NSWindow> = msg_send![NSWindow::alloc(mtm), initWithContentRect: NSRect::new(NSPoint::ZERO, NSSize::new(size.width.max(1.0), size.height.max(1.0))), styleMask: 15usize, backing: 2usize, defer: false];
        let _: () = msg_send![&window, setReleasedWhenClosed: false];
        let _: () = msg_send![&window, setTitle: &*NSString::from_str("PIXL")];
        let parent: Retained<NSView> = msg_send![&window, contentView];
        let handle = open_child_view(Retained::as_ptr(&parent) as *mut c_void, app)?;
        let _: () = msg_send![&window, center];
        let _: () = msg_send![&window, makeKeyAndOrderFront: Option::<&AnyObject>::None];
        let _: () = msg_send![&application, finishLaunching];
        let _: () = msg_send![&application, activateIgnoringOtherApps: true];
        // A bounded event pump lets the harness return naturally on close, without terminating the process.
        while window.isVisible() && timeout.is_none_or(|seconds| started.elapsed().as_secs_f64() < seconds) {
            objc2::rc::autoreleasepool(|_| {
                let date = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
                let event: Option<Retained<NSEvent>> = msg_send![&application, nextEventMatchingMask: usize::MAX, untilDate: &*date, inMode: objc2_foundation::NSDefaultRunLoopMode, dequeue: true];
                if let Some(event) = event {
                    let _: () = msg_send![&application, sendEvent: &*event];
                }
                let _: () = msg_send![&application, updateWindows];
            });
        }
        let stats = handle.stats();
        if timeout.is_some() {
            let _: () = msg_send![&window, close];
        }
        drop(handle);
        if timeout.is_some() {
            eprintln!("pixl-gfx smoke: frames={}, errors={}, ticks={}", stats.frames(), stats.errors(), stats.ticks());
            if stats.frames() == 0 || stats.errors() != 0 {
                return Err(GfxError::Render(format!(
                    "native smoke failed: {} presented frames, {} errors",
                    stats.frames(),
                    stats.errors()
                )));
            }
        }
    }
    Ok(())
}

fn surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    formats
        .iter()
        .copied()
        .find(|format| matches!(format, wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm))
}

fn harness_timeout(value: Option<&str>) -> Result<Option<f64>, GfxError> {
    value
        .map(|value| {
            value
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or_else(|| GfxError::Platform("PIXL_HARNESS_SECONDS must be a finite positive number".into()))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn surface_selection_avoids_double_srgb_encoding() {
        use wgpu::TextureFormat::*;
        assert_eq!(surface_format(&[Bgra8UnormSrgb, Bgra8Unorm]), Some(Bgra8Unorm));
        assert_eq!(surface_format(&[Rgba8UnormSrgb, Rgba8Unorm]), Some(Rgba8Unorm));
        assert_eq!(surface_format(&[Bgra8UnormSrgb]), None);
    }
    #[test]
    fn smoke_timeout_is_opt_in_and_validated() {
        assert_eq!(harness_timeout(None).unwrap(), None);
        assert_eq!(harness_timeout(Some("3.5")).unwrap(), Some(3.5));
        for value in ["", "NaN", "inf", "0", "-1", "garbage"] {
            assert!(harness_timeout(Some(value)).is_err());
        }
    }
    #[test]
    fn maps_navigation_and_function_keys() {
        assert_eq!(key(123, ""), Key::ArrowLeft);
        assert_eq!(key(76, ""), Key::Enter);
        assert_eq!(key(90, ""), Key::F(20));
        assert_eq!(key(0, "Ф"), Key::Character('ф'));
    }
    #[test]
    fn rejects_appkit_operations_on_worker_threads() {
        struct Empty;
        impl App for Empty {
            fn size(&self) -> Size {
                Size::new(10.0, 10.0)
            }
            fn event(&mut self, _: &Event, _: &mut EventCtx) {}
            fn frame(&mut self, _: &mut crate::Canvas, _: f64) {}
        }
        std::thread::spawn(|| {
            assert!(MainThreadMarker::new().is_none());
            assert!(matches!(
                unsafe { open_child_view(std::ptr::null_mut(), Box::new(Empty)) },
                Err(GfxError::Platform(_))
            ));
            assert!(matches!(run_standalone(Box::new(Empty)), Err(GfxError::Platform(_))));
            assert!(super::super::clipboard::read().is_none());
            assert!(matches!(super::super::clipboard::write("test"), Err(GfxError::Platform(_))));
        })
        .join()
        .unwrap();
    }
    #[test]
    fn stats_can_outlive_the_view_and_cross_threads() {
        let stats = ViewStats::default();
        let other = stats.clone();
        std::thread::spawn(move || {
            other.frames.fetch_add(1, Ordering::Relaxed);
        })
        .join()
        .unwrap();
        assert_eq!(stats.frames(), 1);
        assert_eq!(stats.errors(), 0);
        fn assert_send<T: Send>() {}
        assert_send::<ViewHandle>();
    }
}
