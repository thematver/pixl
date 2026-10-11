# PIXL — Product & Engineering Specification

PIXL is a premium chiptune / 8-bit synthesizer plugin (NES, Game Boy, and an unrestricted
"FREE" mode) written in Rust. Target host: **Logic Pro on macOS (Audio Unit v2)**; VST3 and
CLAP are built from the same code. Price class: high-end commercial (~$3000 positioning), so
every detail — sound accuracy, aliasing, CPU, UI polish, state robustness — must hold up to
professional scrutiny.

This document is the contract between all crates and contributors. Where it says MUST, it is
an acceptance criterion.

---

## 0. Quality bar (applies to everything)

- **Real-time safety.** Nothing called from the audio thread allocates, locks, blocks, logs,
  or performs I/O. All buffers are allocated in `new()`/`prepare()`. Use atomics/lock-free
  containers (`pixl_core::sync`) for cross-thread state.
- **No panics in production paths.** No `unwrap()`/`expect()` on fallible runtime data, no
  unchecked indexing that can go out of bounds, no integer overflow on long sessions (use
  wrapping/u64 counters where relevant). Every DSP output is finite (no NaN/Inf ever escapes;
  sanitize inputs, guard divisions).
- **Denormals.** nih-plug enables flush-to-zero during `process`; DSP must still not rely on
  denormal-producing feedback paths being cheap.
- **Determinism.** Same input + same state → bit-identical output (seeded PRNGs only).
- **Tests.** Each crate has meaningful unit tests; `cargo test --workspace --release` passes.
  `cargo clippy --workspace --all-targets -- -D warnings` is clean. Code is `rustfmt`-ed
  (max_width 120, see `rustfmt.toml`).
- **Docs.** Public items have doc comments explaining intent and units. Code comments are in
  English. User-facing strings are Russian (default, as in the design) and English.
- **Never** change another crate's public API without updating all its users and this spec.

---

## 1. Repository layout

```
Cargo.toml                workspace
Makefile                  `make` = build Release + install AU/VST3/CLAP (replace) + refresh AU cache
wrapper/                  CMake: clap-wrapper v0.16 "clap-first" → PIXL.component / .vst3 / .clap
crates/
  pixl-core/   (DONE)     parameter table, enums, macros, wave RAM, presets format, Bridge trait, lock-free sync
  pixl-dsp/               synthesis engine (voices) — §4
  pixl-fx/                effect chain (crush, chip output filters, SNES echo, output stage, meters) — §5
  pixl-gfx/               native rendering platform: NSView + wgpu(Metal) + vello + parley text — §6
  pixl-gui/               the PIXL editor UI built on pixl-gfx — §7
  pixl-plugin/            nih-plug glue: params, state, MIDI, process loop, presets store, editor — §8
presets/factory.json      the 128-preset factory library — §9
docs/design/source/       the Claude Design export (PIXL.dc.html = the UI spec)
docs/design/ref/*.png     reference renders of the 11 design screens (3600×2240 = 900×560 @4x)
tools/design-ref/         script that regenerates the reference renders
```

Crate graph: `pixl-core` ← `pixl-dsp`, `pixl-fx`, `pixl-gui`; `pixl-gfx` ← `pixl-gui`;
`pixl-plugin` depends on all. `pixl-dsp`, `pixl-fx`, `pixl-core` have **no** plugin-framework
or platform dependencies (pure, testable Rust). `pixl-gui` does not depend on nih-plug.

Build: `make` (see Makefile). The plugin crate builds a `staticlib` (`libpixl.a`) that
exports nih-plug's `clap_entry`; CMake links it into all three formats. AU identity:
type `aumu`, subtype `PIXL`, manufacturer `PXLa`. Validate with `make validate` (auval).

---

## 2. Parameters, state and presets (`pixl-core`, implemented)

Read `crates/pixl-core/src/*.rs` — it is the source of truth. Summary:

- `PARAMS` / `ParamId`: 91 parameters with stable string keys, ranges, defaults, units,
  host formatting/parsing, `modulatable` flag. Plain values are `f32` (bool 0/1, enum index,
  int as float). Normalization matches nih-plug `FloatRange::Skewed` exactly.
- `ParamValues`: `[f32; PARAM_COUNT]` of plain values with typed accessors (`chip()`, …).
  DSP crates consume **effective** values (after macro modulation) through this type.
- Macros: 4 macros (`macro1..4` params, 0..100 %), each with up to 8 routings of bipolar
  depth (normalized domain). `apply_macros()` computes effective values.
  `MacroAssignments::factory_default()` = routings of a fresh instance.
- Wave RAM: `[u8; 32]`, values 0..15. `wave::default_wave()` and shape generators.
- Presets: JSON format (`Preset`, `PresetLibrary`), lenient parsing with warnings.
- `Bridge` trait: everything the GUI may ask of the plugin. `TelemetrySnapshot`, `UiState`.
- `sync`: `AtomicWave`, `AtomicMacros`, `AtomicTelemetry`, `DirtyFlag`.

The defaults of the parameter table are the design's initial state (GB chip, wave channel
on, sweep −12 st/180 ms, vibrato 5.5 Hz/18 c/120 ms, crush 6 bit/11 kHz off, glide 90 ms
legato, unison 3 voices/14 c/60 %). A fresh instance = parameter defaults + `default_wave()`
+ `MacroAssignments::factory_default()`; this is exactly factory preset **014
"Hyrule Fanfare" (LEAD)**, which the header shows on a fresh instance.

---

## 3. Sound model overview

Each note produces a **voice**. A voice owns up to 4 **unison sub-voices**; each sub-voice
runs the 4 **chip channels**: Pulse 1, Pulse 2, Wave, Noise. Channels are enabled per patch
(`p1_on`, …) and mixed according to the chip mode. Effects tiles (sweep, vibrato, arp, glide,
unison) act on the voice's pitch/voicing; crush and echo are global post effects.

Chip modes (`chip` param):

| | NES (Ricoh 2A03) | GB (DMG) | FREE |
|---|---|---|---|
| Pulse duty | 12.5/25/50/75 % (nearest) | 12.5/25/50/75 % | continuous 2–98 % (PWM) |
| Wave channel | **fixed triangle** (32 steps, 4-bit, no volume) | 32×4-bit wave RAM, 2-bit volume shift | wave RAM, continuous level |
| Noise | 15-bit LFSR, long/short(93-step), 16-period table | 15/7-bit LFSR, divisor/shift table | LFSR, continuous rate |
| Levels | 4-bit (0..15) | 4-bit (wave: 0/25/50/100 %) | continuous |
| Pitch | 11-bit timer quantization (real detuning) | 11-bit frequency register | exact |
| Modulation update | stepped at frame rate (60.0988 / 50.007 Hz) | stepped at 59.7275 Hz | smooth (≥ 2.7 kHz) |
| Mixer | non-linear 2A03 mixer | linear sum | linear sum |
| Panning | continuous (mixer) | hard L/C/R (NR51) | continuous |

The orange stepped line in the UI always shows what the chip will actually play; the grey
dashed line shows the ideal curve (design principle).

---

## 4. `pixl-dsp` — synthesis engine

### 4.1 Public API (frozen; stub exists in `crates/pixl-dsp/src/lib.rs`)

```rust
pub struct Transport { pub tempo_bpm: f64, pub playing: bool, pub beat_pos: Option<f64> }
pub enum EngineEvent { NoteOn{note,velocity}, NoteOff{note}, PitchBend{value}, ModWheel{value},
                       Aftertouch{value}, Sustain{on}, AllNotesOff, AllSoundOff }
impl Engine {
  pub fn new(max_block: usize) -> Self;              // allocates
  pub fn prepare(&mut self, sample_rate: f32, max_block: usize);   // may allocate
  pub fn reset(&mut self);
  pub fn set_params(&mut self, params: &ParamValues);   // effective values; every sub-block
  pub fn set_wave(&mut self, wave: &WaveRam);
  pub fn set_transport(&mut self, transport: Transport);
  pub fn event(&mut self, event: EngineEvent);          // at current position
  pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]);  // overwrite, len ≤ max_block
  pub fn fill_telemetry(&self, t: &mut TelemetrySnapshot);         // voice fields only
  pub fn is_silent(&self) -> bool;
}
```

The caller (plugin) splits blocks at event times and every ≤ 64 samples, calling
`set_params` before each `render`. Supported sample rates: 22.05–192 kHz.

### 4.2 Band-limited step synthesis (the core of the sound quality)

All chip channels produce piecewise-constant signals. PIXL renders them **alias-free** with
band-limited step (BLEP) synthesis, exactly like the best hardware emulators (blargg's
Blip_Buffer approach), but with a **minimum-phase** kernel so there is zero latency:

- A `BlipBuffer` per output channel (L, R). Every amplitude change of any channel at time
  `t` (fractional samples within the block) adds `delta × kernel[phase]` into the impulse
  buffer; the output is the running integral. Pan gains are applied per delta (L/R).
- Kernel: minimum-phase band-limited impulse, cutoff ≈ 0.45·fs, ≥ 32 taps, ≥ 64 phases with
  linear interpolation between phases (or ≥ 512 phases). Each phase normalized to sum
  exactly 1.0 so steps settle with no drift. Computed once (process-wide `OnceLock`, or at
  build time) via the cepstral method; computing it must never happen on the audio thread.
- Integrator: f64 accumulator with a tiny leak (≈ 1 Hz) to bound numeric drift; buffers carry
  the kernel tail into the next block.
- Very high transition rates (noise at NES period 4 = 447 kHz clock): when a channel would
  emit more than ~4 transitions per output sample, switch that channel to
  integrate-and-dump (average level over the sample interval, emitted as one delta at the
  sample boundary). This bounds CPU while keeping aliasing low.
- Acceptance: a FREE-mode 50 % pulse at 4.186 kHz (C8) rendered at 44.1 kHz has all
  non-harmonic (aliased) spectral components ≤ −90 dB relative to the fundamental
  (Blackman-Harris FFT, harmonics masked). Same test for the wave channel and NES pulse.

### 4.3 Chip models

**Pitch → register quantization (chip modes).** For each pitch update compute the target
frequency `f`, then:

- NES (CPU clock 1 789 773 Hz NTSC, 1 662 607 Hz PAL — chosen by `frame_rate`):
  pulse `t = round(CPU/(16 f) − 1)`, clamp 0..2047, actual `f = CPU/(16(t+1))`; `t < 8` →
  channel muted (hardware sweep-unit behaviour). Triangle `t = round(CPU/(32 f) − 1)`,
  `t < 2` → hold current step (ultrasonic). Notes below the 11-bit range clamp to `t = 2047`
  (the lowest note the chip can play — authentic).
- GB (4 194 304 Hz): pulse `x = round(2048 − 131072/f)`, clamp 0..2047, `f = 131072/(2048−x)`;
  wave `x = round(2048 − 65536/f)`, `f = 65536/(2048−x)`.
- FREE: exact frequency, no quantization.

**Pulse.** 8-step duty sequences (output bit × level):
NES 12.5 % `01000000`, 25 % `01100000`, 50 % `01111000`, 75 % `10011111`;
GB 12.5 % `00000001`, 25 % `10000001`, 50 % `10000111`, 75 % `01111110`.
Chip modes snap `pN_duty` to the nearest of 12.5/25/50/75. FREE: continuous duty — two
transitions per cycle at exact fractional times (true PWM; smooth duty modulation by macros).

**Wave channel.** NES: the fixed sequence `pixl_core::wave::NES_TRIANGLE` (15…0,0…15), no
volume control: level, envelope shape and velocity are ignored — it is gated on while the
envelope is non-zero; when it stops, the sequencer halts on its current step (hardware
behaviour; the DC is removed downstream). GB: the 32×4-bit wave RAM; volume is a 2-bit
shift: effective level (level × env × velocity, 0..1) maps to 100 % (≥ 0.75), 50 % (≥ 0.375),
25 % (≥ 0.125), mute; samples are right-shifted (integer) like hardware. FREE: wave RAM with
continuous level.

**Noise.** 15-bit LFSR, initial state 0x7FFF. NES: feedback `bit0 ^ bit1` (long) or
`bit0 ^ bit6` (short), shift right, feedback into bit 14; output = level when `bit0 == 0`.
Clock periods (CPU cycles) NTSC `[4,8,16,32,64,96,128,160,202,254,380,508,762,1016,2034,4068]`,
PAL `[4,8,14,30,60,88,118,148,188,236,354,472,708,944,1890,3778]`. GB: `xor = bit0 ^ bit1`,
`lfsr = (lfsr >> 1) | (xor << 14)`, short mode also writes `xor` into bit 6; output = level when
`bit0 == 0`; clock `524288 / r / 2^(s+1)` (r = 0.5 for code 0) — build a 16-entry table of
(r, s) pairs with monotonically decreasing frequency from 262 144 Hz down to ≈ 1.5 kHz (≈
half-octave spacing). Index 0 = brightest. FREE: the GB-style LFSR at a continuous clock
rate interpolated over the same span (semitone resolution).
Period index = `ns_period`; with `ns_track` on, `index = ns_period − (note + coarse − 60)`
(one index per semitone; FREE: continuous, 1/12 octave per semitone); pitch modulation
(sweep, vibrato, glide, arp, bend) offsets the index the same way (in chip modes rounded at
frame ticks). Clamp 0..15.

**Levels and mixing.**
- Channel amplitude = `level/15 × env × vel_gain` where `vel_gain = 1 − s + s × velocity`
  (`s = vel_sens/100`). Chip modes quantize the product to an integer 0..15 (GB wave: shift
  as above; NES triangle: none).
- `analog` on + NES/GB: unipolar hardware DAC levels (0..15). Otherwise (FREE, or analog off)
  bipolar, centred levels (pulse ±, wave `(v − 7.5)/7.5`, noise ±) so no DC is produced.
- NES mixer (per sub-voice, applied to the 4 channel levels at each transition, events
  merged in time order): `pulse = 95.88 / (8128/(p1+p2) + 100)` (0 if both 0),
  `tnd = 159.79 / (1/(t/8227 + n/12241) + 100)` (0 if both 0). GB/FREE: linear sum.
- Pan: per channel `pan` (−100..100, equal-power). GB quantizes to L/C/R (|pan| > 33 → hard).
- Calibration: one channel at level 15 produces ≈ −6 dBFS peak after DC removal in every
  chip mode (±1.5 dB), so switching modes keeps loudness consistent. Unison sub-voices are
  scaled by `1/√n`. Polyphony is not auto-scaled (the output stage protects).

### 4.4 Frame clock and modulation timing

- Chip modes: a free-running global frame clock (NES NTSC 60.0988 Hz / PAL 50.007 Hz; GB
  59.7275 Hz) shared by all voices. At each frame tick (exact fractional sample position) every
  voice re-evaluates envelopes, pitch (→ register), duty and noise index, and the resulting
  changes are emitted as deltas at that exact time. On note-on, the initial values apply
  immediately (no waiting for the next tick); later updates happen on ticks.
- FREE: modulators update at least every 16 samples with per-update deltas; pitch changes
  keep phase continuity.

### 4.5 Envelopes

Per channel ADSR (`*_attack/decay/sustain/release`, sustain 0..15 → 0..1). Linear segments in
level. Release starts from the current level. Retrigger on new notes except legato
transitions. In chip modes the level is sampled at frame ticks and quantized (the stepped
look in the UI). A voice ends when all enabled channels' envelopes reach 0.

### 4.6 Pitch pipeline (per sub-voice)

`pitch = note + glide + arp + sweep + vibrato/100 + bend·pb_range + coarse + fine/100 + tune/100
+ unison_detune_offset` (semitones) → `f = 440·2^((pitch−69)/12)` → chip quantization.

- **Sweep** (`sweep_*`): on each envelope (re)trigger, offset starts at `+amt` (DOWN) or
  `−amt` (UP) and reaches 0 after `sweep_time`. EXP: `amt·(1 − t/T)²`; LIN: `amt·(1 − t/T)`.
  Chip modes step it at frame rate ("NES STEP").
- **Vibrato** (`vib_*`): depth (cents) with delay then linear fade-in from note-on; shapes
  sine/tri/square; phase reset at note-on. Mod wheel adds up to +50 c and aftertouch up to
  +50 c using the same rate/shape — active even when `vib_on` is off.
- **Glide** (`glide_*`): constant-time portamento in semitones. EXP: exponential approach
  reaching 99 % at `glide_time`; LIN: linear. POLY: a new voice glides from the previously
  played note when `ALWAYS`, or when another note is still held for `LEGATO`. MONO/LEGATO:
  between successive notes under the same rule.
- **Arp** (`arp_*`): pitch-only stepping (no envelope retrigger — the classic chip "fake
  chord"). `arp_chord = HELD`: the held notes form the sequence and play on a single voice
  (POLY collapses to one arp voice). Other chords: every played note arpeggiates its chord
  intervals. `arp_oct` repeats the sequence over octaves. Order UP / DOWN / UPDOWN (no
  repeated ends) / RANDOM (seeded per note-on) / ORDER (as played). Step length: synced
  (`arp_sync`): `arp_div` at host tempo, free-running from the first note-on; else `arp_rate`
  Hz, which chip modes quantize to an integer number of frames (≥ 1).
- **Pitch bend**: `pb_range` semitones; stepped at frame rate in chip modes.

### 4.7 Voices, unison, allocation

- `voice_mode` POLY (up to `polyphony` ≤ 16 voices) / MONO / LEGATO. Note priority in mono:
  last note; releasing returns to the most recent still-held note (MONO retriggers, LEGATO
  does not). Sustain pedal (CC64) defers note-offs.
- Stealing: prefer the quietest releasing voice, else the oldest. A stolen voice fades out
  over 2 ms (click-free) in all modes.
- Unison (`uni_on`, `uni_voices` 1..4, `uni_detune` c, `uni_width` %): sub-voice offsets are
  `detune × linspace(−1, 1, n)` cents and pan spread `width × linspace(−1, 1, n)` combined with
  channel pan. Sub-voice 0 starts at phase 0 on a fresh voice; others at seeded random
  phases. `uni_on` off ⇒ 1 sub-voice.
- Velocity 0 note-on is a note-off.

### 4.8 Parameter smoothing

Continuous parameters that would zipper (levels, pan, fine, tune, FREE duty, unison detune
and width) are smoothed (~10 ms) per sub-voice; chip modes still quantize at frame ticks.
Structural changes (chip mode, voice mode, polyphony, unison count) apply at the next note or
with a short crossfade; they MUST NOT click badly or glitch the engine.

### 4.9 Telemetry

`fill_telemetry` fills: `active_voices`, `held_notes`, `last_note`, `lfsr` (noise LFSR of
the most recent voice's first sub-voice), `wave_step`, `env_level[4]`, `pitch_offset_st`,
`arp_step`, `frame`.

### 4.10 Performance

48 kHz, 128-sample blocks, release build, Apple M1: default patch with 8 held notes < 2 % of
one core; worst case (16 voices × 4 unison × 4 channels, noise at the brightest period) <
25 %. Include a benchmark test that prints the numbers and asserts generous bounds.

### 4.11 Tests (minimum)

Kernel invariants; alias test (§4.2); NES/GB register quantization (A4 → NES pulse t = 253,
f ≈ 440.4 Hz); LFSR periods (long 32767; NES short 93 or 31); duty sequences; envelope
timing; frame stepping; poly/mono/legato allocation and stealing; glide timing; arp orders
and HELD collapse; sustain pedal; velocity; no NaN/Inf under random parameter fuzzing;
determinism; `examples/render_wav.rs` that renders demo phrases per chip mode to WAV.

---

## 5. `pixl-fx` — effect chain

### 5.1 Public API (frozen; stub in `crates/pixl-fx/src/lib.rs`)

`FxChain::{new, prepare(sr, max_block), reset, set_params(&ParamValues), set_tempo(bpm),
process(l, r), fill_telemetry(&mut TelemetrySnapshot) /* meters */, tail_samples()}`.

Order: **voices → crush → chip output filter → echo → output gain → safety clipper → meters.**
A DC blocker (5 Hz, first order) is always applied (after the chip filter).

### 5.2 Bitcrusher (`crush_on`, `crush_bits` 1..16, `crush_rate` 1..48 kHz)

Quantize to `2^bits` levels (fractional bits allowed, mid-tread, clamp ±1). Sample-and-hold at
`crush_rate` with a fractional phase accumulator; render the held steps band-limited (BLEP/
polyBLEP) so non-integer hold periods don't add jitter. `crush_rate ≥ fs` → quantize only.
Smooth rate changes (~20 ms) so macro sweeps are click-free. Off ⇒ bypass (no cost).

### 5.3 Chip output filter (`analog`)

NES: first-order HPF 90 Hz → HPF 440 Hz → LPF 14 kHz (NESdev APU mixer model). GB: first-order
HPF ≈ 28 Hz (DMG output capacitor). FREE or analog off: none (DC blocker only). Coefficients
recomputed on sample-rate change; switching modes must not click (crossfade ~10 ms).

### 5.4 Echo — SNES SPC700 style (`echo_*`)

- Runs internally at **32 kHz** like the SPC700: resample host → 32 kHz (polyphase windowed
  sinc, ≥ 32 taps, passband to 14 kHz, ≥ 80 dB stopband at 16 kHz), process, resample back.
- Stereo delay lines up to 2 s. Time = `echo_sync ? echo_div × 60/bpm : echo_time` (clamped to
  10 ms..2 s). Time changes are click-free (30 ms crossfade between read heads).
- 8-tap FIR on the echo output (coefficients in Q7, /128), presets FLAT/WARM/DARK/CAVE/BRIGHT/
  LOFI with documented responses; DC gain ≤ 1 for every preset. FIR output feeds back with
  `echo_fb` (0..95 %) through a soft clipper (stable at 95 % with any preset) and 16-bit
  quantization (authentic grit).
- `echo_width`: 0 = independent L/R echoes, 100 % = full ping-pong (input summed into one
  side, feedback crosses L↔R); interpolate.
- Output: `dry + wet × mix` (`echo_mix` 0..100 %). `echo_on` off ⇒ tail keeps decaying for
  natural cut (then the line is cleared/bypassed when silent).
- `tail_samples()` estimates remaining audible tail.

### 5.5 Output stage and meters

`out_gain` dB (smoothed 20 ms) → safety clipper: transparent below −1 dBFS, smooth
saturation above, ceiling −0.01 dBFS → meters: per-channel peak (instant attack, release
−40 dB/s), RMS (300 ms exponential), `peak_hold_db` = max sample peak of both channels held
1.5 s. Floor `METER_FLOOR_DB` (−100).

### 5.6 Tests

Resampler THD+N and passband ripple; echo stability (fb 95 %, all presets, 60 s noise input);
FIR DC gains; crusher level counts and S&H period; filter magnitude responses at key
frequencies; clipper transparency below −1 dBFS; meter ballistics; NaN/Inf fuzz.

---

## 6. `pixl-gfx` — native rendering platform (macOS)

A small, reusable foundation; no PIXL-specific UI.

- **View.** An `NSView` subclass (objc2 `define_class!`) added as a child of the host's view:
  layer-backed (`CAMetalLayer` via wgpu surface from raw-window-handle 0.6 AppKit handle),
  `isFlipped = YES`, `acceptsFirstMouse: = YES` (first click works in a background window),
  tracking area for hover/exit, mouse down/up/drag (left/right), double-click count, scroll
  (precise trackpad deltas + line deltas), modifier flags, `keyDown`/`keyUp`.
  Keyboard: when the UI has no focused text field, key events MUST be passed up the
  responder chain (so Space etc. reach Logic); when a text field is focused, the view
  becomes first responder and receives text (`insertText:` via `interpretKeyEvents:` so
  Cyrillic/IME input works).
- **Rendering.** wgpu 30 (Metal) + vello 0.11 (`render_to_texture` into an `Rgba8Unorm`
  storage texture, then blit to the surface). Retina: render at `backingScaleFactor`; react
  to `viewDidChangeBackingProperties` and resizes. Present mode: `Immediate`/`AutoNoVsync`
  where available (never block the host's main thread on vsync); pace frames with a 60 Hz
  timer on the main run loop in common modes (keeps animating during menu/drag tracking).
  Redraw only when something changed (input, animation, `App::needs_redraw`) — idle CPU ≈ 0.
- **Shared GPU context.** One `wgpu::Device` + `vello::Renderer` per process, created lazily
  and shared by all editor instances. `prewarm()` starts creating it on a background thread
  (called when a plugin instance is created) so opening the editor is instant. Opening must
  never block the main thread > 50 ms; until the GPU is ready the view shows the background
  colour `#F7F6F3` (no black/white flash). Handle surface loss/outdated/device loss by
  reconfiguring/recreating.
- **Text.** parley 0.12 with embedded fonts (`assets/fonts`: Geist 400/500/600/700, Geist
  Mono 400/500/600; OFL licence text shipped). API supports family (Sans/Mono), weight, size,
  colour, letter-spacing (em), alignment, max width + wrapping (preset descriptions), and
  measurement. Cache shaped layouts (keyed by text+style) — no reshaping every frame. Glyphs
  missing from Geist (e.g. ✎ ╱ ∿ ⇅ ★ ‹ ›) MUST NOT render as tofu: draw them as vector icons
  (provide an icon module) or verify coverage.
- **Drawing API** (`Canvas`): fill/stroke rect, rounded rect, circle, `BezPath` (with dash
  patterns, line joins), solid colours with alpha, clip push/pop, opacity layers, transforms,
  text, images not required. Coordinates are logical points (design px).
- **App trait** (the GUI implements it): `size()`, `event(&Event, &mut EventCtx)`,
  `frame(&mut Canvas, dt)`, `needs_redraw()`, `cursor()`. `EventCtx` can: request redraw,
  set cursor, begin/end "infinite drag" (hide cursor + relative deltas + restore position on
  release), focus/unfocus text input, show a native context menu (`NSMenu`, nested submenus,
  checkmarks, disabled items) and receive the selected item id.
- **Offscreen rendering** `render_to_png(app, scale, path)` for golden screenshots/tests
  (no window needed).
- **Standalone harness** example: opens an `NSWindow` hosting a given `App` (development).

## 7. `pixl-gui` — the PIXL editor

### 7.1 Public API (frozen; stub in `crates/pixl-gui/src/lib.rs`)

`unsafe fn open_editor(parent_nsview: *mut c_void, bridge: Arc<dyn Bridge>) -> Option<EditorHandle>`,
`fn prewarm()`, `const DESIGN_SIZE: (u32, u32) = (900, 560)`. `EditorHandle` drop detaches.
Also ships `MockBridge` (in-memory params, fake telemetry animation, sample preset list) and
examples: `harness` (window with MockBridge) and `snapshots` (renders every screen to
`target/gui-snapshots/*.png` at 2×, including the 11 design screens with the same states as
`tools/design-ref/snapshot.swift`).

### 7.2 Visual fidelity

`docs/design/source/PIXL.dc.html` is the pixel spec (900×560 logical): colours, fonts, sizes,
paddings, radii, SVG paths and the generators in `renderVals()`. Reproduce it exactly; compare
your snapshots side by side with `docs/design/ref/*.png`. Palette: bg `#F7F6F3`, canvas
`#E7E4DE`, ink `#161616`, accent `#FF5A1F`, muted `#6E6A63`, lines `#E2DFD8`, grid `#EFEDE8`,
chip bg `#F1EFEA`, segmented bg `#ECE9E3`, inactive `#C9C5BC`, list muted `#8A867E`,
dark-sub `#A9A49A`. Fonts: Geist and Geist Mono at the design's weights/sizes; mono labels use
letter-spacing .06em where the design does. Every value shown is **live** (bridge), every
control in the design is **functional**.

### 7.3 Layout (top → bottom) and behaviour

**Header (52 px).** Left: `PIXL` + orange square; clicking the logo opens the settings menu
(native NSMenu): UI size 75–200 %, language Русский/English, frame rate NTSC 60 Гц/PAL 50 Гц
(`frame_rate`), pitch-bend range ±1/2/3/5/7/12/24 (`pb_range`), velocity sensitivity
0/25/50/75/100 % (`vel_sens`), analog output ✓ (`analog`), separator, "PIXL v1.0.0 · PIXL
Audio" (disabled). Centre: preset pill `CAT / NUM  Name ‹ ›` (orange dot after the name
when modified); click → browser; ‹ › → previous/next preset in the current browser order.
Right: chip segmented NES/GB/FREE (`chip`); output meter (two bars = RMS L/R mapped −60..0 dB,
label = `peak_hold_db` as `−6.2 DB`, `−∞ DB` below −99); vertical drag on the meter changes
`out_gain` (tooltip `OUT −2.0 DB`), double-click resets.

**Channel tabs row.** Tabs PULSE 1 / PULSE 2 / WAVE (TRIANGLE in NES) / NOISE with icons as
designed. Each tab gets an enable dot (7 px; on = accent fill, off = 1.5 px `#C9C5BC` ring;
16 px hit area) toggling `pN_on`/`wv_on`/`ns_on`; disabled channels' labels are dimmed. To
the right: the **channel strip** for the selected channel — drag-value chips `VOL 12`,
`PAN C`, `ST +0` (coarse), `CT +0` (fine) in the hero-chip style (mono 10 px, label muted,
value ink, bg `#F1EFEA`, radius 5, padding 4×8), then a 1 px divider, then the chip name
(`RICOH 2A03 · 4 КАНАЛА` etc.). NES triangle: VOL chip disabled with tooltip
(«треугольник NES без громкости»).

**Hero display (858×248).** Channel views exactly as designed: WAVE (draw/line tools,
smooth/invert, SIN/TRI/SAW/SQR, step tooltip; plus a subtle accent band on the currently
playing step from telemetry), TRIANGLE (locked, NES), PULSE (duty chips, drag the edge —
continuous in FREE, snapping in NES/GB; harmonics bars), NOISE (LONG/SHORT, live LFSR register
from telemetry while playing, period bars clickable/draggable, plus a `TRACK` toggle chip).
Every channel view also has an `ENV` chip (rightmost, margin-left 10) opening the channel's
**envelope editor** (`Focus::Env`): back chip `← CHANNEL`, title `ENVELOPE · PULSE 1`, ideal
ADSR as grey dashed line, the stepped 16-level frame-rate version in orange (smooth orange line
in FREE), draggable handles: attack peak (x), decay end (x = decay, y = sustain), release end
(x), with labels `A 2 MS`, `D 400 MS`, `S 12/15`, `R 150 MS`.

Focus editors (open from tiles / ECHO button; back chip returns to the channel view):
SWEEP (start dot drag-y = amount, end dot drag-x = time; DOWN/UP; EXP/LIN toggle; a non-
clickable `NES STEP`/`GB STEP`/`SMOOTH` indicator), VIBRATO (delay handle drag-x, depth/fade
handle drag-x = fade, drag-y = depth; SINE/TRI/SQUARE; `5.5 HZ` rate drag-value), CRUSH (bit
chips 1–8, rate chips 44.1K/22K/11K/5.5K; vertical drag on the display = continuous bits,
horizontal = continuous rate), GLIDE (end handle drag-x = time; ALWAYS/LEGATO; EXP/LIN),
UNISON (1–4; `DET 14 C`, `WIDTH 60%` drag-values; POLY/MONO/LEGATO voice mode; `POLY 8`
polyphony drag-value in POLY), **ARP** (new: piano-roll lanes showing the arp pattern on an
example chord, orange stepped pitch line; mode chips UP/DOWN/⇅/RND/ORD; drag-values `CHORD
MAJ`, `OCT 1`, `RATE 20 HZ` or `1/16` when synced; `SYNC` toggle), **ECHO** (new: tap stems at
k×time decaying by feedback, L/R offset by width, dashed decay envelope; first tap drag-x =
time (snaps to divisions when synced), drag-y = mix; second tap drag-y = feedback; FIR chips
FLAT/WARM/DARK/CAVE/BRIGHT/LOFI; `SYNC` toggle; time drag-value; `WIDTH` drag-value). Axes
auto-scale to the current values with "nice" ranges. Every editor's orange stepped line
reflects the current chip mode's frame stepping.

**Tiles row (6 tiles):** SWEEP, VIBRATO, ARP, CRUSH, GLIDE, UNISON in the design's tile style
(equal columns, gap 10). Values are live (modulated values; a tiny accent tick shows when a
macro is moving the value). ARP tile: big = rate (`20`) or division (`1/16`), unit
`HZ · UP` / `· UP · MAJ`, viz = steps. Click = open/close focus editor; dot = enable; vertical
drag (3 px threshold) = adjust the tile's primary parameter (sweep_amt, vib_depth, arp_rate,
crush_bits, glide_time, uni_detune) with a tooltip; scroll wheel = fine adjust; double-click =
reset; focused tile is inverted (ink background) as designed.

**Macro row.** `MACRO` label, four macro boxes (number, name, bar, value) as designed: drag
horizontally to set the value, double-click resets, click selects the macro and enters
ASSIGN mode. New `ECHO` button before ASSIGN (style of ASSIGN; label + dot: dot toggles
`echo_on`, label opens the echo editor; inverted when the echo editor is open). ASSIGN toggles
assign mode (label `ГОТОВО`/`DONE`, accent background when active). In assign mode the selected
macro's targets get a dashed accent border and a badge (`M2 +40%`) on tiles and channel-strip
chips; dragging vertically on any assignable control sets the depth (−100..+100 %),
double-click removes the routing.

**Preset browser overlay** (as designed, fully functional): search field (live filtering by
name/tags/description), categories with live counts (Все/All, Lead, Bass, Arp, Pad, Drum,
SFX, and «Мои»/User when user presets exist), chip filter chips GB/NES/FREE and ★ favourites,
list header `ВСЕ ПРЕСЕТЫ · 128` with sort toggle `A–Z ↓` / `№ ↓`, virtualized scrolling list
(wheel/trackpad, thin scrollbar), click = select (details on the right), double-click /
Enter = load, ↑/↓ = move selection, Esc = close; right panel details with favourite star,
tags, description in the UI language, `Загрузить`/`Load` and `ESC` buttons, plus «Сохранить
текущий…» (inline form: name field, category chips, Save/Cancel; errors shown in small accent
text) and «Удалить» for user presets.

**General interaction rules.** Hover states on everything clickable (120 ms ease); pressed
states; value tooltips while dragging (design tip style: ink bg, white mono 10 px, radius 5);
Shift (or ⌘) = fine adjust ×0.1; Option-click or double-click = reset to default; infinite
drag for value drags (cursor hidden, restored on release); right-click on any parameter control
→ context menu: «Сбросить», «Назначить на макро ▸ 1–4», «Убрать из макро»; every parameter
gesture uses `begin_edit/set_param/end_edit` so Logic records automation and undo correctly.
Transitions between hero views crossfade (≈160 ms); the browser fades/slides in (≈160 ms).
All strings go through an i18n table (RU default, EN).

### 7.4 Tests

Golden snapshots for all 11 design states + ENV, ARP, ECHO, browser-save, assign mode,
NES/FREE variants; synthetic-event tests (drawing the wave updates `Bridge::set_wave`, tile drag
emits begin/set/end with the right values, assign-mode drag creates routings, browser search
filters); layout tests that no text overflows its box at 1× in both languages.

---

## 8. `pixl-plugin` — nih-plug glue

- **Params.** `PixlParams` implements nih-plug's `Params` manually from `pixl_core::PARAMS`
  (single source of truth): Float → `FloatParam` (Linear/Skewed ranges identical to the
  table), Int → `IntParam`, Bool → `BoolParam`, Enum → `IntParam` with label formatting/
  parsing. Formatting/parsing via `ParamId::format/parse`. `param_map()` returns
  `(key, ptr, group)` in table order. Persistent fields via `serialize_fields`/
  `deserialize_fields`: `wave`, `macros`, `preset` (current preset meta + modified), `ui`
  (UiState). Robust to missing/garbage fields (keep defaults).
- **Process.** `SAMPLE_ACCURATE_AUTOMATION = true`; stereo out, no inputs;
  `MidiConfig::MidiCCs`. Per block: transport → engine/fx; wave/macros via versioned atomics;
  split at note events and every ≤ 64 samples; per sub-block build base `ParamValues`, apply
  macros, `set_params` on engine and fx, render, process; publish telemetry. MIDI: NoteOn
  (vel 0 = off), NoteOff, Choke, PitchBend, CC1 mod wheel, CC64 sustain, CC120 all sound off,
  CC121 reset controllers, CC123 all notes off, channel pressure → aftertouch. Factor this
  into a framework-free `Processor` struct so it is unit-testable without a host.
- **Presets store.** Factory library embedded with `include_str!("…/presets/factory.json")`,
  parsed once. User presets: `~/Library/Application Support/PIXL/Presets/*.pixlpreset`
  (atomic writes, sanitized file names); favourites: `…/PIXL/favorites.json`. Loading a preset
  uses `GuiContext::set_state(PluginState)` so the host is notified and records it.
- **Editor.** `nih_plug::editor::Editor` impl: `spawn` → `pixl_gui::open_editor` with a
  `PluginBridge` (GuiContext + params + shared state + preset store); `size()` = design size
  × `UiState.scale`; resize via `GuiContext::request_resize`. `pixl_gui::prewarm()` on
  instance creation.
- **Identity.** CLAP id `audio.pixl.pixl`, features Instrument/Synthesizer/Stereo, vendor
  "PIXL Audio", version from Cargo.
- **Tests.** Param map ↔ table equivalence (keys, ranges, defaults, formatting); state
  roundtrip; preset load/save roundtrip; MIDI mapping; `Processor` sample-accuracy and macro
  application; factory library loads with zero warnings and every preset renders audible,
  non-clipping audio for a C4 note.

## 9. Factory presets (`presets/factory.json`)

128 presets in `pixl-library` format: Lead 001–032 (32), Bass 033–056 (24), Arp 057–074 (18),
Pad 075–086 (12), Drum 087–108 (22), SFX 109–128 (20). Must include the design's twelve with
their descriptions (Hyrule Fanfare = LEAD 014 and equal to the init state; Overworld 1-1,
Ocarina Lead, Pocket Arp, Castle Bass, Coin Square, Lavender Wave, Power-Up, Static Snare,
Dungeon Drone, Kick 8, Rupee Blip). Names evocative of 8-/16-bit adventure games without
using third-party trademarks beyond those twelve. Each preset: RU + EN description (one or two
sentences, like the design's), tags `[CHIP, CATEGORY, WAVE-TYPE]`, meaningful macro routings
(the four macros BRIGHT / DIRT / MOTION / SPACE must do something musical on every preset),
icon matching the main channel. Sound-design guidance: NES drums layer noise + triangle
(kick: triangle sweep down + short noise; snare: noise + pulse burst); leads use duty choice,
delayed vibrato, legato glide; arps use `arp_chord`; bass uses triangle (NES) or wave/pulse
(GB); pads use slow attacks, unison, echo; SFX use sweeps, arps and short envelopes.
Validation test: library parses with zero warnings, numbers/categories as above, unique names.
