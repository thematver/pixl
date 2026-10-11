//! Typed views of the enum parameters.
//!
//! Enum parameters are stored as their variant index (as `f32`) in [`crate::ParamValues`].
//! The variant labels defined here are also the strings used by the preset format and the
//! host display, so they are part of the stable on-disk contract: never reorder or rename
//! variants, only append.

use serde::{Deserialize, Serialize};

macro_rules! pixl_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$vmeta:meta])* $variant:ident => $label:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
        pub enum $name {
            #[default]
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            /// All variants in index order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            /// Stable labels in index order (host display and preset files).
            pub const LABELS: &'static [&'static str] = &[$($label),+];

            /// Variant for an index; out-of-range indices clamp to the last variant.
            pub fn from_index(index: usize) -> Self {
                Self::ALL[index.min(Self::ALL.len() - 1)]
            }

            /// Variant for a plain parameter value (rounded index).
            pub fn from_plain(plain: f32) -> Self {
                let i = if plain.is_finite() { plain.round().max(0.0) as usize } else { 0 };
                Self::from_index(i)
            }

            /// Index of this variant.
            pub fn index(self) -> usize {
                self as usize
            }

            /// Stable label of this variant.
            pub fn label(self) -> &'static str {
                Self::LABELS[self as usize]
            }

            /// Case-insensitive lookup by label.
            pub fn from_label(label: &str) -> Option<Self> {
                Self::LABELS
                    .iter()
                    .position(|l| l.eq_ignore_ascii_case(label.trim()))
                    .map(Self::from_index)
            }
        }
    };
}

// Note: the first variant listed is the `Default` impl, which is only used for
// `Default::default()` on containers. Parameter defaults live in the parameter table.

pixl_enum! {
    /// Which sound chip PIXL emulates.
    ChipMode {
        /// Ricoh 2A03 (NES/Famicom): pulse duties 12.5/25/50/75 %, fixed 4-bit triangle,
        /// 16-entry noise period table, non-linear mixer, 11-bit timer pitch quantization.
        Nes => "NES",
        /// Sharp LR35902 / DMG (Game Boy): pulse duties, 32 × 4-bit wave RAM with 2-bit
        /// volume shift, 7/15-bit noise LFSR, linear mixer, hard L/C/R panning.
        Gb => "GB",
        /// No chip restrictions: continuous duty (PWM), continuous levels and pitch.
        Free => "FREE",
    }
}

impl ChipMode {
    /// Whether this mode applies chip restrictions (frame-rate stepping, 4-bit levels,
    /// register pitch quantization).
    pub fn is_chip(self) -> bool {
        !matches!(self, ChipMode::Free)
    }
}

pixl_enum! {
    /// Voice allocation mode.
    VoiceMode {
        /// Polyphonic, up to `polyphony` voices.
        Poly => "POLY",
        /// Monophonic, envelopes retrigger on every note.
        Mono => "MONO",
        /// Monophonic, overlapping notes do not retrigger envelopes.
        Legato => "LEGATO",
    }
}

pixl_enum! {
    /// Chip frame rate used for stepped modulation in NES/GB modes.
    FrameRate {
        /// 60.0988 Hz (NTSC). NES uses the NTSC CPU clock 1.789773 MHz.
        Ntsc => "NTSC",
        /// 50.0070 Hz (PAL). NES uses the PAL CPU clock 1.662607 MHz.
        Pal => "PAL",
    }
}

impl FrameRate {
    /// Frames per second.
    pub fn hz(self) -> f64 {
        match self {
            FrameRate::Ntsc => 60.0988,
            FrameRate::Pal => 50.0070,
        }
    }
}

pixl_enum! {
    /// One of the four chip channels.
    Channel {
        /// Pulse 1.
        P1 => "P1",
        /// Pulse 2.
        P2 => "P2",
        /// Wave channel (GB wave RAM / FREE wave RAM; NES triangle in NES mode).
        Wave => "WAVE",
        /// Noise channel (LFSR).
        Noise => "NOISE",
    }
}

pixl_enum! {
    /// Noise LFSR length.
    NoiseMode {
        /// 15-bit LFSR: white noise (hats, explosions).
        Long => "LONG",
        /// Short loop (NES mode 1 / GB 7-bit): metallic, tonal.
        Short => "SHORT",
    }
}

pixl_enum! {
    /// Pitch sweep direction. `Down` starts above the note and falls onto it.
    SweepDir {
        /// Starts `sweep_amt` semitones above the note and falls to the note.
        Down => "DOWN",
        /// Starts `sweep_amt` semitones below the note and rises to the note.
        Up => "UP",
    }
}

pixl_enum! {
    /// Curve shape used by sweep and glide.
    Curve {
        /// Exponential-style (fast start, slow settle). Sweep: offset · (1 − t/T)².
        Exp => "EXP",
        /// Linear.
        Lin => "LIN",
    }
}

pixl_enum! {
    /// Vibrato LFO shape.
    VibShape {
        /// Sine.
        Sine => "SINE",
        /// Triangle.
        Tri => "TRI",
        /// Square.
        Square => "SQUARE",
    }
}

pixl_enum! {
    /// Arpeggiator step order.
    ArpMode {
        /// Ascending.
        Up => "UP",
        /// Descending.
        Down => "DOWN",
        /// Ping-pong without repeating the end notes.
        UpDown => "UPDOWN",
        /// Random (deterministic per note-on seed).
        Random => "RANDOM",
        /// In the order the notes were played (HELD chord); equals UP for chord presets.
        Order => "ORDER",
    }
}

pixl_enum! {
    /// Tempo-synced arpeggiator step length.
    ArpDiv {
        /// Quarter note.
        D4 => "1/4",
        /// Eighth note.
        D8 => "1/8",
        /// Eighth-note triplet.
        D8T => "1/8T",
        /// Sixteenth note.
        D16 => "1/16",
        /// Sixteenth-note triplet.
        D16T => "1/16T",
        /// Thirty-second note.
        D32 => "1/32",
        /// Thirty-second-note triplet.
        D32T => "1/32T",
        /// Sixty-fourth note.
        D64 => "1/64",
    }
}

impl ArpDiv {
    /// Step length in quarter-note beats.
    pub fn beats(self) -> f64 {
        match self {
            ArpDiv::D4 => 1.0,
            ArpDiv::D8 => 0.5,
            ArpDiv::D8T => 1.0 / 3.0,
            ArpDiv::D16 => 0.25,
            ArpDiv::D16T => 1.0 / 6.0,
            ArpDiv::D32 => 0.125,
            ArpDiv::D32T => 1.0 / 12.0,
            ArpDiv::D64 => 0.0625,
        }
    }
}

pixl_enum! {
    /// Arpeggiator note source: the held notes, or a chord built on each played note.
    ArpChord {
        /// Arpeggiate the notes currently held (collapses to one arp voice).
        Held => "HELD",
        /// Major triad 0-4-7.
        Maj => "MAJ",
        /// Minor triad 0-3-7.
        Min => "MIN",
        /// 0-2-7.
        Sus2 => "SUS2",
        /// 0-5-7.
        Sus4 => "SUS4",
        /// 0-4-7-11.
        Maj7 => "MAJ7",
        /// 0-3-7-10.
        Min7 => "MIN7",
        /// 0-4-7-10.
        Dom7 => "DOM7",
        /// 0-3-6.
        Dim => "DIM",
        /// 0-4-8.
        Aug => "AUG",
        /// Power chord 0-7.
        Pow => "POW",
        /// Octave 0-12.
        Oct => "OCT",
    }
}

impl ArpChord {
    /// Semitone intervals of the chord (empty for `Held`).
    pub fn intervals(self) -> &'static [i8] {
        match self {
            ArpChord::Held => &[],
            ArpChord::Maj => &[0, 4, 7],
            ArpChord::Min => &[0, 3, 7],
            ArpChord::Sus2 => &[0, 2, 7],
            ArpChord::Sus4 => &[0, 5, 7],
            ArpChord::Maj7 => &[0, 4, 7, 11],
            ArpChord::Min7 => &[0, 3, 7, 10],
            ArpChord::Dom7 => &[0, 4, 7, 10],
            ArpChord::Dim => &[0, 3, 6],
            ArpChord::Aug => &[0, 4, 8],
            ArpChord::Pow => &[0, 7],
            ArpChord::Oct => &[0, 12],
        }
    }
}

pixl_enum! {
    /// When glide applies.
    GlideMode {
        /// On every note (from the previously played note).
        Always => "ALWAYS",
        /// Only between overlapping (legato) notes.
        Legato => "LEGATO",
    }
}

pixl_enum! {
    /// Tempo-synced echo time.
    EchoDiv {
        /// Quarter note.
        D4 => "1/4",
        /// Eighth note.
        D8 => "1/8",
        /// Dotted eighth.
        D8D => "1/8D",
        /// Eighth-note triplet.
        D8T => "1/8T",
        /// Sixteenth note.
        D16 => "1/16",
        /// Dotted sixteenth.
        D16D => "1/16D",
        /// Sixteenth-note triplet.
        D16T => "1/16T",
        /// Thirty-second note.
        D32 => "1/32",
    }
}

impl EchoDiv {
    /// Delay length in quarter-note beats.
    pub fn beats(self) -> f64 {
        match self {
            EchoDiv::D4 => 1.0,
            EchoDiv::D8 => 0.5,
            EchoDiv::D8D => 0.75,
            EchoDiv::D8T => 1.0 / 3.0,
            EchoDiv::D16 => 0.25,
            EchoDiv::D16D => 0.375,
            EchoDiv::D16T => 1.0 / 6.0,
            EchoDiv::D32 => 0.125,
        }
    }
}

pixl_enum! {
    /// SNES-style 8-tap echo FIR presets.
    EchoFir {
        /// Unfiltered repeats.
        Flat => "FLAT",
        /// Gentle low-pass, the classic 16-bit console echo.
        Warm => "WARM",
        /// Strong low-pass.
        Dark => "DARK",
        /// Band-limited, resonant "cave" colouring.
        Cave => "CAVE",
        /// High-passed, thin repeats.
        Bright => "BRIGHT",
        /// Narrow band-pass, telephone-like.
        Lofi => "LOFI",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_roundtrip() {
        for c in ChipMode::ALL {
            assert_eq!(ChipMode::from_label(c.label()), Some(*c));
        }
        assert_eq!(ArpDiv::from_label("1/16t"), Some(ArpDiv::D16T));
        assert_eq!(ChipMode::from_plain(1.4), ChipMode::Gb);
        assert_eq!(ChipMode::from_plain(99.0), ChipMode::Free);
        assert_eq!(ChipMode::from_plain(f32::NAN), ChipMode::Nes);
    }

    #[test]
    fn chord_intervals_start_at_root() {
        for c in ArpChord::ALL.iter().skip(1) {
            assert_eq!(c.intervals()[0], 0);
        }
    }
}
