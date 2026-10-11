//! Input events delivered to [`App::event`](crate::App::event), plus the cursor and menu
//! description types used through [`EventCtx`](crate::EventCtx).
//!
//! All positions and deltas are in **logical points** (design px), already corrected for the
//! Retina backing scale and the UI zoom.

use vello::kurbo::{Point, Vec2};

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Primary button.
    Left,
    /// Secondary button. A **Control-click** with the primary button is reported as `Right`
    /// (the macOS convention for context menus).
    Right,
    /// Middle button / wheel click.
    Middle,
    /// Any further button (index as reported by the OS, ≥ 3).
    Other(u8),
}

impl MouseButton {
    fn bit(self) -> u8 {
        match self {
            MouseButton::Left => 1,
            MouseButton::Right => 2,
            MouseButton::Middle => 4,
            MouseButton::Other(_) => 8,
        }
    }
}

/// Set of pressed mouse buttons.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MouseButtons(u8);

impl MouseButtons {
    /// No button.
    pub const NONE: MouseButtons = MouseButtons(0);

    /// Whether `button` is pressed.
    pub fn contains(self, button: MouseButton) -> bool {
        self.0 & button.bit() != 0
    }

    /// Whether no button is pressed.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Adds `button`.
    pub fn insert(&mut self, button: MouseButton) {
        self.0 |= button.bit();
    }

    /// Removes `button`.
    pub fn remove(&mut self, button: MouseButton) {
        self.0 &= !button.bit();
    }

    /// The set with `button` added.
    #[must_use]
    pub fn with(mut self, button: MouseButton) -> Self {
        self.insert(button);
        self
    }
}

/// Keyboard modifier state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// ⇧ Shift.
    pub shift: bool,
    /// ⌃ Control.
    pub ctrl: bool,
    /// ⌥ Option / Alt.
    pub alt: bool,
    /// ⌘ Command.
    pub cmd: bool,
    /// Caps Lock is on.
    pub caps_lock: bool,
}

impl Modifiers {
    /// No modifier.
    pub const NONE: Modifiers = Modifiers { shift: false, ctrl: false, alt: false, cmd: false, caps_lock: false };

    /// Whether no modifier (ignoring Caps Lock) is held.
    pub fn is_empty(self) -> bool {
        !(self.shift || self.ctrl || self.alt || self.cmd)
    }

    /// PIXL's "fine adjust" chord: Shift or Command (SPEC §7.3).
    pub fn fine(self) -> bool {
        self.shift || self.cmd
    }
}

/// A mouse button / motion event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseEvent {
    /// Position in logical points. During an *infinite drag* (see
    /// [`EventCtx::begin_infinite_drag`](crate::EventCtx::begin_infinite_drag)) this is a
    /// virtual position: the press position plus all accumulated motion, so it can leave the
    /// view bounds.
    pub pos: Point,
    /// Motion since the previous mouse event of this gesture (logical points; zero for
    /// presses).
    pub delta: Vec2,
    /// The button that changed (presses/releases) or the button that drives a drag. `None`
    /// for hover moves.
    pub button: Option<MouseButton>,
    /// Buttons held after this event.
    pub buttons: MouseButtons,
    /// Modifiers held.
    pub mods: Modifiers,
    /// Click count of a press (1 = single click, 2 = double click, …); 0 for moves.
    pub click_count: u8,
}

impl MouseEvent {
    /// A bare event at `pos` (for synthetic events in tests).
    pub fn at(pos: impl Into<Point>) -> Self {
        Self {
            pos: pos.into(),
            delta: Vec2::ZERO,
            button: None,
            buttons: MouseButtons::NONE,
            mods: Modifiers::NONE,
            click_count: 0,
        }
    }

    /// Sets the button (and adds it to `buttons`).
    #[must_use]
    pub fn button(mut self, button: MouseButton) -> Self {
        self.button = Some(button);
        self.buttons.insert(button);
        self
    }

    /// Sets the click count.
    #[must_use]
    pub fn clicks(mut self, count: u8) -> Self {
        self.click_count = count;
        self
    }

    /// Sets the modifiers.
    #[must_use]
    pub fn mods(mut self, mods: Modifiers) -> Self {
        self.mods = mods;
        self
    }

    /// Sets the motion delta.
    #[must_use]
    pub fn delta(mut self, delta: impl Into<Vec2>) -> Self {
        self.delta = delta.into();
        self
    }
}

/// Phase of a trackpad scroll gesture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScrollPhase {
    /// Discrete wheel or no phase information.
    #[default]
    None,
    /// Fingers touched down.
    Began,
    /// Fingers moving.
    Changed,
    /// Fingers lifted (momentum may follow).
    Ended,
}

/// A scroll-wheel / trackpad event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollEvent {
    /// Pointer position (logical points).
    pub pos: Point,
    /// Scroll distance in logical points, AppKit sign convention (already including the
    /// user's "natural scrolling" preference): positive `y` = the content should move
    /// **down** (scrolling towards the top, or a wheel turned away from the user), positive
    /// `x` = content moves right. A list's scroll offset therefore *decreases* by `delta.y`.
    /// Discrete wheel steps are converted at [`ScrollEvent::LINE_POINTS`] points per line.
    pub delta: Vec2,
    /// `true` for pixel-precise devices (trackpads, Magic Mouse); `false` for notched wheels.
    pub precise: bool,
    /// For notched wheels: the raw line counts (same sign as `delta`); zero for trackpads.
    pub lines: Vec2,
    /// Gesture phase (trackpads).
    pub phase: ScrollPhase,
    /// `true` while the system is delivering inertial momentum after the fingers lifted.
    /// Value controls usually ignore momentum events.
    pub momentum: bool,
    /// Modifiers held.
    pub mods: Modifiers,
}

impl ScrollEvent {
    /// Points per wheel "line" used to convert notched-wheel deltas.
    pub const LINE_POINTS: f64 = 16.0;

    /// A precise (trackpad-like) scroll event at `pos` (for synthetic events in tests).
    pub fn precise(pos: impl Into<Point>, delta: impl Into<Vec2>) -> Self {
        Self {
            pos: pos.into(),
            delta: delta.into(),
            precise: true,
            lines: Vec2::ZERO,
            phase: ScrollPhase::Changed,
            momentum: false,
            mods: Modifiers::NONE,
        }
    }
}

/// A key, as far as UI code cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// Return / keypad Enter.
    Enter,
    /// Tab.
    Tab,
    /// Space bar.
    Space,
    /// Delete-backward (labelled "delete" on Mac keyboards).
    Backspace,
    /// Forward delete (fn+delete).
    Delete,
    /// Escape.
    Escape,
    /// ←
    ArrowLeft,
    /// →
    ArrowRight,
    /// ↑
    ArrowUp,
    /// ↓
    ArrowDown,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Function key F1..F20.
    F(u8),
    /// Any other key, by the character it types. Letters are lower-case; with ⌘ held this is
    /// the layout's command character (Latin on Cyrillic layouts, so ⌘C is `'c'`).
    Character(char),
    /// A key without a character (e.g. a bare modifier).
    Unknown,
}

/// A key press or release.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyEvent {
    /// Logical key.
    pub key: Key,
    /// Hardware key code (macOS virtual key code), layout independent.
    pub code: u16,
    /// Modifiers held.
    pub mods: Modifiers,
    /// Auto-repeat.
    pub repeat: bool,
    /// The characters the key produces with the current modifiers (may be empty). Text
    /// fields should consume [`Event::TextInput`] instead.
    pub text: String,
}

impl KeyEvent {
    /// A key event without modifiers (for synthetic events in tests).
    pub fn new(key: Key) -> Self {
        let text = match key {
            Key::Character(c) => c.to_string(),
            Key::Space => " ".to_string(),
            _ => String::new(),
        };
        Self { key, code: 0, mods: Modifiers::NONE, repeat: false, text }
    }

    /// Sets the modifiers.
    #[must_use]
    pub fn mods(mut self, mods: Modifiers) -> Self {
        self.mods = mods;
        self
    }
}

/// Editing commands produced by the system's key bindings while a text field is focused
/// (the AppKit `doCommandBySelector:` vocabulary).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TextCommand {
    /// Return.
    InsertNewline,
    /// Tab.
    InsertTab,
    /// Shift-Tab.
    InsertBacktab,
    /// Escape.
    Cancel,
    /// Delete backward one character.
    DeleteBackward,
    /// Delete forward one character.
    DeleteForward,
    /// Option-delete.
    DeleteWordBackward,
    /// Option-fn-delete.
    DeleteWordForward,
    /// Command-delete.
    DeleteToLineStart,
    /// Control-K.
    DeleteToLineEnd,
    /// Move one character; `select` extends the selection.
    MoveLeft {
        /// Extend the selection instead of moving the caret.
        select: bool,
    },
    /// Move one character right.
    MoveRight {
        /// Extend the selection.
        select: bool,
    },
    /// Move up (list navigation in single-line fields).
    MoveUp {
        /// Extend the selection.
        select: bool,
    },
    /// Move down.
    MoveDown {
        /// Extend the selection.
        select: bool,
    },
    /// Option-← word move.
    MoveWordLeft {
        /// Extend the selection.
        select: bool,
    },
    /// Option-→ word move.
    MoveWordRight {
        /// Extend the selection.
        select: bool,
    },
    /// Command-← / Home.
    MoveToLineStart {
        /// Extend the selection.
        select: bool,
    },
    /// Command-→ / End.
    MoveToLineEnd {
        /// Extend the selection.
        select: bool,
    },
    /// Command-↑.
    MoveToDocumentStart {
        /// Extend the selection.
        select: bool,
    },
    /// Command-↓.
    MoveToDocumentEnd {
        /// Extend the selection.
        select: bool,
    },
    /// Select all.
    SelectAll,
    /// Any other selector, by name (e.g. `"transpose:"`).
    Other(String),
}

impl TextCommand {
    /// Maps an AppKit selector name (`"deleteBackward:"`) to a command.
    pub fn from_selector(name: &str) -> TextCommand {
        match name {
            "insertNewline:" | "insertLineBreak:" | "insertNewlineIgnoringFieldEditor:" => TextCommand::InsertNewline,
            "insertTab:" | "insertTabIgnoringFieldEditor:" => TextCommand::InsertTab,
            "insertBacktab:" => TextCommand::InsertBacktab,
            "cancelOperation:" | "cancel:" => TextCommand::Cancel,
            "deleteBackward:" | "deleteBackwardByDecomposingPreviousCharacter:" => TextCommand::DeleteBackward,
            "deleteForward:" => TextCommand::DeleteForward,
            "deleteWordBackward:" => TextCommand::DeleteWordBackward,
            "deleteWordForward:" => TextCommand::DeleteWordForward,
            "deleteToBeginningOfLine:" | "deleteToBeginningOfParagraph:" => TextCommand::DeleteToLineStart,
            "deleteToEndOfLine:" | "deleteToEndOfParagraph:" => TextCommand::DeleteToLineEnd,
            "moveLeft:" | "moveBackward:" => TextCommand::MoveLeft { select: false },
            "moveRight:" | "moveForward:" => TextCommand::MoveRight { select: false },
            "moveLeftAndModifySelection:" | "moveBackwardAndModifySelection:" => TextCommand::MoveLeft { select: true },
            "moveRightAndModifySelection:" | "moveForwardAndModifySelection:" => TextCommand::MoveRight { select: true },
            "moveUp:" => TextCommand::MoveUp { select: false },
            "moveDown:" => TextCommand::MoveDown { select: false },
            "moveUpAndModifySelection:" => TextCommand::MoveUp { select: true },
            "moveDownAndModifySelection:" => TextCommand::MoveDown { select: true },
            "moveWordLeft:" | "moveWordBackward:" => TextCommand::MoveWordLeft { select: false },
            "moveWordRight:" | "moveWordForward:" => TextCommand::MoveWordRight { select: false },
            "moveWordLeftAndModifySelection:" | "moveWordBackwardAndModifySelection:" => {
                TextCommand::MoveWordLeft { select: true }
            }
            "moveWordRightAndModifySelection:" | "moveWordForwardAndModifySelection:" => {
                TextCommand::MoveWordRight { select: true }
            }
            "moveToBeginningOfLine:" | "moveToLeftEndOfLine:" | "moveToBeginningOfParagraph:" => {
                TextCommand::MoveToLineStart { select: false }
            }
            "moveToEndOfLine:" | "moveToRightEndOfLine:" | "moveToEndOfParagraph:" => {
                TextCommand::MoveToLineEnd { select: false }
            }
            "moveToBeginningOfLineAndModifySelection:"
            | "moveToLeftEndOfLineAndModifySelection:"
            | "moveParagraphBackwardAndModifySelection:" => TextCommand::MoveToLineStart { select: true },
            "moveToEndOfLineAndModifySelection:"
            | "moveToRightEndOfLineAndModifySelection:"
            | "moveParagraphForwardAndModifySelection:" => TextCommand::MoveToLineEnd { select: true },
            "moveToBeginningOfDocument:" | "scrollToBeginningOfDocument:" => {
                TextCommand::MoveToDocumentStart { select: false }
            }
            "moveToEndOfDocument:" | "scrollToEndOfDocument:" => TextCommand::MoveToDocumentEnd { select: false },
            "moveToBeginningOfDocumentAndModifySelection:" => TextCommand::MoveToDocumentStart { select: true },
            "moveToEndOfDocumentAndModifySelection:" => TextCommand::MoveToDocumentEnd { select: true },
            "selectAll:" => TextCommand::SelectAll,
            other => TextCommand::Other(other.to_string()),
        }
    }
}

/// An input event. See the [module docs](self) for units.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A mouse button went down.
    MouseDown(MouseEvent),
    /// A mouse button went up (always delivered to the view that received the press, even
    /// outside its bounds).
    MouseUp(MouseEvent),
    /// The pointer moved with no button held (hover).
    MouseMove(MouseEvent),
    /// The pointer moved with a button held (also outside the view).
    MouseDrag(MouseEvent),
    /// The pointer entered the view.
    MouseEnter,
    /// The pointer left the view (clear hover state).
    MouseLeave,
    /// Scroll wheel / trackpad.
    Scroll(ScrollEvent),
    /// A key went down. Call [`EventCtx::set_handled`](crate::EventCtx::set_handled) when the
    /// UI used it; otherwise it continues up the host's responder chain (so Space still
    /// starts Logic's transport) or, while a text field is focused, is interpreted into
    /// [`Event::TextInput`] / [`Event::TextCommand`].
    ///
    /// Without a focused text field the view is not the first responder, so most hosts
    /// deliver no key events at all.
    KeyDown(KeyEvent),
    /// A key went up (same forwarding rules as `KeyDown`).
    KeyUp(KeyEvent),
    /// Modifier keys changed (also delivered while hovering, so cursors can react to ⌥).
    ModifiersChanged(Modifiers),
    /// Text typed into the focused text field (after keyboard layout / IME processing;
    /// Cyrillic, dead keys and input methods all arrive here).
    TextInput(String),
    /// IME composition in progress ("marked text"); an empty string ends composition.
    /// Display it at the caret, underlined; the final text arrives as `TextInput`.
    TextPreedit(String),
    /// An editing command for the focused text field.
    TextCommand(TextCommand),
    /// The focused text field lost keyboard focus (another control or window took it).
    TextFocusLost,
    /// The user picked the context-menu item with this id (see
    /// [`EventCtx::show_context_menu`](crate::EventCtx::show_context_menu)).
    MenuItemSelected(u32),
    /// The context menu closed without a selection.
    MenuDismissed,
}

/// Mouse cursor shapes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cursor {
    /// Standard arrow.
    #[default]
    Arrow,
    /// Pointing hand (links, clickable chips).
    Hand,
    /// Text I-beam.
    IBeam,
    /// Crosshair (drawing).
    Crosshair,
    /// Left/right resize (horizontal value drags).
    ResizeHorizontal,
    /// Up/down resize (vertical value drags).
    ResizeVertical,
    /// Open hand (draggable).
    Grab,
    /// Closed hand (dragging).
    Grabbing,
    /// Operation not allowed.
    NotAllowed,
    /// No cursor (hidden while over the view).
    Hidden,
}

/// A native context menu description. Item ids are chosen by the app and come back in
/// [`Event::MenuItemSelected`].
///
/// ```
/// use pixl_gfx::Menu;
/// let menu = Menu::new()
///     .item(1, "Сбросить")
///     .submenu("Назначить на макро", Menu::new().item(11, "1").item(12, "2").checked_item(13, "3", true))
///     .separator()
///     .disabled_item(2, "Убрать из макро");
/// assert_eq!(menu.items.len(), 4);
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Menu {
    /// Items in display order.
    pub items: Vec<MenuItem>,
}

/// One entry of a [`Menu`].
#[derive(Clone, Debug, PartialEq)]
pub enum MenuItem {
    /// A selectable item.
    Action {
        /// Id reported in [`Event::MenuItemSelected`].
        id: u32,
        /// Label.
        label: String,
        /// Shows a check mark.
        checked: bool,
        /// Greyed out and not selectable when `false`.
        enabled: bool,
    },
    /// A nested menu.
    Submenu {
        /// Label.
        label: String,
        /// The nested menu.
        menu: Menu,
        /// Greyed out when `false`.
        enabled: bool,
    },
    /// A separator line.
    Separator,
}

impl Menu {
    /// An empty menu.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an enabled, unchecked item.
    #[must_use]
    pub fn item(mut self, id: u32, label: impl Into<String>) -> Self {
        self.items.push(MenuItem::Action { id, label: label.into(), checked: false, enabled: true });
        self
    }

    /// Appends an item with a check mark state.
    #[must_use]
    pub fn checked_item(mut self, id: u32, label: impl Into<String>, checked: bool) -> Self {
        self.items.push(MenuItem::Action { id, label: label.into(), checked, enabled: true });
        self
    }

    /// Appends a disabled (greyed out) item.
    #[must_use]
    pub fn disabled_item(mut self, id: u32, label: impl Into<String>) -> Self {
        self.items.push(MenuItem::Action { id, label: label.into(), checked: false, enabled: false });
        self
    }

    /// Appends a submenu.
    #[must_use]
    pub fn submenu(mut self, label: impl Into<String>, menu: Menu) -> Self {
        self.items.push(MenuItem::Submenu { label: label.into(), menu, enabled: true });
        self
    }

    /// Appends a separator.
    #[must_use]
    pub fn separator(mut self) -> Self {
        self.items.push(MenuItem::Separator);
        self
    }

    /// Appends an arbitrary item.
    pub fn push(&mut self, item: MenuItem) {
        self.items.push(item);
    }

    /// Whether the menu has no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Finds the item with `id` (searching submenus).
    pub fn find(&self, id: u32) -> Option<&MenuItem> {
        self.items.iter().find_map(|item| match item {
            MenuItem::Action { id: i, .. } if *i == id => Some(item),
            MenuItem::Submenu { menu, .. } => menu.find(id),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_set() {
        let mut b = MouseButtons::NONE;
        assert!(b.is_empty());
        b.insert(MouseButton::Left);
        b.insert(MouseButton::Right);
        assert!(b.contains(MouseButton::Left) && b.contains(MouseButton::Right));
        b.remove(MouseButton::Left);
        assert!(!b.contains(MouseButton::Left));
        assert!(!b.is_empty());
    }

    #[test]
    fn selectors_map_to_commands() {
        assert_eq!(TextCommand::from_selector("deleteBackward:"), TextCommand::DeleteBackward);
        assert_eq!(TextCommand::from_selector("moveLeftAndModifySelection:"), TextCommand::MoveLeft { select: true });
        assert_eq!(TextCommand::from_selector("insertNewline:"), TextCommand::InsertNewline);
        assert_eq!(TextCommand::from_selector("cancelOperation:"), TextCommand::Cancel);
        assert_eq!(TextCommand::from_selector("transpose:"), TextCommand::Other("transpose:".into()));
    }

    #[test]
    fn menu_find_searches_submenus() {
        let m = Menu::new().item(1, "a").submenu("b", Menu::new().checked_item(7, "c", true)).separator();
        assert!(matches!(m.find(7), Some(MenuItem::Action { checked: true, .. })));
        assert!(m.find(3).is_none());
    }

    #[test]
    fn fine_adjust_chord() {
        assert!(Modifiers { shift: true, ..Modifiers::NONE }.fine());
        assert!(Modifiers { cmd: true, ..Modifiers::NONE }.fine());
        assert!(!Modifiers { alt: true, ..Modifiers::NONE }.fine());
        assert!(Modifiers { caps_lock: true, ..Modifiers::NONE }.is_empty());
    }
}
