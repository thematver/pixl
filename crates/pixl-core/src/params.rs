//! The PIXL parameter table.
//!
//! Every automatable parameter is listed exactly once in [`PARAMS`], in [`ParamId`] order.
//! The plugin builds its host-facing parameters from this table, the DSP reads plain values
//! through [`ParamValues`], the GUI uses the ranges/units for drags and display, and presets
//! store values under [`ParamSpec::key`].
//!
//! Stability rules (they protect saved Logic projects and user presets):
//! - `key` strings never change once shipped. Host automation IDs are derived from them.
//! - Ranges may only be widened, never narrowed.
//! - New parameters are appended at the end of their group in the table; `ParamId`
//!   discriminants are internal and may be renumbered, keys may not.
//!
//! Value domains ("plain" values, all stored as `f32`):
//! - `Float`: the value in its unit, `min..=max`.
//! - `Int`: integer value stored as float.
//! - `Bool`: 0.0 or 1.0.
//! - `Enum`: variant index stored as float.
//!
//! Normalization (0..1) matches nih-plug's `FloatRange::Skewed` exactly:
//! `norm = ((plain - min) / (max - min)).powf(skew)`, with `skew == 1.0` meaning linear.

use crate::enums::*;

/// Kind and range of a parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamKind {
    /// Continuous value with optional skew (exponent applied in the normalized domain).
    Float { min: f32, max: f32, skew: f32 },
    /// Integer value.
    Int { min: i32, max: i32 },
    /// Toggle.
    Bool,
    /// One of a fixed set of labelled variants.
    Enum { variants: &'static [&'static str] },
}

/// Display unit of a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    None,
    Db,
    Percent,
    Semitones,
    Cents,
    Ms,
    Hz,
    /// Chip level 0..15.
    Level,
    /// Pan −100 (L) .. +100 (R).
    Pan,
    Bits,
    Voices,
    Octaves,
    /// Noise period index 0..15, displayed 1-based as "n/16".
    Period,
}

/// Static description of one parameter.
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub id: ParamId,
    /// Stable string ID (host automation identity, preset key). Never change.
    pub key: &'static str,
    /// Host-visible name, e.g. "Pulse 1 Level".
    pub name: &'static str,
    /// Compact label for the GUI, e.g. "LEVEL".
    pub short: &'static str,
    /// Host group path, e.g. "Pulse 1".
    pub group: &'static str,
    pub kind: ParamKind,
    /// Default plain value.
    pub default: f32,
    pub unit: Unit,
    /// Whether macros may target this parameter.
    pub modulatable: bool,
}

macro_rules! param_ids {
    ($($id:ident),+ $(,)?) => {
        /// Index of a parameter in [`PARAMS`] and [`ParamValues`].
        #[repr(u16)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum ParamId { $($id),+ }

        impl ParamId {
            /// All parameter IDs in table order.
            pub const ALL: &'static [ParamId] = &[$(ParamId::$id),+];
        }

        /// Number of parameters.
        pub const PARAM_COUNT: usize = [$(stringify!($id)),+].len();
    };
}

param_ids! {
    // Global
    Chip, OutGain, VoiceMode, Polyphony, FrameRate, PbRange, VelSens, Tune, Analog,
    // Pulse 1
    P1On, P1Level, P1Pan, P1Coarse, P1Fine, P1Attack, P1Decay, P1Sustain, P1Release, P1Duty,
    // Pulse 2
    P2On, P2Level, P2Pan, P2Coarse, P2Fine, P2Attack, P2Decay, P2Sustain, P2Release, P2Duty,
    // Wave
    WvOn, WvLevel, WvPan, WvCoarse, WvFine, WvAttack, WvDecay, WvSustain, WvRelease,
    // Noise
    NsOn, NsLevel, NsPan, NsCoarse, NsFine, NsAttack, NsDecay, NsSustain, NsRelease,
    NsMode, NsPeriod, NsTrack,
    // Sweep
    SweepOn, SweepAmt, SweepDir, SweepTime, SweepCurve,
    // Vibrato
    VibOn, VibRate, VibDepth, VibDelay, VibFade, VibShape,
    // Arpeggiator
    ArpOn, ArpMode, ArpRate, ArpSync, ArpDiv, ArpOct, ArpChord,
    // Bitcrusher
    CrushOn, CrushBits, CrushRate,
    // Glide
    GlideOn, GlideTime, GlideMode, GlideCurve,
    // Unison
    UniOn, UniVoices, UniDetune, UniWidth,
    // Echo
    EchoOn, EchoTime, EchoSync, EchoDiv, EchoFb, EchoMix, EchoFir, EchoWidth,
    // Macros
    Macro1, Macro2, Macro3, Macro4,
}

/// Per-channel parameter fields shared by all four channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelField {
    On,
    Level,
    Pan,
    Coarse,
    Fine,
    Attack,
    Decay,
    Sustain,
    Release,
}

impl ParamId {
    /// Static description.
    pub fn spec(self) -> &'static ParamSpec {
        &PARAMS[self as usize]
    }

    /// Table index.
    pub fn index(self) -> usize {
        self as usize
    }

    /// ID for a table index.
    pub fn from_index(i: usize) -> Option<ParamId> {
        ParamId::ALL.get(i).copied()
    }

    /// Stable string key.
    pub fn key(self) -> &'static str {
        self.spec().key
    }

    /// Lookup by stable string key.
    pub fn from_key(key: &str) -> Option<ParamId> {
        PARAMS.iter().find(|p| p.key == key).map(|p| p.id)
    }

    /// The parameter for `field` of `channel`.
    pub fn channel(channel: Channel, field: ChannelField) -> ParamId {
        use ChannelField as F;
        let base = match channel {
            Channel::P1 => ParamId::P1On,
            Channel::P2 => ParamId::P2On,
            Channel::Wave => ParamId::WvOn,
            Channel::Noise => ParamId::NsOn,
        } as usize;
        let off = match field {
            F::On => 0,
            F::Level => 1,
            F::Pan => 2,
            F::Coarse => 3,
            F::Fine => 4,
            F::Attack => 5,
            F::Decay => 6,
            F::Sustain => 7,
            F::Release => 8,
        };
        ParamId::ALL[base + off]
    }

    /// Duty parameter for a pulse channel.
    pub fn duty(channel: Channel) -> Option<ParamId> {
        match channel {
            Channel::P1 => Some(ParamId::P1Duty),
            Channel::P2 => Some(ParamId::P2Duty),
            _ => None,
        }
    }

    /// Macro value parameter `0..4`.
    pub fn macro_value(index: usize) -> ParamId {
        [ParamId::Macro1, ParamId::Macro2, ParamId::Macro3, ParamId::Macro4][index.min(3)]
    }

    /// Default plain value.
    pub fn default_value(self) -> f32 {
        self.spec().default
    }

    /// Plain → normalized 0..1 (nih-plug compatible).
    pub fn normalize(self, plain: f32) -> f32 {
        self.spec().normalize(plain)
    }

    /// Normalized 0..1 → plain (nih-plug compatible, snapped for discrete kinds).
    pub fn denormalize(self, norm: f32) -> f32 {
        self.spec().denormalize(norm)
    }

    /// Clamp and snap a plain value into the parameter's domain.
    pub fn sanitize(self, plain: f32) -> f32 {
        self.spec().sanitize(plain)
    }

    /// Host display string (value with unit).
    pub fn format(self, plain: f32) -> String {
        self.spec().format(plain)
    }

    /// Parse a host-entered string into a plain value.
    pub fn parse(self, text: &str) -> Option<f32> {
        self.spec().parse(text)
    }
}

impl ParamSpec {
    /// Plain range as `(min, max)`.
    pub fn range(&self) -> (f32, f32) {
        match self.kind {
            ParamKind::Float { min, max, .. } => (min, max),
            ParamKind::Int { min, max } => (min as f32, max as f32),
            ParamKind::Bool => (0.0, 1.0),
            ParamKind::Enum { variants } => (0.0, (variants.len() - 1) as f32),
        }
    }

    /// Number of discrete steps (`None` for continuous parameters).
    pub fn step_count(&self) -> Option<usize> {
        match self.kind {
            ParamKind::Float { .. } => None,
            ParamKind::Int { min, max } => Some((max - min) as usize),
            ParamKind::Bool => Some(1),
            ParamKind::Enum { variants } => Some(variants.len() - 1),
        }
    }

    /// Clamp and snap a plain value into the parameter's domain. Non-finite input yields
    /// the default.
    pub fn sanitize(&self, plain: f32) -> f32 {
        if !plain.is_finite() {
            return self.default;
        }
        let (min, max) = self.range();
        let v = plain.clamp(min, max);
        match self.kind {
            ParamKind::Float { .. } => v,
            _ => v.round(),
        }
    }

    /// Plain → normalized.
    pub fn normalize(&self, plain: f32) -> f32 {
        let v = self.sanitize(plain);
        let (min, max) = self.range();
        if max <= min {
            return 0.0;
        }
        let lin = (v - min) / (max - min);
        match self.kind {
            ParamKind::Float { skew, .. } if skew != 1.0 => lin.powf(skew),
            _ => lin,
        }
    }

    /// Normalized → plain.
    pub fn denormalize(&self, norm: f32) -> f32 {
        let n = if norm.is_finite() { norm.clamp(0.0, 1.0) } else { self.normalize(self.default) };
        let (min, max) = self.range();
        let lin = match self.kind {
            ParamKind::Float { skew, .. } if skew != 1.0 => n.powf(1.0 / skew),
            _ => n,
        };
        self.sanitize(min + lin * (max - min))
    }

    /// Host display string.
    pub fn format(&self, plain: f32) -> String {
        let v = self.sanitize(plain);
        match self.kind {
            ParamKind::Bool => (if v >= 0.5 { "On" } else { "Off" }).to_string(),
            ParamKind::Enum { variants } => variants[v as usize].to_string(),
            ParamKind::Int { .. } => {
                let i = v as i32;
                match self.unit {
                    Unit::Semitones => format!("{i:+} st"),
                    Unit::Voices => format!("{i}"),
                    Unit::Octaves => format!("{i} oct"),
                    Unit::Period => format!("{}/16", i + 1),
                    _ => format!("{i}"),
                }
            }
            ParamKind::Float { .. } => match self.unit {
                Unit::Db => format!("{v:.1} dB"),
                Unit::Percent => format!("{v:.0}%"),
                Unit::Semitones => format!("{v:.1} st"),
                Unit::Cents => {
                    if self.range().0 < 0.0 {
                        format!("{v:+.0} ct")
                    } else {
                        format!("{v:.0} ct")
                    }
                }
                Unit::Ms => {
                    if v >= 1000.0 {
                        format!("{:.2} s", v / 1000.0)
                    } else if v < 10.0 {
                        format!("{v:.1} ms")
                    } else {
                        format!("{v:.0} ms")
                    }
                }
                Unit::Hz => {
                    if v >= 1000.0 {
                        format!("{:.2} kHz", v / 1000.0)
                    } else if v < 10.0 {
                        format!("{v:.2} Hz")
                    } else {
                        format!("{v:.1} Hz")
                    }
                }
                Unit::Level => format!("{v:.1}"),
                Unit::Pan => {
                    if v.abs() < 0.5 {
                        "C".to_string()
                    } else if v < 0.0 {
                        format!("L{:.0}", -v)
                    } else {
                        format!("R{v:.0}")
                    }
                }
                Unit::Bits => format!("{v:.1} bit"),
                _ => format!("{v:.2}"),
            },
        }
    }

    /// Parse a host-entered string. Accepts the formats produced by [`Self::format`] as
    /// well as bare numbers; enum labels and on/off are case-insensitive.
    pub fn parse(&self, text: &str) -> Option<f32> {
        let t = text.trim();
        match self.kind {
            ParamKind::Bool => match t.to_ascii_lowercase().as_str() {
                "on" | "1" | "true" | "yes" => Some(1.0),
                "off" | "0" | "false" | "no" => Some(0.0),
                _ => None,
            },
            ParamKind::Enum { variants } => variants
                .iter()
                .position(|v| v.eq_ignore_ascii_case(t))
                .map(|i| i as f32)
                .or_else(|| t.parse::<f32>().ok().map(|v| self.sanitize(v))),
            _ => {
                let lower = t.to_ascii_lowercase();
                if self.unit == Unit::Pan {
                    if lower == "c" {
                        return Some(0.0);
                    }
                    if let Some(rest) = lower.strip_prefix('l') {
                        return rest.trim().parse::<f32>().ok().map(|v| self.sanitize(-v));
                    }
                    if let Some(rest) = lower.strip_prefix('r') {
                        return rest.trim().parse::<f32>().ok().map(|v| self.sanitize(v));
                    }
                }
                let (num_part, scale) = if let Some(n) = lower.strip_suffix("khz") {
                    (n, 1000.0)
                } else if let Some(n) = lower
                    .strip_suffix(" s")
                    .or_else(|| lower.strip_suffix('s'))
                    .filter(|_| self.unit == Unit::Ms && !lower.ends_with("ms"))
                {
                    (n, 1000.0)
                } else {
                    (lower.as_str(), 1.0)
                };
                let numeric: String = num_part
                    .trim()
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | '−'))
                    .map(|c| if c == '−' { '-' } else { c })
                    .collect();
                let v = numeric.parse::<f32>().ok()? * scale;
                let v = if self.unit == Unit::Period { v - 1.0 } else { v };
                Some(self.sanitize(v))
            }
        }
    }
}

/// Flat array of plain parameter values indexed by [`ParamId`].
///
/// This is what the DSP crates consume every block. It is `Copy` and contains no heap data,
/// so it can be built and passed around freely on the audio thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamValues(pub [f32; PARAM_COUNT]);

impl Default for ParamValues {
    fn default() -> Self {
        Self::defaults()
    }
}

impl ParamValues {
    /// All parameters at their defaults.
    pub fn defaults() -> Self {
        let mut v = [0.0; PARAM_COUNT];
        for (i, spec) in PARAMS.iter().enumerate() {
            v[i] = spec.default;
        }
        Self(v)
    }

    #[inline]
    pub fn get(&self, id: ParamId) -> f32 {
        self.0[id as usize]
    }

    /// Set a plain value (sanitized).
    #[inline]
    pub fn set(&mut self, id: ParamId, plain: f32) {
        self.0[id as usize] = id.sanitize(plain);
    }

    #[inline]
    pub fn bool(&self, id: ParamId) -> bool {
        self.get(id) >= 0.5
    }

    #[inline]
    pub fn int(&self, id: ParamId) -> i32 {
        self.get(id).round() as i32
    }

    #[inline]
    pub fn channel(&self, ch: Channel, field: ChannelField) -> f32 {
        self.get(ParamId::channel(ch, field))
    }

    #[inline]
    pub fn chip(&self) -> ChipMode {
        ChipMode::from_plain(self.get(ParamId::Chip))
    }
    #[inline]
    pub fn voice_mode(&self) -> VoiceMode {
        VoiceMode::from_plain(self.get(ParamId::VoiceMode))
    }
    #[inline]
    pub fn frame_rate(&self) -> FrameRate {
        FrameRate::from_plain(self.get(ParamId::FrameRate))
    }
    #[inline]
    pub fn noise_mode(&self) -> NoiseMode {
        NoiseMode::from_plain(self.get(ParamId::NsMode))
    }
    #[inline]
    pub fn sweep_dir(&self) -> SweepDir {
        SweepDir::from_plain(self.get(ParamId::SweepDir))
    }
    #[inline]
    pub fn sweep_curve(&self) -> Curve {
        Curve::from_plain(self.get(ParamId::SweepCurve))
    }
    #[inline]
    pub fn vib_shape(&self) -> VibShape {
        VibShape::from_plain(self.get(ParamId::VibShape))
    }
    #[inline]
    pub fn arp_mode(&self) -> ArpMode {
        ArpMode::from_plain(self.get(ParamId::ArpMode))
    }
    #[inline]
    pub fn arp_div(&self) -> ArpDiv {
        ArpDiv::from_plain(self.get(ParamId::ArpDiv))
    }
    #[inline]
    pub fn arp_chord(&self) -> ArpChord {
        ArpChord::from_plain(self.get(ParamId::ArpChord))
    }
    #[inline]
    pub fn glide_mode(&self) -> GlideMode {
        GlideMode::from_plain(self.get(ParamId::GlideMode))
    }
    #[inline]
    pub fn glide_curve(&self) -> Curve {
        Curve::from_plain(self.get(ParamId::GlideCurve))
    }
    #[inline]
    pub fn echo_div(&self) -> EchoDiv {
        EchoDiv::from_plain(self.get(ParamId::EchoDiv))
    }
    #[inline]
    pub fn echo_fir(&self) -> EchoFir {
        EchoFir::from_plain(self.get(ParamId::EchoFir))
    }
}

// ---------------------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------------------

const MS_SKEW: f32 = 0.25; // nih-plug FloatRange::skew_factor(-2.0)
const HZ_SKEW: f32 = 0.354; // ≈ skew_factor(-1.5)

const fn float(min: f32, max: f32) -> ParamKind {
    ParamKind::Float { min, max, skew: 1.0 }
}
const fn skewed(min: f32, max: f32, skew: f32) -> ParamKind {
    ParamKind::Float { min, max, skew }
}
const fn int(min: i32, max: i32) -> ParamKind {
    ParamKind::Int { min, max }
}
const fn enumk(variants: &'static [&'static str]) -> ParamKind {
    ParamKind::Enum { variants }
}

#[allow(clippy::too_many_arguments)]
const fn p(
    id: ParamId,
    key: &'static str,
    name: &'static str,
    short: &'static str,
    group: &'static str,
    kind: ParamKind,
    default: f32,
    unit: Unit,
    modulatable: bool,
) -> ParamSpec {
    ParamSpec { id, key, name, short, group, kind, default, unit, modulatable }
}

macro_rules! channel_params {
    ($on:ident, $level:ident, $pan:ident, $coarse:ident, $fine:ident, $a:ident, $d:ident, $s:ident, $r:ident,
     $prefix:literal, $group:literal, $default_on:expr) => {
        [
            p(
                ParamId::$on,
                concat!($prefix, "_on"),
                concat!($group, " On"),
                "ON",
                $group,
                ParamKind::Bool,
                $default_on,
                Unit::None,
                false,
            ),
            p(
                ParamId::$level,
                concat!($prefix, "_level"),
                concat!($group, " Level"),
                "VOL",
                $group,
                float(0.0, 15.0),
                12.0,
                Unit::Level,
                true,
            ),
            p(
                ParamId::$pan,
                concat!($prefix, "_pan"),
                concat!($group, " Pan"),
                "PAN",
                $group,
                float(-100.0, 100.0),
                0.0,
                Unit::Pan,
                true,
            ),
            p(
                ParamId::$coarse,
                concat!($prefix, "_coarse"),
                concat!($group, " Coarse"),
                "OCT",
                $group,
                int(-24, 24),
                0.0,
                Unit::Semitones,
                true,
            ),
            p(
                ParamId::$fine,
                concat!($prefix, "_fine"),
                concat!($group, " Fine"),
                "FINE",
                $group,
                float(-100.0, 100.0),
                0.0,
                Unit::Cents,
                true,
            ),
            p(
                ParamId::$a,
                concat!($prefix, "_attack"),
                concat!($group, " Attack"),
                "A",
                $group,
                skewed(0.0, 5000.0, MS_SKEW),
                2.0,
                Unit::Ms,
                true,
            ),
            p(
                ParamId::$d,
                concat!($prefix, "_decay"),
                concat!($group, " Decay"),
                "D",
                $group,
                skewed(0.0, 5000.0, MS_SKEW),
                400.0,
                Unit::Ms,
                true,
            ),
            p(
                ParamId::$s,
                concat!($prefix, "_sustain"),
                concat!($group, " Sustain"),
                "S",
                $group,
                float(0.0, 15.0),
                12.0,
                Unit::Level,
                true,
            ),
            p(
                ParamId::$r,
                concat!($prefix, "_release"),
                concat!($group, " Release"),
                "R",
                $group,
                skewed(0.0, 5000.0, MS_SKEW),
                150.0,
                Unit::Ms,
                true,
            ),
        ]
    };
}

const P1: [ParamSpec; 9] = channel_params!(
    P1On, P1Level, P1Pan, P1Coarse, P1Fine, P1Attack, P1Decay, P1Sustain, P1Release, "p1", "Pulse 1", 0.0
);
const P2: [ParamSpec; 9] = channel_params!(
    P2On, P2Level, P2Pan, P2Coarse, P2Fine, P2Attack, P2Decay, P2Sustain, P2Release, "p2", "Pulse 2", 0.0
);
const WV: [ParamSpec; 9] =
    channel_params!(WvOn, WvLevel, WvPan, WvCoarse, WvFine, WvAttack, WvDecay, WvSustain, WvRelease, "wv", "Wave", 1.0);
const NS: [ParamSpec; 9] = channel_params!(
    NsOn, NsLevel, NsPan, NsCoarse, NsFine, NsAttack, NsDecay, NsSustain, NsRelease, "ns", "Noise", 0.0
);

/// The parameter table, in [`ParamId`] order.
pub static PARAMS: [ParamSpec; PARAM_COUNT] = [
    // Global
    p(ParamId::Chip, "chip", "Chip", "CHIP", "Global", enumk(ChipMode::LABELS), 1.0, Unit::None, false),
    p(ParamId::OutGain, "out_gain", "Output", "OUT", "Global", float(-36.0, 12.0), 0.0, Unit::Db, true),
    p(
        ParamId::VoiceMode,
        "voice_mode",
        "Voice Mode",
        "MODE",
        "Global",
        enumk(VoiceMode::LABELS),
        0.0,
        Unit::None,
        false,
    ),
    p(ParamId::Polyphony, "polyphony", "Polyphony", "POLY", "Global", int(1, 16), 8.0, Unit::Voices, false),
    p(
        ParamId::FrameRate,
        "frame_rate",
        "Frame Rate",
        "FPS",
        "Global",
        enumk(FrameRate::LABELS),
        0.0,
        Unit::None,
        false,
    ),
    p(ParamId::PbRange, "pb_range", "Pitch Bend Range", "PB", "Global", int(0, 24), 2.0, Unit::Semitones, false),
    p(
        ParamId::VelSens,
        "vel_sens",
        "Velocity Sensitivity",
        "VEL",
        "Global",
        float(0.0, 100.0),
        50.0,
        Unit::Percent,
        true,
    ),
    p(ParamId::Tune, "tune", "Master Tune", "TUNE", "Global", float(-100.0, 100.0), 0.0, Unit::Cents, true),
    p(ParamId::Analog, "analog", "Analog Output", "ANALOG", "Global", ParamKind::Bool, 1.0, Unit::None, false),
    // Pulse 1
    P1[0],
    P1[1],
    P1[2],
    P1[3],
    P1[4],
    P1[5],
    P1[6],
    P1[7],
    P1[8],
    p(ParamId::P1Duty, "p1_duty", "Pulse 1 Duty", "DUTY", "Pulse 1", float(2.0, 98.0), 25.0, Unit::Percent, true),
    // Pulse 2
    P2[0],
    P2[1],
    P2[2],
    P2[3],
    P2[4],
    P2[5],
    P2[6],
    P2[7],
    P2[8],
    p(ParamId::P2Duty, "p2_duty", "Pulse 2 Duty", "DUTY", "Pulse 2", float(2.0, 98.0), 50.0, Unit::Percent, true),
    // Wave
    WV[0],
    WV[1],
    WV[2],
    WV[3],
    WV[4],
    WV[5],
    WV[6],
    WV[7],
    WV[8],
    // Noise
    NS[0],
    NS[1],
    NS[2],
    NS[3],
    NS[4],
    NS[5],
    NS[6],
    NS[7],
    NS[8],
    p(ParamId::NsMode, "ns_mode", "Noise Mode", "LFSR", "Noise", enumk(NoiseMode::LABELS), 0.0, Unit::None, false),
    p(ParamId::NsPeriod, "ns_period", "Noise Period", "PERIOD", "Noise", int(0, 15), 5.0, Unit::Period, true),
    p(ParamId::NsTrack, "ns_track", "Noise Key Track", "TRACK", "Noise", ParamKind::Bool, 0.0, Unit::None, false),
    // Sweep
    p(ParamId::SweepOn, "sweep_on", "Sweep On", "ON", "Sweep", ParamKind::Bool, 1.0, Unit::None, false),
    p(ParamId::SweepAmt, "sweep_amt", "Sweep Amount", "AMT", "Sweep", float(0.0, 48.0), 12.0, Unit::Semitones, true),
    p(
        ParamId::SweepDir,
        "sweep_dir",
        "Sweep Direction",
        "DIR",
        "Sweep",
        enumk(SweepDir::LABELS),
        0.0,
        Unit::None,
        false,
    ),
    p(ParamId::SweepTime, "sweep_time", "Sweep Time", "TIME", "Sweep", skewed(5.0, 2000.0, 0.3), 180.0, Unit::Ms, true),
    p(
        ParamId::SweepCurve,
        "sweep_curve",
        "Sweep Curve",
        "CURVE",
        "Sweep",
        enumk(Curve::LABELS),
        0.0,
        Unit::None,
        false,
    ),
    // Vibrato
    p(ParamId::VibOn, "vib_on", "Vibrato On", "ON", "Vibrato", ParamKind::Bool, 1.0, Unit::None, false),
    p(ParamId::VibRate, "vib_rate", "Vibrato Rate", "RATE", "Vibrato", skewed(0.1, 20.0, HZ_SKEW), 5.5, Unit::Hz, true),
    p(ParamId::VibDepth, "vib_depth", "Vibrato Depth", "DEPTH", "Vibrato", float(0.0, 100.0), 18.0, Unit::Cents, true),
    p(
        ParamId::VibDelay,
        "vib_delay",
        "Vibrato Delay",
        "DELAY",
        "Vibrato",
        skewed(0.0, 2000.0, 0.5),
        120.0,
        Unit::Ms,
        true,
    ),
    p(
        ParamId::VibFade,
        "vib_fade",
        "Vibrato Fade-In",
        "FADE",
        "Vibrato",
        skewed(0.0, 2000.0, 0.5),
        150.0,
        Unit::Ms,
        true,
    ),
    p(
        ParamId::VibShape,
        "vib_shape",
        "Vibrato Shape",
        "SHAPE",
        "Vibrato",
        enumk(VibShape::LABELS),
        0.0,
        Unit::None,
        false,
    ),
    // Arpeggiator
    p(ParamId::ArpOn, "arp_on", "Arp On", "ON", "Arp", ParamKind::Bool, 0.0, Unit::None, false),
    p(ParamId::ArpMode, "arp_mode", "Arp Mode", "MODE", "Arp", enumk(ArpMode::LABELS), 0.0, Unit::None, false),
    p(ParamId::ArpRate, "arp_rate", "Arp Rate", "RATE", "Arp", skewed(1.0, 60.0, 0.5), 20.0, Unit::Hz, true),
    p(ParamId::ArpSync, "arp_sync", "Arp Sync", "SYNC", "Arp", ParamKind::Bool, 0.0, Unit::None, false),
    p(ParamId::ArpDiv, "arp_div", "Arp Division", "DIV", "Arp", enumk(ArpDiv::LABELS), 3.0, Unit::None, false),
    p(ParamId::ArpOct, "arp_oct", "Arp Octaves", "OCT", "Arp", int(1, 4), 1.0, Unit::Octaves, true),
    p(ParamId::ArpChord, "arp_chord", "Arp Chord", "CHORD", "Arp", enumk(ArpChord::LABELS), 0.0, Unit::None, false),
    // Bitcrusher
    p(ParamId::CrushOn, "crush_on", "Crush On", "ON", "Crush", ParamKind::Bool, 0.0, Unit::None, false),
    p(ParamId::CrushBits, "crush_bits", "Crush Bits", "BITS", "Crush", float(1.0, 16.0), 6.0, Unit::Bits, true),
    p(
        ParamId::CrushRate,
        "crush_rate",
        "Crush Rate",
        "RATE",
        "Crush",
        skewed(1000.0, 48000.0, 0.3),
        11025.0,
        Unit::Hz,
        true,
    ),
    // Glide
    p(ParamId::GlideOn, "glide_on", "Glide On", "ON", "Glide", ParamKind::Bool, 1.0, Unit::None, false),
    p(ParamId::GlideTime, "glide_time", "Glide Time", "TIME", "Glide", skewed(1.0, 2000.0, 0.3), 90.0, Unit::Ms, true),
    p(
        ParamId::GlideMode,
        "glide_mode",
        "Glide Mode",
        "MODE",
        "Glide",
        enumk(GlideMode::LABELS),
        1.0,
        Unit::None,
        false,
    ),
    p(
        ParamId::GlideCurve,
        "glide_curve",
        "Glide Curve",
        "CURVE",
        "Glide",
        enumk(Curve::LABELS),
        0.0,
        Unit::None,
        false,
    ),
    // Unison
    p(ParamId::UniOn, "uni_on", "Unison On", "ON", "Unison", ParamKind::Bool, 1.0, Unit::None, false),
    p(ParamId::UniVoices, "uni_voices", "Unison Voices", "VOICES", "Unison", int(1, 4), 3.0, Unit::Voices, true),
    p(ParamId::UniDetune, "uni_detune", "Unison Detune", "DETUNE", "Unison", float(0.0, 50.0), 14.0, Unit::Cents, true),
    p(ParamId::UniWidth, "uni_width", "Unison Width", "WIDTH", "Unison", float(0.0, 100.0), 60.0, Unit::Percent, true),
    // Echo
    p(ParamId::EchoOn, "echo_on", "Echo On", "ON", "Echo", ParamKind::Bool, 0.0, Unit::None, false),
    p(ParamId::EchoTime, "echo_time", "Echo Time", "TIME", "Echo", skewed(10.0, 1000.0, 0.4), 250.0, Unit::Ms, true),
    p(ParamId::EchoSync, "echo_sync", "Echo Sync", "SYNC", "Echo", ParamKind::Bool, 1.0, Unit::None, false),
    p(ParamId::EchoDiv, "echo_div", "Echo Division", "DIV", "Echo", enumk(EchoDiv::LABELS), 2.0, Unit::None, false),
    p(ParamId::EchoFb, "echo_fb", "Echo Feedback", "FB", "Echo", float(0.0, 95.0), 40.0, Unit::Percent, true),
    p(ParamId::EchoMix, "echo_mix", "Echo Mix", "MIX", "Echo", float(0.0, 100.0), 25.0, Unit::Percent, true),
    p(ParamId::EchoFir, "echo_fir", "Echo Filter", "FIR", "Echo", enumk(EchoFir::LABELS), 1.0, Unit::None, false),
    p(ParamId::EchoWidth, "echo_width", "Echo Width", "WIDTH", "Echo", float(0.0, 100.0), 50.0, Unit::Percent, true),
    // Macros
    p(ParamId::Macro1, "macro1", "Macro 1", "M1", "Macros", float(0.0, 100.0), 62.0, Unit::Percent, false),
    p(ParamId::Macro2, "macro2", "Macro 2", "M2", "Macros", float(0.0, 100.0), 30.0, Unit::Percent, false),
    p(ParamId::Macro3, "macro3", "Macro 3", "M3", "Macros", float(0.0, 100.0), 45.0, Unit::Percent, false),
    p(ParamId::Macro4, "macro4", "Macro 4", "M4", "Macros", float(0.0, 100.0), 20.0, Unit::Percent, false),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn table_is_in_id_order_and_keys_unique() {
        let mut keys = HashSet::new();
        for (i, spec) in PARAMS.iter().enumerate() {
            assert_eq!(spec.id as usize, i, "PARAMS[{i}] ({}) out of order", spec.key);
            assert!(keys.insert(spec.key), "duplicate key {}", spec.key);
            assert!(spec.key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
        }
        assert_eq!(PARAMS.len(), PARAM_COUNT);
    }

    #[test]
    fn defaults_are_in_range_and_snapped() {
        for spec in PARAMS.iter() {
            assert_eq!(spec.sanitize(spec.default), spec.default, "{} default not sanitized", spec.key);
        }
    }

    #[test]
    fn normalization_roundtrips() {
        for spec in PARAMS.iter() {
            for k in 0..=20 {
                let n = k as f32 / 20.0;
                let plain = spec.denormalize(n);
                let n2 = spec.normalize(plain);
                let plain2 = spec.denormalize(n2);
                assert!((plain - plain2).abs() <= 1e-3 * (1.0 + plain.abs()), "{}: {plain} vs {plain2}", spec.key);
            }
            assert_eq!(spec.normalize(spec.range().0), 0.0);
            assert!((spec.normalize(spec.range().1) - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn channel_lookup() {
        assert_eq!(ParamId::channel(Channel::Wave, ChannelField::Release), ParamId::WvRelease);
        assert_eq!(ParamId::channel(Channel::Noise, ChannelField::On), ParamId::NsOn);
        assert_eq!(ParamId::channel(Channel::P2, ChannelField::Fine), ParamId::P2Fine);
        for ch in Channel::ALL {
            for f in [
                ChannelField::On,
                ChannelField::Level,
                ChannelField::Pan,
                ChannelField::Coarse,
                ChannelField::Fine,
                ChannelField::Attack,
                ChannelField::Decay,
                ChannelField::Sustain,
                ChannelField::Release,
            ] {
                let id = ParamId::channel(*ch, f);
                assert!(id.key().starts_with(match ch {
                    Channel::P1 => "p1_",
                    Channel::P2 => "p2_",
                    Channel::Wave => "wv_",
                    Channel::Noise => "ns_",
                }));
            }
        }
    }

    #[test]
    fn format_and_parse() {
        assert_eq!(ParamId::SweepTime.format(180.0), "180 ms");
        assert_eq!(ParamId::SweepTime.parse("180 ms"), Some(180.0));
        assert_eq!(ParamId::SweepTime.parse("1.5 s"), Some(1500.0));
        assert_eq!(ParamId::CrushRate.format(11025.0), "11.02 kHz");
        assert_eq!(ParamId::CrushRate.parse("11 kHz"), Some(11000.0));
        assert_eq!(ParamId::Chip.format(1.0), "GB");
        assert_eq!(ParamId::Chip.parse("free"), Some(2.0));
        assert_eq!(ParamId::P1Pan.format(-30.0), "L30");
        assert_eq!(ParamId::P1Pan.parse("R25"), Some(25.0));
        assert_eq!(ParamId::NsPeriod.format(5.0), "6/16");
        assert_eq!(ParamId::NsPeriod.parse("6/16"), Some(5.0));
        assert_eq!(ParamId::P1Coarse.format(-12.0), "-12 st");
        assert_eq!(ParamId::OutGain.parse("−6.2 dB"), Some(-6.2));
        assert_eq!(ParamId::SweepOn.parse("off"), Some(0.0));
    }

    #[test]
    fn skew_matches_nih_plug_formula() {
        // nih-plug: normalized = ((v - min)/(max - min))^factor
        let s = ParamId::P1Attack.spec();
        let v = 312.5;
        let expected = (v / 5000.0f32).powf(MS_SKEW);
        assert!((s.normalize(v) - expected).abs() < 1e-6);
    }

    #[test]
    fn values_accessors() {
        let v = ParamValues::defaults();
        assert_eq!(v.chip(), ChipMode::Gb);
        assert!(v.bool(ParamId::WvOn));
        assert!(!v.bool(ParamId::P1On));
        assert_eq!(v.int(ParamId::UniVoices), 3);
        assert_eq!(v.arp_div(), ArpDiv::D16);
        assert_eq!(v.echo_div(), EchoDiv::D8D);
        assert_eq!(v.glide_mode(), GlideMode::Legato);
    }
}
