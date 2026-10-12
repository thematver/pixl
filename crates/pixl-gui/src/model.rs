//! A per-frame snapshot of everything the editor displays, read from the [`Bridge`].
//!
//! The editor never caches sound state of its own: every frame (and every
//! [`App::needs_redraw`](pixl_gfx::App::needs_redraw) poll) reads a fresh [`Model`] and compares
//! it with the one last drawn, so host automation, preset loads and macro modulation show up
//! without any notification plumbing.

use pixl_core::{
    Bridge, Channel, ChannelField, ChipMode, CurrentPreset, MacroAssignments, ParamId, ParamValues,
    TelemetrySnapshot, WaveRam, MACRO_COUNT, MACRO_SLOTS, PARAM_COUNT,
};

/// Snapshot of the plugin state shown by the editor.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    /// Base (unmodulated) plain values.
    pub base: ParamValues,
    /// Effective values after macro modulation (equal to `base` for untargeted parameters).
    pub live: ParamValues,
    /// Wave RAM.
    pub wave: WaveRam,
    /// Macro routings.
    pub macros: MacroAssignments,
    /// Header preset.
    pub preset: CurrentPreset,
    /// Live audio-thread data.
    pub telemetry: TelemetrySnapshot,
}

impl Model {
    /// Reads a snapshot. Only macro targets are queried for modulated values.
    pub fn read(bridge: &dyn Bridge) -> Self {
        let mut base = ParamValues::defaults();
        for (i, id) in ParamId::ALL.iter().enumerate().take(PARAM_COUNT) {
            base.0[i] = id.sanitize(bridge.param(*id));
        }
        let macros = bridge.macro_assignments();
        let mut live = base;
        for slots in macros.slots.iter().take(MACRO_COUNT) {
            for slot in slots.iter().take(MACRO_SLOTS) {
                if let Some(t) = slot.target {
                    live.0[t as usize] = t.sanitize(bridge.param_modulated(t));
                }
            }
        }
        Self {
            base,
            live,
            wave: bridge.wave(),
            macros,
            preset: bridge.current_preset(),
            telemetry: bridge.telemetry(),
        }
    }

    /// Whether anything except the telemetry differs from `other`.
    pub fn state_differs(&self, other: &Model) -> bool {
        self.base != other.base
            || self.live != other.live
            || self.wave != other.wave
            || self.macros != other.macros
            || self.preset != other.preset
    }

    /// Base value.
    pub fn get(&self, id: ParamId) -> f32 {
        self.base.get(id)
    }

    /// Effective (modulated) value.
    pub fn live(&self, id: ParamId) -> f32 {
        self.live.get(id)
    }

    /// Bool parameter (base).
    pub fn on(&self, id: ParamId) -> bool {
        self.base.bool(id)
    }

    /// Chip mode.
    pub fn chip(&self) -> ChipMode {
        self.base.chip()
    }

    /// Channel field (base).
    pub fn channel(&self, ch: Channel, field: ChannelField) -> f32 {
        self.base.channel(ch, field)
    }

    /// Whether a macro moves `id` right now (its live value differs from the base).
    pub fn is_modulated(&self, id: ParamId) -> bool {
        (self.live.get(id) - self.base.get(id)).abs() > 1e-4
    }

    /// Whether the wave channel is the fixed NES triangle.
    pub fn is_nes_triangle(&self) -> bool {
        self.chip() == ChipMode::Nes
    }

    /// Chip frame rate in Hz (`None` in FREE mode, which is unstepped).
    pub fn frame_hz(&self) -> Option<f64> {
        match self.chip() {
            ChipMode::Free => None,
            ChipMode::Gb => Some(59.7275),
            ChipMode::Nes => Some(self.base.frame_rate().hz()),
        }
    }

    /// Duration of one chip frame in ms (`None` in FREE mode).
    pub fn frame_ms(&self) -> Option<f64> {
        self.frame_hz().map(|hz| 1000.0 / hz)
    }
}
