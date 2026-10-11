//! PIXL shared definitions.
//!
//! This crate is the single source of truth that every other PIXL crate builds on:
//!
//! - [`params`]: the complete parameter table (stable string keys, ranges, defaults,
//!   units, host display formatting) and [`ParamValues`], a flat array of plain values
//!   indexed by [`ParamId`] that the DSP crates read every block.
//! - [`enums`]: typed views of the enum parameters (chip mode, voice mode, ...).
//! - [`macros`]: the four assignable macros and their modulation semantics.
//! - [`wave`]: the 32 × 4-bit wave RAM type and its shape generators.
//! - [`preset`]: the preset / factory library file format.
//! - [`bridge`]: the contract between the GUI and the plugin (`Bridge` trait), plus the
//!   telemetry and UI-state types that cross it.
//! - [`sync`]: small lock-free containers used to move state between the audio thread
//!   and the GUI without locks or allocations.
//!
//! Nothing here allocates on the audio thread or depends on a plugin framework.

pub mod bridge;
pub mod enums;
pub mod macros;
pub mod params;
pub mod preset;
pub mod sync;
pub mod wave;

pub use bridge::{
    Bridge, CurrentPreset, Focus, HeroView, Lang, PresetId, PresetInfo, TelemetrySnapshot, UiState, WaveTool,
};
pub use enums::*;
pub use macros::{apply_macros, MacroAssignments, MacroSlot, MACRO_COUNT, MACRO_SLOTS};
pub use params::{ChannelField, ParamId, ParamKind, ParamSpec, ParamValues, Unit, PARAMS, PARAM_COUNT};
pub use preset::{Category, LocalizedText, Preset, PresetIcon, PresetLibrary};
pub use wave::{WaveRam, WAVE_LEN, WAVE_MAX};

/// Product name as shown to users.
pub const PRODUCT_NAME: &str = "PIXL";
/// Vendor name as shown to users.
pub const VENDOR_NAME: &str = "PIXL Audio";
/// Version of this build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
