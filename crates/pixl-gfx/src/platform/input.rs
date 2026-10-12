//! Translation of AppKit input into pixl-gfx events. Everything that does not need a live
//! `NSEvent` is a pure function with unit tests.

use crate::event::{Key, KeyEvent, Modifiers, MouseButton, ScrollPhase};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventPhase};

/// Modifier state from AppKit flags.
pub(super) fn modifiers(flags: NSEventModifierFlags) -> Modifiers {
    Modifiers {
        shift: flags.contains(NSEventModifierFlags::Shift),
        ctrl: flags.contains(NSEventModifierFlags::Control),
        alt: flags.contains(NSEventModifierFlags::Option),
        cmd: flags.contains(NSEventModifierFlags::Command),
        caps_lock: flags.contains(NSEventModifierFlags::CapsLock),
    }
}

/// Modifier state of `event`.
pub(super) fn event_modifiers(event: &NSEvent) -> Modifiers {
    modifiers(event.modifierFlags())
}

/// The button of an `otherMouse…` event (AppKit numbers: 2 = middle, 3+ = extra buttons).
pub(super) fn other_button(number: isize) -> MouseButton {
    match number {
        2 => MouseButton::Middle,
        n => MouseButton::Other(n.clamp(3, 255) as u8),
    }
}

/// Maps a trackpad gesture phase. `None` means "drop the event": `MayBegin` (fingers rest on
/// the pad, nothing scrolls yet) carries no motion and is followed by `Began` or `Cancelled`.
pub(super) fn scroll_phase(phase: NSEventPhase) -> Option<ScrollPhase> {
    if phase.contains(NSEventPhase::Began) {
        Some(ScrollPhase::Began)
    } else if phase.intersects(NSEventPhase::Ended | NSEventPhase::Cancelled) {
        Some(ScrollPhase::Ended)
    } else if phase.intersects(NSEventPhase::Changed | NSEventPhase::Stationary) {
        Some(ScrollPhase::Changed)
    } else if phase.contains(NSEventPhase::MayBegin) {
        None
    } else {
        Some(ScrollPhase::None)
    }
}

/// AppKit encodes function keys (arrows, F-keys, Home…) as characters in the private-use
/// range U+F700..U+F8FF. They are not text.
fn is_function_key_char(c: char) -> bool {
    ('\u{F700}'..='\u{F8FF}').contains(&c)
}

/// The logical key for a hardware key code, falling back to the first character the key
/// produces (lower-cased) for character keys.
pub(super) fn key_from_code(code: u16, chars: &str) -> Key {
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
        _ => chars
            .chars()
            .find(|c| !c.is_control() && !is_function_key_char(*c))
            .and_then(|c| c.to_lowercase().next())
            .map_or(Key::Unknown, Key::Character),
    }
}

/// Builds a [`KeyEvent`] from its parts.
///
/// `chars` are the characters the key types with the current modifiers; `command_chars` are
/// the characters relevant for shortcuts (with ⌘ held: the layout's command character, so
/// ⌘C on a Cyrillic layout is `'c'`; otherwise the characters ignoring modifiers).
pub(super) fn key_event_from_parts(
    code: u16,
    chars: &str,
    command_chars: &str,
    mods: Modifiers,
    repeat: bool,
) -> KeyEvent {
    let text: String = chars.chars().filter(|c| !is_function_key_char(*c) && !c.is_control()).collect();
    KeyEvent { key: key_from_code(code, command_chars), code, mods, repeat, text }
}

/// A [`KeyEvent`] for a key-down/up `NSEvent`.
pub(super) fn key_event(event: &NSEvent) -> KeyEvent {
    let mods = event_modifiers(event);
    let chars = event.characters().map(|s| s.to_string()).unwrap_or_default();
    let command_chars = if mods.cmd {
        event.charactersByApplyingModifiers(NSEventModifierFlags::Command)
    } else {
        event.charactersIgnoringModifiers()
    }
    .map(|s| s.to_string())
    .unwrap_or_default();
    key_event_from_parts(event.keyCode(), &chars, &command_chars, mods, event.isARepeat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_flags() {
        let m = modifiers(NSEventModifierFlags::Shift | NSEventModifierFlags::Command);
        assert!(m.shift && m.cmd && !m.alt && !m.ctrl && !m.caps_lock);
        assert!(m.fine());
        let caps = modifiers(NSEventModifierFlags::CapsLock);
        assert!(caps.caps_lock && caps.is_empty());
        assert_eq!(modifiers(NSEventModifierFlags::empty()), Modifiers::NONE);
    }

    #[test]
    fn navigation_and_function_keys() {
        assert_eq!(key_from_code(123, "\u{F702}"), Key::ArrowLeft);
        assert_eq!(key_from_code(76, "\u{3}"), Key::Enter);
        assert_eq!(key_from_code(53, "\u{1b}"), Key::Escape);
        assert_eq!(key_from_code(90, ""), Key::F(20));
        assert_eq!(key_from_code(49, " "), Key::Space);
    }

    #[test]
    fn character_keys_follow_the_layout() {
        // Cyrillic layout: the key labelled A types "ф".
        assert_eq!(key_from_code(0, "Ф"), Key::Character('ф'));
        assert_eq!(key_from_code(8, "c"), Key::Character('c'));
        assert_eq!(key_from_code(200, ""), Key::Unknown);
        assert_eq!(key_from_code(200, "\u{F710}"), Key::Unknown);
    }

    #[test]
    fn key_event_text_excludes_function_key_characters() {
        let arrow = key_event_from_parts(126, "\u{F700}", "\u{F700}", Modifiers::NONE, false);
        assert_eq!(arrow.key, Key::ArrowUp);
        assert!(arrow.text.is_empty());
        let cmd = Modifiers { cmd: true, ..Modifiers::NONE };
        // ⌘C on a Cyrillic layout: the typed character is "с", the command character "c".
        let copy = key_event_from_parts(8, "с", "c", cmd, false);
        assert_eq!(copy.key, Key::Character('c'));
        assert_eq!(copy.text, "с");
        let enter = key_event_from_parts(36, "\r", "\r", Modifiers::NONE, true);
        assert_eq!(enter.key, Key::Enter);
        assert!(enter.text.is_empty() && enter.repeat);
    }

    #[test]
    fn scroll_phases() {
        assert_eq!(scroll_phase(NSEventPhase::empty()), Some(ScrollPhase::None));
        assert_eq!(scroll_phase(NSEventPhase::Began), Some(ScrollPhase::Began));
        assert_eq!(scroll_phase(NSEventPhase::Changed), Some(ScrollPhase::Changed));
        assert_eq!(scroll_phase(NSEventPhase::Stationary), Some(ScrollPhase::Changed));
        assert_eq!(scroll_phase(NSEventPhase::Ended), Some(ScrollPhase::Ended));
        assert_eq!(scroll_phase(NSEventPhase::Cancelled), Some(ScrollPhase::Ended));
        assert_eq!(scroll_phase(NSEventPhase::MayBegin), None);
    }

    #[test]
    fn other_buttons() {
        assert_eq!(other_button(2), MouseButton::Middle);
        assert_eq!(other_button(4), MouseButton::Other(4));
        assert_eq!(other_button(-1), MouseButton::Other(3));
        assert_eq!(other_button(10_000), MouseButton::Other(255));
    }
}
