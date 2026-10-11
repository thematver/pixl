//! Preset and preset-library file formats.
//!
//! A preset is a JSON document:
//!
//! ```json
//! {
//!   "format": "pixl-preset", "version": 1,
//!   "name": "Hyrule Fanfare", "category": "LEAD", "number": 14, "icon": "wav",
//!   "tags": ["GB", "LEAD", "WAVE"],
//!   "description": { "ru": "…", "en": "…" },
//!   "author": "PIXL Factory",
//!   "params": { "chip": "GB", "wv_on": true, "wv_level": 12, "sweep_amt": 12, … },
//!   "wave": [10, 11, …32 values…],
//!   "macros": [[{"target": "crush_bits", "depth": -0.25}], [], [], []]
//! }
//! ```
//!
//! Parameter values are plain values keyed by [`crate::ParamSpec::key`]; enums may be given
//! as their label (preferred) or index, booleans as `true`/`false` or numbers. Parameters
//! missing from `params` take their default, unknown keys are ignored (forward
//! compatibility). The factory library is `{ "format": "pixl-library", "version": 1,
//! "presets": [ … ] }`.

use crate::enums::ChipMode;
use crate::macros::MacroAssignments;
use crate::params::{ParamId, ParamKind, ParamValues, PARAMS};
use crate::wave::{self, WaveRam};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Preset file format tag.
pub const PRESET_FORMAT: &str = "pixl-preset";
/// Library file format tag.
pub const LIBRARY_FORMAT: &str = "pixl-library";
/// Current format version.
pub const FORMAT_VERSION: u32 = 1;
/// User preset file extension.
pub const PRESET_EXTENSION: &str = "pixlpreset";

/// Preset category (browser filter).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Category {
    #[default]
    Lead,
    Bass,
    Arp,
    Pad,
    Drum,
    Sfx,
}

impl Category {
    pub const ALL: [Category; 6] =
        [Category::Lead, Category::Bass, Category::Arp, Category::Pad, Category::Drum, Category::Sfx];

    /// Upper-case tag used in the header pill and preset list ("LEAD").
    pub fn tag(self) -> &'static str {
        match self {
            Category::Lead => "LEAD",
            Category::Bass => "BASS",
            Category::Arp => "ARP",
            Category::Pad => "PAD",
            Category::Drum => "DRUM",
            Category::Sfx => "SFX",
        }
    }

    /// Title-case name used in the browser sidebar ("Lead").
    pub fn title(self) -> &'static str {
        match self {
            Category::Lead => "Lead",
            Category::Bass => "Bass",
            Category::Arp => "Arp",
            Category::Pad => "Pad",
            Category::Drum => "Drum",
            Category::Sfx => "SFX",
        }
    }
}

/// Small waveform glyph shown next to a preset (design icon set).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PresetIcon {
    /// Narrow pulse.
    #[serde(rename = "p12")]
    P12,
    /// Square.
    #[default]
    #[serde(rename = "p50")]
    P50,
    /// Triangle.
    #[serde(rename = "tri")]
    Tri,
    /// Drawn wave.
    #[serde(rename = "wav")]
    Wav,
    /// Noise.
    #[serde(rename = "noi")]
    Noi,
}

impl PresetIcon {
    /// Tag shown in the browser detail panel ("PULSE", "WAVE", …).
    pub fn tag(self) -> &'static str {
        match self {
            PresetIcon::P12 | PresetIcon::P50 => "PULSE",
            PresetIcon::Tri => "TRI",
            PresetIcon::Wav => "WAVE",
            PresetIcon::Noi => "NOISE",
        }
    }
}

/// Text in the two shipped UI languages.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LocalizedText {
    #[serde(default)]
    pub ru: String,
    #[serde(default)]
    pub en: String,
}

impl LocalizedText {
    /// Text for a language, falling back to the other one when empty.
    pub fn get(&self, lang: crate::bridge::Lang) -> &str {
        let (a, b) = match lang {
            crate::bridge::Lang::Ru => (&self.ru, &self.en),
            crate::bridge::Lang::En => (&self.en, &self.ru),
        };
        if a.is_empty() {
            b
        } else {
            a
        }
    }
}

/// A preset document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default = "default_version")]
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub category: Category,
    /// Display number (factory 1..=128; user presets 0).
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    pub icon: PresetIcon,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub description: LocalizedText,
    #[serde(default)]
    pub author: String,
    /// Plain parameter values keyed by parameter key.
    #[serde(default)]
    pub params: Map<String, Value>,
    /// Wave RAM; absent means the default wave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave: Option<Vec<u8>>,
    #[serde(default)]
    pub macros: MacroAssignments,
}

fn default_format() -> String {
    PRESET_FORMAT.to_string()
}
fn default_version() -> u32 {
    FORMAT_VERSION
}

/// The fully resolved state a preset describes.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetState {
    pub values: ParamValues,
    pub wave: WaveRam,
    pub macros: MacroAssignments,
}

impl Preset {
    /// Build a preset document from a complete state. All parameters are written so the
    /// file is self-describing and immune to future default changes.
    pub fn from_state(name: &str, category: Category, state: &PresetState) -> Self {
        let mut params = Map::new();
        for spec in PARAMS.iter() {
            let v = state.values.get(spec.id);
            let json = match spec.kind {
                ParamKind::Bool => Value::Bool(v >= 0.5),
                ParamKind::Enum { variants } => Value::String(variants[v as usize].to_string()),
                ParamKind::Int { .. } => Value::from(v.round() as i64),
                ParamKind::Float { .. } => Value::from(((v as f64) * 10_000.0).round() / 10_000.0),
            };
            params.insert(spec.key.to_string(), json);
        }
        let chip = state.values.chip();
        let icon = guess_icon(&state.values);
        Preset {
            format: default_format(),
            version: FORMAT_VERSION,
            name: name.to_string(),
            category,
            number: 0,
            icon,
            tags: vec![chip.label().to_string(), category.tag().to_string(), icon.tag().to_string()],
            description: LocalizedText::default(),
            author: String::new(),
            params,
            wave: Some(state.wave.to_vec()),
            macros: state.macros,
        }
    }

    /// Resolve into a complete state. Returns the state plus human-readable warnings for
    /// values that had to be ignored or clamped.
    pub fn to_state(&self) -> (PresetState, Vec<String>) {
        let mut warnings = Vec::new();
        let mut values = ParamValues::defaults();
        for (key, json) in &self.params {
            let Some(id) = ParamId::from_key(key) else {
                warnings.push(format!("unknown parameter '{key}' ignored"));
                continue;
            };
            match json_to_plain(id, json) {
                Some(v) => {
                    let s = id.sanitize(v);
                    if (s - v).abs() > 1e-4 {
                        warnings.push(format!("'{key}' = {v} clamped to {s}"));
                    }
                    values.set(id, s);
                }
                None => warnings.push(format!("'{key}' has an invalid value {json}")),
            }
        }
        let wave = match &self.wave {
            Some(w) => {
                if w.len() != wave::WAVE_LEN || w.iter().any(|v| *v > wave::WAVE_MAX) {
                    warnings.push("wave RAM malformed; sanitized".to_string());
                }
                wave::sanitize(w)
            }
            None => wave::default_wave(),
        };
        let mut macros = self.macros;
        macros.sanitize();
        (PresetState { values, wave, macros }, warnings)
    }

    /// Chip mode stored in the preset.
    pub fn chip(&self) -> ChipMode {
        self.params
            .get(ParamId::Chip.key())
            .and_then(|v| json_to_plain(ParamId::Chip, v))
            .map(ChipMode::from_plain)
            .unwrap_or(ChipMode::from_plain(ParamId::Chip.default_value()))
    }

    /// Parse a single preset document.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let p: Preset = serde_json::from_str(json).map_err(|e| format!("invalid preset: {e}"))?;
        if p.format != PRESET_FORMAT {
            return Err(format!("not a PIXL preset (format '{}')", p.format));
        }
        if p.version > FORMAT_VERSION {
            return Err(format!("preset version {} is newer than this PIXL ({FORMAT_VERSION})", p.version));
        }
        if p.name.trim().is_empty() {
            return Err("preset has no name".to_string());
        }
        Ok(p)
    }

    /// Serialize as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("preset serialization cannot fail")
    }
}

/// Convert a JSON preset value into a plain value for `id`.
pub fn json_to_plain(id: ParamId, json: &Value) -> Option<f32> {
    match (id.spec().kind, json) {
        (_, Value::Bool(b)) => Some(if *b { 1.0 } else { 0.0 }),
        (ParamKind::Enum { variants }, Value::String(s)) => {
            variants.iter().position(|v| v.eq_ignore_ascii_case(s)).map(|i| i as f32)
        }
        (_, Value::String(s)) => id.parse(s),
        (_, Value::Number(n)) => n.as_f64().map(|v| v as f32).filter(|v| v.is_finite()),
        _ => None,
    }
}

/// Best-guess list icon for a state (primary enabled channel).
pub fn guess_icon(v: &ParamValues) -> PresetIcon {
    use crate::enums::Channel;
    use crate::params::ChannelField;
    let on = |c| v.bool(ParamId::channel(c, ChannelField::On));
    let level = |c| if on(c) { v.channel(c, ChannelField::Level) } else { -1.0 };
    let best = Channel::ALL.iter().copied().max_by(|a, b| level(*a).total_cmp(&level(*b))).unwrap_or(Channel::P1);
    match best {
        Channel::P1 | Channel::P2 => {
            let duty = v.get(ParamId::duty(best).unwrap_or(ParamId::P1Duty));
            if (duty - 50.0).abs() < 13.0 {
                PresetIcon::P50
            } else {
                PresetIcon::P12
            }
        }
        Channel::Wave if v.chip() == ChipMode::Nes => PresetIcon::Tri,
        Channel::Wave => PresetIcon::Wav,
        Channel::Noise => PresetIcon::Noi,
    }
}

/// A collection of presets (the embedded factory library).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetLibrary {
    pub format: String,
    pub version: u32,
    pub presets: Vec<Preset>,
}

impl PresetLibrary {
    /// Parse and validate a library document.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let lib: PresetLibrary = serde_json::from_str(json).map_err(|e| format!("invalid preset library: {e}"))?;
        if lib.format != LIBRARY_FORMAT {
            return Err(format!("not a PIXL preset library (format '{}')", lib.format));
        }
        if lib.version > FORMAT_VERSION {
            return Err(format!("library version {} is newer than this PIXL", lib.version));
        }
        Ok(lib)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_full_state() {
        let mut values = ParamValues::defaults();
        values.set(ParamId::Chip, 0.0);
        values.set(ParamId::P1On, 1.0);
        values.set(ParamId::P1Duty, 12.5);
        values.set(ParamId::SweepTime, 333.3);
        let mut macros = MacroAssignments::empty();
        macros.set(0, ParamId::CrushBits, -0.25);
        let state = PresetState { values, wave: wave::sine(), macros };
        let p = Preset::from_state("Test", Category::Bass, &state);
        let json = p.to_json();
        let back = Preset::from_json(&json).unwrap();
        let (s2, warnings) = back.to_state();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(s2.wave, state.wave);
        assert_eq!(s2.macros, state.macros);
        for id in ParamId::ALL {
            assert!((s2.values.get(*id) - state.values.get(*id)).abs() < 1e-3, "{}", id.key());
        }
        assert_eq!(back.chip(), ChipMode::Nes);
    }

    #[test]
    fn sparse_and_lenient_parsing() {
        let json = r#"{"format":"pixl-preset","version":1,"name":"X","category":"SFX","icon":"noi",
            "params":{"chip":"nes","ns_on":true,"ns_period":3,"echo_fir":2,"bogus":5,"crush_bits":99}}"#;
        let p = Preset::from_json(json).unwrap();
        let (s, warnings) = p.to_state();
        assert_eq!(s.values.chip(), ChipMode::Nes);
        assert!(s.values.bool(ParamId::NsOn));
        assert_eq!(s.values.int(ParamId::NsPeriod), 3);
        assert_eq!(s.values.echo_fir(), crate::enums::EchoFir::Dark);
        assert_eq!(s.values.get(ParamId::CrushBits), 16.0);
        assert_eq!(s.values.get(ParamId::GlideTime), ParamId::GlideTime.default_value());
        assert_eq!(warnings.len(), 2);
        assert_eq!(s.wave, wave::default_wave());
    }

    #[test]
    fn rejects_foreign_documents() {
        assert!(Preset::from_json(r#"{"format":"other","name":"x"}"#).is_err());
        assert!(Preset::from_json(r#"{"format":"pixl-preset","version":99,"name":"x"}"#).is_err());
        assert!(Preset::from_json("not json").is_err());
    }
}
