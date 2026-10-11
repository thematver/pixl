//! PIXL — chiptune synthesizer plugin (nih-plug glue, SPEC §8).
//!
//! This crate ties the PIXL crates together into a CLAP plugin; VST3 and AUv2 are assembled
//! from the CLAP by clap-wrapper (see `wrapper/`).
//!
//! - [`params`]: host parameters generated from the `pixl_core` table (single source of
//!   truth), canonical host text;
//! - [`state`]: persistent fields (wave, macros, preset metadata, UI), state filtering and
//!   the "modified" logic;
//! - [`midi`]: note/MIDI event mapping;
//! - [`processor`]: the framework-free audio processor (engine + effects, sample-accurate
//!   block splitting, macro modulation, telemetry);
//! - [`presets`]: factory library, user presets and favourites;
//! - [`bridge`]: the [`pixl_core::Bridge`] implementation used by the editor;
//! - [`editor`]: the nih-plug editor embedding the native PIXL GUI (cargo feature `gui`,
//!   on by default; without it hosts show their generic parameter UI);
//! - `entry`: the exported `clap_entry` (nih-plug's, with a hardened state loader).
//!
//! Identity: CLAP id `audio.pixl.pixl`, features instrument/synthesizer/stereo, vendor
//! "PIXL Audio", version from Cargo; AU `aumu`/`PIXL`/`PXLa` (set in `wrapper/`).

pub mod bridge;
pub mod editor;
mod entry;
pub mod midi;
pub mod params;
pub mod presets;
pub mod processor;
pub mod state;

pub use entry::{clap_entry, MAX_STATE_BYTES};

use crate::midi::{map_note_event, EventStream};
use crate::params::PixlParams;
use crate::presets::PresetStore;
use crate::processor::{BlockInput, BlockStatus, Processor};
use nih_plug::prelude::*;
use pixl_core::ParamValues;
use pixl_dsp::Transport as EngineTransport;
use std::sync::Arc;

/// One PIXL plugin instance.
pub struct Pixl {
    params: Arc<PixlParams>,
    store: Arc<PresetStore>,
    processor: Processor,
    /// Base parameter values of the current block (scratch, no allocation).
    base: ParamValues,
    /// Render buffers for hosts that hand us fewer than two output channels (never the case
    /// with our only layout, handled anyway). Sized in `initialize`.
    scratch: [Vec<f32>; 2],
}

impl Default for Pixl {
    fn default() -> Self {
        // Start the GPU context in the background so the first editor opens instantly.
        #[cfg(feature = "gui")]
        pixl_gui::prewarm();
        // Parse the factory library and start the liveness clock off the audio thread.
        let _ = presets::factory_library();
        let _ = state::epoch();
        Self {
            params: Arc::new(PixlParams::new()),
            store: Arc::new(PresetStore::system()),
            processor: Processor::new(),
            base: ParamValues::defaults(),
            scratch: [Vec::new(), Vec::new()],
        }
    }
}

impl Pixl {
    /// The instance's parameters and shared state.
    pub fn pixl_params(&self) -> &Arc<PixlParams> {
        &self.params
    }

    /// The instance's preset store.
    pub fn preset_store(&self) -> &Arc<PresetStore> {
        &self.store
    }
}

/// Host transport → engine transport (120 BPM when the host reports no tempo). Further
/// sanitizing happens in [`processor::sanitize_transport`].
fn engine_transport(tempo: Option<f64>, playing: bool, beat_pos: Option<f64>) -> EngineTransport {
    EngineTransport { tempo_bpm: tempo.unwrap_or(120.0), playing, beat_pos }
}

impl Plugin for Pixl {
    const NAME: &'static str = pixl_core::PRODUCT_NAME;
    const VENDOR: &'static str = pixl_core::VENDOR_NAME;
    // No public web presence or support address has been decided yet; these stay empty
    // rather than pointing at a domain the vendor may not own.
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: None,
        main_output_channels: NonZeroU32::new(2),
        aux_input_ports: &[],
        aux_output_ports: &[],
        names: PortNames {
            layout: Some("Stereo"),
            main_input: None,
            main_output: Some("Output"),
            aux_inputs: &[],
            aux_outputs: &[],
        },
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const MIDI_OUTPUT: MidiConfig = MidiConfig::None;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    #[cfg(feature = "gui")]
    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        Some(Box::new(editor::PixlEditor::new(self.params.clone(), self.store.clone())))
    }

    fn filter_state(state: &mut PluginState) {
        state::filter_state(state);
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        // nih-plug also calls this after every state load (possibly on the audio thread):
        // nothing below allocates unless the configuration actually changed.
        let sr = buffer_config.sample_rate;
        self.processor.prepare(sr);
        if !self.processor.is_rate_supported() {
            nih_warn!("PIXL cannot run at {sr} Hz (8-768 kHz); the output stays silent");
        } else if !(processor::SPEC_MIN_SAMPLE_RATE..=processor::SPEC_MAX_SAMPLE_RATE).contains(&sr) {
            nih_log!("PIXL runs at {sr} Hz, outside the 22.05-192 kHz range it is specified for");
        }
        let max = buffer_config.max_buffer_size as usize;
        for s in &mut self.scratch {
            if s.len() < max {
                s.resize(max, 0.0);
            }
        }
        self.params.shared.runtime.set_active(true);
        true
    }

    fn reset(&mut self) {
        self.processor.reset();
    }

    fn deactivate(&mut self) {
        self.params.shared.runtime.set_active(false);
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let shared = &self.params.shared;
        shared.runtime.mark_processed();
        // With sample-accurate automation nih-plug splits the host block at every parameter
        // change, so the values are constant for this call.
        self.params.read_values(&mut self.base);
        let transport = {
            let t = context.transport();
            engine_transport(t.tempo, t.playing, t.pos_beats())
        };
        let input = BlockInput { params: &self.base, wave: &shared.wave, macros: &shared.macros, transport };
        let events = EventStream::new(|| context.next_event().map(|e| map_note_event(&e)));

        let samples = buffer.samples();
        let status = match buffer.as_slice() {
            [left, right, rest @ ..] => {
                let status = self.processor.process(&input, events, left, right);
                for ch in rest {
                    ch.fill(0.0);
                }
                status
            }
            other => {
                // Fewer than two channels: render stereo into scratch and downmix.
                let n = samples.min(self.scratch[0].len()).min(self.scratch[1].len());
                let [l, r] = &mut self.scratch;
                let status = self.processor.process(&input, events, &mut l[..n], &mut r[..n]);
                for ch in other.iter_mut() {
                    for (i, x) in ch.iter_mut().enumerate() {
                        *x = if i < n { 0.5 * (l[i] + r[i]) } else { 0.0 };
                    }
                }
                status
            }
        };

        shared.telemetry.publish(self.processor.telemetry());
        match status {
            BlockStatus::Active => ProcessStatus::KeepAlive,
            BlockStatus::Tail(n) => ProcessStatus::Tail(n),
            BlockStatus::Silent => ProcessStatus::Normal,
        }
    }
}

impl ClapPlugin for Pixl {
    const CLAP_ID: &'static str = "audio.pixl.pixl";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Chiptune synthesizer: NES, Game Boy and FREE modes");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] =
        &[ClapFeature::Instrument, ClapFeature::Synthesizer, ClapFeature::Stereo];

    fn remote_controls(&self, context: &mut impl RemoteControlsContext) {
        self.params.remote_controls(context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity() {
        assert_eq!(Pixl::NAME, "PIXL");
        assert_eq!(Pixl::VENDOR, "PIXL Audio");
        assert_eq!(Pixl::VERSION, pixl_core::VERSION);
        assert_eq!(Pixl::CLAP_ID, "audio.pixl.pixl");
        assert_eq!(Pixl::CLAP_FEATURES, &[ClapFeature::Instrument, ClapFeature::Synthesizer, ClapFeature::Stereo]);
        const { assert!(Pixl::SAMPLE_ACCURATE_AUTOMATION) };
        assert_eq!(Pixl::MIDI_INPUT, MidiConfig::MidiCCs);
        assert_eq!(Pixl::MIDI_OUTPUT, MidiConfig::None);
        assert_eq!(Pixl::AUDIO_IO_LAYOUTS.len(), 1);
        assert_eq!(Pixl::AUDIO_IO_LAYOUTS[0].main_output_channels, NonZeroU32::new(2));
        assert_eq!(Pixl::AUDIO_IO_LAYOUTS[0].main_input_channels, None);
    }

    #[test]
    fn transport_conversion() {
        assert_eq!(engine_transport(None, false, None).tempo_bpm, 120.0);
        let e = engine_transport(Some(140.0), true, Some(3.5));
        assert_eq!(e.tempo_bpm, 140.0);
        assert!(e.playing);
        assert_eq!(e.beat_pos, Some(3.5));
    }

    #[test]
    fn fresh_instance_is_the_init_preset() {
        let plugin = Pixl::default();
        let params = plugin.pixl_params();
        assert_eq!(params.base_values(), ParamValues::defaults());
        let header = state::current_preset(params);
        assert_eq!((header.name.as_str(), header.number, header.modified), ("Hyrule Fanfare", 14, false));
        assert_eq!(params.shared.wave.load(), pixl_core::wave::default_wave());
        assert_eq!(params.shared.macros.load(), pixl_core::MacroAssignments::factory_default());
    }
}
