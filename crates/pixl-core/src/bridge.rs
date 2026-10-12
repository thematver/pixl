//! The contract between the GUI (`pixl-gui`) and the plugin (`pixl-plugin`).
//!
//! The GUI never touches nih-plug or the DSP directly. Everything it needs goes through a
//! [`Bridge`] implementation: the plugin provides the real one (backed by nih-plug's
//! `GuiContext`, the shared lock-free state and the preset store), and `pixl-gui` ships a
//! `MockBridge` for the standalone harness and screenshot tests.
//!
//! Threading: all `Bridge` methods are called from the GUI (main) thread. Implementations
//! must not block for long (no disk I/O on hot paths such as `param`/`telemetry`).

use crate::enums::{Channel, ChipMode};
use crate::macros::MacroAssignments;
use crate::params::ParamId;
use crate::preset::{Category, LocalizedText, PresetIcon};
use crate::wave::WaveRam;
use serde::{Deserialize, Serialize};

/// Preset identifier: `"factory:014"` for factory presets, `"user:<file stem>"` for user
/// presets.
pub type PresetId = String;

/// Browser listing entry.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetInfo {
    pub id: PresetId,
    pub name: String,
    pub category: Category,
    /// Display number (factory 1..=128, user 0).
    pub number: u32,
    pub chip: ChipMode,
    pub icon: PresetIcon,
    pub tags: Vec<String>,
    pub description: LocalizedText,
    pub author: String,
    pub is_factory: bool,
    pub favorite: bool,
}

/// What the header pill shows.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CurrentPreset {
    /// `None` when the state did not come from a stored preset (e.g. a fresh instance).
    pub id: Option<PresetId>,
    pub name: String,
    pub category: Category,
    pub number: u32,
    /// Whether parameters/wave/macros changed since the preset was loaded or saved.
    pub modified: bool,
}

/// Live values from the audio thread for meters and animated displays.
///
/// Published by the plugin roughly every audio block; the GUI samples it once per frame.
/// Values may be slightly torn across fields; they are for display only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TelemetrySnapshot {
    /// Output peak level per channel (dBFS, ballistic: instant attack, ~300 ms release).
    pub peak_db: [f32; 2],
    /// Output RMS level per channel (dBFS, ~300 ms window).
    pub rms_db: [f32; 2],
    /// Max of both channels' peaks, held ~1.5 s (header "−6.2 DB" readout).
    pub peak_hold_db: f32,
    /// Number of sounding voices (including releasing ones).
    pub active_voices: u8,
    /// Bitset of held MIDI notes (bit n of `held_notes[n / 64]`).
    pub held_notes: [u64; 2],
    /// Most recently triggered note, if any voice is sounding.
    pub last_note: Option<u8>,
    /// Noise LFSR register of the most recent voice (15 bits).
    pub lfsr: u16,
    /// Current wave RAM step (0..32) of the most recent voice's wave channel.
    pub wave_step: Option<u8>,
    /// Current envelope level 0..1 per channel of the most recent voice.
    pub env_level: [f32; 4],
    /// Current pitch modulation offset in semitones (sweep + vibrato + glide + arp + bend)
    /// of the most recent voice.
    pub pitch_offset_st: f32,
    /// Current arpeggiator step index of the most recent voice.
    pub arp_step: Option<u8>,
    /// Chip frame counter (increments at the frame rate while audio runs).
    pub frame: u64,
}

/// Silence floor used for meters.
pub const METER_FLOOR_DB: f32 = -100.0;

impl Default for TelemetrySnapshot {
    fn default() -> Self {
        Self {
            peak_db: [METER_FLOOR_DB; 2],
            rms_db: [METER_FLOOR_DB; 2],
            peak_hold_db: METER_FLOOR_DB,
            active_voices: 0,
            held_notes: [0; 2],
            last_note: None,
            lfsr: 0x7fff,
            wave_step: None,
            env_level: [0.0; 4],
            pitch_offset_st: 0.0,
            arp_step: None,
            frame: 0,
        }
    }
}

impl TelemetrySnapshot {
    /// Whether `note` is currently held.
    pub fn is_held(&self, note: u8) -> bool {
        let n = note as usize;
        n < 128 && (self.held_notes[n / 64] >> (n % 64)) & 1 == 1
    }
}

/// UI language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Lang {
    /// Russian (the design's language for hints and descriptions).
    #[default]
    Ru,
    En,
}

/// Which editor the large hero display shows on top of the channel view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Focus {
    /// Envelope editor of the selected channel.
    Env,
    Sweep,
    Vibrato,
    Arp,
    Crush,
    Glide,
    Unison,
    Echo,
}

/// Hero view of the selected channel (kept for state compatibility; `Focus::Env` drives
/// the envelope view).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HeroView {
    #[default]
    Shape,
    Env,
}

/// Wave drawing tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WaveTool {
    #[default]
    Draw,
    Line,
}

/// Editor state persisted with the plugin state so a reopened editor looks the same.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Selected channel tab.
    pub channel: Channel,
    /// Open focus editor.
    pub focus: Option<Focus>,
    /// Wave drawing tool.
    pub wave_tool: WaveTool,
    /// UI scale factor relative to the 900 × 560 design size (0.75..=2.0).
    pub scale: f32,
    pub lang: Lang,
    /// Selected macro for ASSIGN mode (0..4).
    pub macro_selected: u8,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            channel: Channel::Wave,
            focus: None,
            wave_tool: WaveTool::Draw,
            scale: 1.0,
            lang: Lang::Ru,
            macro_selected: 1,
        }
    }
}

/// Logical (unscaled) editor size, from the design.
pub const EDITOR_WIDTH: f32 = 900.0;
/// Logical (unscaled) editor height, from the design.
pub const EDITOR_HEIGHT: f32 = 560.0;
/// Allowed UI scales offered in the settings menu.
pub const UI_SCALES: [f32; 6] = [0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

/// GUI ↔ plugin contract. See the module docs.
pub trait Bridge: Send + Sync {
    // ----- Parameters (plain values, see `pixl_core::params`) -----

    /// Current base (unmodulated) plain value.
    fn param(&self, id: ParamId) -> f32;
    /// Value after macro modulation (what the DSP actually uses). Used to draw modulation
    /// indicators; equals `param` when unmodulated.
    fn param_modulated(&self, id: ParamId) -> f32;
    /// Start a gesture (mouse down on a control). Must be paired with `end_edit`.
    fn begin_edit(&self, id: ParamId);
    /// Set a plain value during a gesture. Implementations sanitize the value.
    fn set_param(&self, id: ParamId, plain: f32);
    /// End a gesture.
    fn end_edit(&self, id: ParamId);
    /// One-shot change (click on a chip/toggle): begin + set + end.
    fn set_param_once(&self, id: ParamId, plain: f32) {
        self.begin_edit(id);
        self.set_param(id, plain);
        self.end_edit(id);
    }
    /// Reset to the default value (double-click / Option-click).
    fn reset_param(&self, id: ParamId) {
        self.set_param_once(id, id.default_value());
    }

    // ----- Wave RAM -----

    fn wave(&self) -> WaveRam;
    /// Replace the wave RAM (takes effect on the audio thread without locks).
    fn set_wave(&self, wave: WaveRam);

    // ----- Macros -----

    fn macro_assignments(&self) -> MacroAssignments;
    /// Create/update/remove (depth ≈ 0) a routing. Returns false if rejected.
    fn set_macro_assignment(&self, macro_index: usize, target: ParamId, depth: f32) -> bool;

    // ----- Presets -----

    /// Factory + user presets, factory first in number order, then user presets by name.
    fn preset_list(&self) -> Vec<PresetInfo>;
    fn current_preset(&self) -> CurrentPreset;
    /// Load a preset (all parameters, wave RAM, macros). Notifies the host.
    fn load_preset(&self, id: &str) -> Result<(), String>;
    /// Save the current state as a user preset (overwrites a user preset of the same name).
    fn save_user_preset(&self, name: &str, category: Category) -> Result<PresetId, String>;
    fn delete_user_preset(&self, id: &str) -> Result<(), String>;
    fn set_favorite(&self, id: &str, favorite: bool);

    // ----- Live data -----

    fn telemetry(&self) -> TelemetrySnapshot;

    // ----- Editor state -----

    fn ui_state(&self) -> UiState;
    /// Changes after host recall, including Compare/Paste of the same preset.
    fn state_generation(&self) -> u64 { 0 }
    fn set_ui_state(&self, state: UiState);
    /// Ask the host to resize the editor to `scale` × design size. Returns whether the host
    /// accepted.
    fn request_resize(&self, scale: f32) -> bool;
}
