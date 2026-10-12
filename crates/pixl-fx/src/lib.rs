//! PIXL effect chain (SPEC §5).
//!
//! Signal order: **voices → crush → chip output filter → DC blocker → echo → output gain →
//! safety clipper → meters.**
//!
//! | stage | module | summary |
//! |---|---|---|
//! | bitcrusher | [`crusher`] + [`blep`] | mid-tread `2^bits` quantizer centred on the tracked signal level (sign quantizer below 2 bits), fractional sample-and-hold with minimum-phase band-limited steps |
//! | chip filter | [`filters`] | NES HPF 90 Hz → HPF 440 Hz → LPF 14 kHz; GB HPF 28 Hz; 10 ms crossfaded mode switches, gradual high-pass release into none |
//! | DC blocker | [`filters`] | first-order 5 Hz high-pass, always on |
//! | echo | [`echo`] + [`resampler`] | SNES SPC700-style 32 kHz echo, Q7 FIR presets, ping-pong width, sleeps when idle |
//! | output | [`output`] | smoothed gain, safety clipper (transparent below −1 dBFS, ceiling −0.01 dBFS) |
//! | meters | [`meter`] | peak (−40 dB/s), RMS (300 ms), 1.5 s peak hold |
//!
//! Real-time contract: [`FxChain::new`] and [`FxChain::prepare`] allocate (and build the
//! process-wide minBLEP and FIR tables on first use); every other method is allocation-,
//! lock- and I/O-free and never panics on any input (NaN/Inf samples are replaced by
//! silence, parameters are sanitized). Output samples are always finite and bounded by the
//! clipper ceiling. Processing is deterministic.
//!
//! Latency: none, except that the crusher's wet signal is one sample late while it is on
//! (its centred interpolator looks one sample ahead; the dry path and bypass are not delayed).
//! The echo compensates its resampling latency internally, so repeats land exactly on the
//! echo time.
//!
//! Cost (measured, Apple M4 Max, everything on, echo at 95 % feedback): ≈ 0.2 % of one core
//! at 44.1/48 kHz, ≈ 0.8 % at 192 kHz. With the effects off only the filters, output stage
//! and meters run.

pub mod blep;
pub mod crusher;
pub mod echo;
pub mod filters;
pub mod meter;
pub mod output;
pub mod resampler;
pub mod util;

use crusher::Crusher;
use echo::Echo;
use filters::{ChipFilter, ChipFilterMode, DcBlocker};
use meter::Meter;
use output::OutputStage;
use pixl_core::{ChipMode, EchoDiv, EchoFir, ParamId, ParamValues, TelemetrySnapshot};

/// Sample rate used by [`FxChain::new`] until [`FxChain::prepare`] is called.
pub const DEFAULT_SAMPLE_RATE: f32 = 48_000.0;
/// Maximum block size used by [`FxChain::new`] until [`FxChain::prepare`] is called.
pub const DEFAULT_MAX_BLOCK: usize = 1024;
/// Lowest sample rate accepted by [`FxChain::prepare`] (lower values are clamped).
pub const MIN_SAMPLE_RATE: f32 = 8_000.0;
/// Highest sample rate accepted by [`FxChain::prepare`] (higher values are clamped).
pub const MAX_SAMPLE_RATE: f32 = 768_000.0;

/// Resolved effect parameters (sanitized plain values).
#[derive(Clone, Copy, Debug, PartialEq)]
struct FxParams {
    crush_on: bool,
    crush_bits: f32,
    crush_rate: f32,
    filter_mode: ChipFilterMode,
    echo_on: bool,
    echo_time_ms: f32,
    echo_sync: bool,
    echo_div: EchoDiv,
    echo_fb: f32,
    echo_mix: f32,
    echo_fir: EchoFir,
    echo_width: f32,
    out_gain_db: f32,
}

impl FxParams {
    fn from_values(p: &ParamValues) -> Self {
        let get = |id: ParamId| id.sanitize(p.get(id));
        let on = |id: ParamId| get(id) >= 0.5;
        let chip = ChipMode::from_plain(get(ParamId::Chip));
        let filter_mode = if on(ParamId::Analog) {
            match chip {
                ChipMode::Nes => ChipFilterMode::Nes,
                ChipMode::Gb => ChipFilterMode::Gb,
                ChipMode::Free => ChipFilterMode::None,
            }
        } else {
            ChipFilterMode::None
        };
        Self {
            crush_on: on(ParamId::CrushOn),
            crush_bits: get(ParamId::CrushBits),
            crush_rate: get(ParamId::CrushRate),
            filter_mode,
            echo_on: on(ParamId::EchoOn),
            echo_time_ms: get(ParamId::EchoTime),
            echo_sync: on(ParamId::EchoSync),
            echo_div: EchoDiv::from_plain(get(ParamId::EchoDiv)),
            echo_fb: get(ParamId::EchoFb) / 100.0,
            echo_mix: get(ParamId::EchoMix) / 100.0,
            echo_fir: EchoFir::from_plain(get(ParamId::EchoFir)),
            echo_width: get(ParamId::EchoWidth) / 100.0,
            out_gain_db: get(ParamId::OutGain),
        }
    }
}

/// voices → crush → analog/chip filter → echo → output gain → safety clipper → meters.
pub struct FxChain {
    sample_rate: f32,
    max_block: usize,
    params: FxParams,
    tempo_bpm: f64,
    crusher: Crusher,
    chip_filter: ChipFilter,
    dc: DcBlocker,
    echo: Echo,
    output: OutputStage,
    meter: Meter,
}

impl Default for FxChain {
    fn default() -> Self {
        Self::new()
    }
}

impl FxChain {
    /// Allocates; call off the audio thread. Configured for [`DEFAULT_SAMPLE_RATE`] and
    /// [`DEFAULT_MAX_BLOCK`] with the parameter-table defaults until `prepare`/`set_params`.
    pub fn new() -> Self {
        let fs = DEFAULT_SAMPLE_RATE;
        let mut chain = Self {
            sample_rate: fs,
            max_block: DEFAULT_MAX_BLOCK,
            params: FxParams::from_values(&ParamValues::defaults()),
            tempo_bpm: 120.0,
            crusher: Crusher::new(fs),
            chip_filter: ChipFilter::new(fs as f64),
            dc: DcBlocker::new(fs as f64),
            echo: Echo::new(fs, DEFAULT_MAX_BLOCK),
            output: OutputStage::new(fs),
            meter: Meter::new(fs),
        };
        chain.apply_params();
        chain.reset();
        chain
    }

    /// Configure for a sample rate and maximum block size. May allocate.
    ///
    /// Supported rates are 22.05–192 kHz; anything finite is accepted and clamped to
    /// [`MIN_SAMPLE_RATE`]..=[`MAX_SAMPLE_RATE`]. All state is cleared.
    pub fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        let fs = if sample_rate.is_finite() { sample_rate.clamp(MIN_SAMPLE_RATE, MAX_SAMPLE_RATE) } else { 48_000.0 };
        let max_block = max_block.clamp(1, 1 << 16);
        if fs != self.sample_rate || max_block != self.max_block || self.echo.host_rate() != fs.round() as u32 {
            self.echo = Echo::new(fs, max_block);
        }
        self.sample_rate = fs;
        self.max_block = max_block;
        self.crusher.set_sample_rate(fs);
        self.chip_filter.set_sample_rate(fs as f64);
        self.dc.set_sample_rate(fs as f64);
        self.output.set_sample_rate(fs);
        self.meter.set_sample_rate(fs);
        self.apply_params();
        self.reset();
    }

    /// Clear delay lines and filter state. Real-time safe.
    ///
    /// Smoothed parameters jump to their targets; meters read silence.
    pub fn reset(&mut self) {
        self.crusher.reset();
        self.chip_filter.reset();
        self.dc.reset();
        self.echo.reset();
        self.output.reset();
        self.meter.reset();
    }

    /// Update parameters (effective, macro-applied plain values). Real-time safe.
    pub fn set_params(&mut self, params: &ParamValues) {
        self.params = FxParams::from_values(params);
        self.apply_params();
    }

    /// Host tempo for synced echo. Real-time safe. Non-finite or non-positive values are
    /// ignored (the last valid tempo is kept).
    pub fn set_tempo(&mut self, bpm: f64) {
        if bpm.is_finite() && bpm > 0.0 {
            let bpm = bpm.clamp(1.0, 2000.0);
            if bpm != self.tempo_bpm {
                self.tempo_bpm = bpm;
                self.apply_echo();
            }
        }
    }

    fn apply_params(&mut self) {
        let p = self.params;
        self.crusher.set_params(p.crush_on, p.crush_bits, p.crush_rate);
        self.chip_filter.set_mode(p.filter_mode);
        self.output.set_gain_db(p.out_gain_db);
        self.apply_echo();
    }

    fn apply_echo(&mut self) {
        let p = self.params;
        let t = echo::echo_time_s(p.echo_sync, p.echo_div, self.tempo_bpm, p.echo_time_ms);
        self.echo.set_params(p.echo_on, t, p.echo_fb, p.echo_mix, p.echo_fir, p.echo_width);
    }

    /// Process in place (`l.len() == r.len()` ≤ max_block). Real-time safe.
    ///
    /// Longer blocks are processed in `max_block` chunks; if the lengths differ only the
    /// common prefix is processed.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(r.len());
        let mut start = 0;
        while start < n {
            let len = (n - start).min(self.max_block);
            let (cl, cr) = (&mut l[start..start + len], &mut r[start..start + len]);
            for x in cl.iter_mut().chain(cr.iter_mut()) {
                *x = util::sanitize_sample(*x);
            }
            self.crusher.process(cl, cr);
            self.chip_filter.process(cl, cr);
            self.dc.process(cl, cr);
            self.echo.process(cl, cr);
            self.output.process(cl, cr);
            self.meter.process(cl, cr);
            start += len;
        }
    }

    /// Fill the meter fields of the telemetry (`peak_db`, `rms_db`, `peak_hold_db`).
    pub fn fill_telemetry(&self, t: &mut TelemetrySnapshot) {
        t.peak_db = self.meter.peak_db();
        t.rms_db = self.meter.rms_db();
        t.peak_hold_db = self.meter.peak_hold_db();
    }

    /// Current tail length (echo decay) in samples, for host tail reporting.
    ///
    /// Estimated from the level currently circulating in the echo and the feedback amount
    /// (time until it falls below one 16-bit LSB), plus the resampler latency; 0 when the
    /// echo is off and silent.
    pub fn tail_samples(&self) -> u32 {
        self.echo.tail_samples().min(u32::MAX as u64) as u32
    }

    /// Sample rate the chain is configured for.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Maximum block size the chain is configured for.
    pub fn max_block(&self) -> usize {
        self.max_block
    }

    /// Whether the echo is currently processing (on, or its tail is still decaying).
    pub fn echo_active(&self) -> bool {
        self.echo.is_active()
    }

    /// Whether the crusher is currently processing (on, or crossfading out).
    pub fn crusher_active(&self) -> bool {
        self.crusher.is_active()
    }
}
