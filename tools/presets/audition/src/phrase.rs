//! The test material played on each category: a musical phrase (loudness, image, macro
//! comparisons, WAV export), the notes used for the worst-case peak check, and the note
//! used for pitch and envelope timing.

use crate::host::{off, on, secs, At};
use pixl_core::preset::Category;

/// Velocity of the phrase notes (MIDI 100).
pub const PHRASE_VELOCITY: f32 = 100.0 / 127.0;

/// A category's test material.
pub struct Material {
    /// Events of the phrase.
    pub phrase: Vec<At>,
    /// Sample position of the phrase's last note-off.
    pub phrase_end: usize,
    /// Single notes checked at full velocity for the worst-case peak.
    pub peak_notes: &'static [u8],
    /// Note used for pitch and envelope timing, and how long it is held (s).
    pub probe_note: u8,
    pub probe_hold: f64,
}

/// Legato line: `(note, start s, end s)` events at `PHRASE_VELOCITY`.
fn line(notes: &[(u8, f64, f64)]) -> (Vec<At>, usize) {
    let mut ev = Vec::new();
    let mut end = 0;
    for &(n, a, b) in notes {
        ev.push(on(secs(a), n, PHRASE_VELOCITY));
        ev.push(off(secs(b), n));
        end = end.max(secs(b));
    }
    (ev, end)
}

/// Melody with 30 ms legato overlaps, a held note, then two detached repeats.
fn melody(base: u8) -> (Vec<At>, usize) {
    let b = base;
    line(&[
        (b, 0.00, 0.45),
        (b + 4, 0.42, 0.87),
        (b + 7, 0.84, 1.29),
        (b + 5, 0.84 + 0.42, 1.71),
        (b + 2, 1.68, 3.20),
        (b, 3.60, 3.90),
        (b, 4.00, 4.30),
    ])
}

/// Test material for a category.
pub fn material(category: Category) -> Material {
    match category {
        Category::Lead => {
            let (phrase, phrase_end) = melody(72);
            Material { phrase, phrase_end, peak_notes: &[60, 72, 84], probe_note: 72, probe_hold: 2.0 }
        }
        Category::Bass => {
            let (phrase, phrase_end) = melody(36);
            Material { phrase, phrase_end, peak_notes: &[28, 36, 48], probe_note: 40, probe_hold: 2.0 }
        }
        Category::Arp => {
            // A held triad, then a single held note.
            let (phrase, phrase_end) = line(&[(60, 0.0, 2.5), (64, 0.0, 2.5), (67, 0.0, 2.5), (57, 3.0, 4.5)]);
            Material { phrase, phrase_end, peak_notes: &[48, 60, 72], probe_note: 60, probe_hold: 2.0 }
        }
        Category::Pad => {
            // Two four-note chords, changed legato.
            let (phrase, phrase_end) = line(&[
                (48, 0.0, 4.0),
                (55, 0.0, 4.0),
                (60, 0.0, 4.0),
                (64, 0.0, 4.0),
                (45, 4.0, 7.0),
                (52, 4.0, 7.0),
                (57, 4.0, 7.0),
                (60, 4.0, 7.0),
            ]);
            Material { phrase, phrase_end, peak_notes: &[48, 60, 72], probe_note: 60, probe_hold: 3.5 }
        }
        Category::Drum => {
            // Hits across the keyboard, including a fast repeat.
            let (phrase, phrase_end) = line(&[
                (36, 0.000, 0.100),
                (48, 0.500, 0.600),
                (60, 1.000, 1.100),
                (72, 1.500, 1.600),
                (60, 2.000, 2.100),
                (60, 2.125, 2.225),
            ]);
            Material { phrase, phrase_end, peak_notes: &[36, 48, 60, 72], probe_note: 48, probe_hold: 1.0 }
        }
        Category::Sfx => {
            let (phrase, phrase_end) = line(&[(60, 0.0, 1.0), (72, 2.0, 3.0)]);
            Material { phrase, phrase_end, peak_notes: &[48, 60, 72, 84], probe_note: 72, probe_hold: 1.0 }
        }
    }
}

/// A single note held for `hold` seconds at `velocity`.
pub fn single(note: u8, velocity: f32, hold: f64) -> (Vec<At>, usize) {
    (vec![on(0, note, velocity), off(secs(hold), note)], secs(hold))
}
