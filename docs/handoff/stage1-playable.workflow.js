export const meta = {
  name: 'pixl-playable',
  description: 'PIXL stage 1 (playable build): finish DSP engine in parallel with FX then plugin glue, max 2 agents at a time',
  phases: [
    { title: 'Playable', detail: 'dsp || (fx -> plugin), continuing partial work on disk' },
  ],
}

const REPORT = {
  type: 'object',
  properties: {
    summary: { type: 'string', description: 'What was built, key design decisions, measured numbers' },
    files_changed: { type: 'array', items: { type: 'string' } },
    tests: { type: 'string', description: 'Exact test/clippy status with counts' },
    open_issues: { type: 'array', items: { type: 'string' }, description: 'Known gaps, bugs, TODOs, requests for other crates' },
    spec_deviations: { type: 'array', items: { type: 'string' } },
  },
  required: ['summary', 'files_changed', 'tests', 'open_issues', 'spec_deviations'],
}

const SCRATCH = '/private/tmp/claude-501/-Users-anomatver-Develop-PIXL/d3514e8b-a7aa-468e-b5c4-f30ff83f387d/scratchpad'

const TARGET_NOTE = {
  dsp: 'Prefix every cargo command with `CARGO_TARGET_DIR=target/wf-dsp` (your own target dir; renders still go to target/pixl-renders/).',
  fx: 'Prefix every cargo command with `CARGO_TARGET_DIR=target/wf-fx` (your own target dir; renders still go to target/pixl-renders/).',
  plugin: 'Use the default target dir (the Makefile builds there too).',
}

const COMMON = (key) => `You are a senior engineer on PIXL, a premium (~$3000-class) chiptune / 8-bit synthesizer plugin written in Rust for Logic Pro (AU), plus VST3/CLAP. Repository: /Users/anomatver/Develop/PIXL.

FIRST read docs/SPEC.md completely (it is the binding contract) and the implemented shared crate crates/pixl-core/src/*.rs (parameter table, enums, macros, wave RAM, preset format, Bridge trait, lock-free sync). Do not modify pixl-core; if you need a change there, put it in open_issues.

CONTINUATION: this task was started twice before and both runs were cut off by an account usage limit. The files listed below under "partial work" were written by the previous engineer in a hurry and have NEVER been compiled or tested together. Review them critically against SPEC (do not trust them blindly), keep what is sound, fix or rewrite what is not, and finish the job completely. The user is waiting to PLAY the instrument in Logic as soon as possible: this stage produces the first playable build (with the host's generic parameter UI; the custom editor comes later), so the sound engine, effects and plugin glue must actually work end to end.

Environment facts (already probed): Apple M4 Max, 14 cores, macOS; rustc/cargo 1.99. There is NO \`timeout\` command on macOS. System python3 is 3.9 WITHOUT numpy/scipy — do numeric analysis in Rust (tests, examples, or scratch binaries), not Python. The git repo has no commits yet (git log fails; expected). ${TARGET_NOTE[key]} The rendering crates pixl-gfx and pixl-gui are NOT being worked on in this stage and pixl-gfx does not compile yet — never build them; that is expected.

Working-tree rules (two engineers work concurrently in this tree):
- ONLY edit files inside the paths you own (listed in your task). Never edit other crates.
- Never run \`make\` targets that install (\`make\`, \`make install\`, \`make universal\`) or anything writing into ~/Library. Never run git commands that change the index/history (no add/commit/stash/checkout/reset).
- Verify with \`cargo test -p <your-crate> --release\` and \`cargo clippy -p <your-crate> --all-targets --release -- -D warnings\`. If compilation fails inside a crate you do not own (the other engineer is mid-edit), wait ~60 s (Monitor/until-loop) and retry; never "fix" their files.
- Quality bar is SPEC §0: real-time safety (no alloc/locks/IO on the audio thread), no panics on runtime data, no NaN/Inf escaping, determinism, thorough tests, doc comments, rustfmt (rustfmt.toml: max_width 120). Commercial product judged by professional sound designers and developers: production-grade code, no TODO stubs for spec'd features.
- Finish with: your crate builds with zero warnings, all its tests pass, clippy is clean. Then report honestly (including anything not done) using the structured output.`

const DSP = `${COMMON('dsp')}

YOUR TASK: implement the synthesis engine crate \`pixl-dsp\` (SPEC §3 and §4) behind the frozen public API in crates/pixl-dsp/src/lib.rs (keep every public signature source-compatible; you may add public items).
You own: crates/pixl-dsp/** (you may add dependencies to its Cargo.toml; keep runtime deps minimal — e.g. rustfft is fine for init-time kernel design or as a dev-dependency for tests; prefer a tiny hand-written PRNG over rand).
Partial work on disk: src/blip.rs (min-phase kernel + BlipBuffer) and src/analysis.rs (wired into lib.rs), src/chip.rs (not yet wired). The Engine in lib.rs is still the silent stub — everything else is yours to write.
IMPORTANT: the plugin engineer builds against your crate concurrently. Keep \`cargo check -p pixl-dsp\` passing between edits: add a new module to lib.rs only once it compiles, and never leave the public API broken for long. Get a complete, musical engine producing sound early (all four channels, envelopes, poly/mono/legato, glide, sweep, vibrato, arp, unison, chip modes), then deepen the tests.
Suggested structure: blip.rs, chip.rs (NES/GB clocks, tables, register quantization, duty sequences, NES non-linear mixer), osc/{pulse,wave,noise}.rs, env.rs, modulation.rs (sweep, vibrato, glide, arp), voice.rs (voice with unison sub-voices × 4 channels, time-ordered event merging per sub-voice, frame-tick handling), alloc.rs (poly/mono/legato, note stack, sustain, stealing with 2 ms fade), smooth.rs, engine.rs.
Priorities in order: (1) chip-accurate behaviour per SPEC tables; (2) alias-free zero-latency min-phase BLEP rendering (acceptance test in SPEC §4.2); (3) real-time safety + CPU targets (§4.10); (4) musical feel: glide/arp/mono/legato/sustain/velocity behave like a pro instrument.
Write all tests in SPEC §4.11 (FFT alias test with a Blackman-Harris window and harmonics masked; register quantization; LFSR periods; envelope timing; allocation; glide timing; arp; fuzzing for NaN; determinism; a benchmark test printing CPU % per §4.10 — mark slow ones #[ignore] only if they take >10 s in release).
Also add examples/render_wav.rs that writes WAVs (tiny WAV writer or hound as a dev-dependency) into target/pixl-renders/: a C-major melody and chords for each chip mode, an NES drum pattern (kick = triangle sweep down + short noise, snare = noise + pulse burst, hats = short-mode noise), an arp demo, a unison/glide lead. You cannot listen, so verify numerically: pitch (FFT/zero crossings), levels (RMS/peak, calibration ±1.5 dB across chip modes per §4.3), envelope timing, spectra. Report measured CPU numbers, alias levels and level calibration.`

const FX = `${COMMON('fx')}

YOUR TASK: implement the effect-chain crate \`pixl-fx\` (SPEC §5) behind the frozen public API in crates/pixl-fx/src/lib.rs (keep signatures source-compatible; you may add public items).
You own: crates/pixl-fx/** (deps allowed in its Cargo.toml; keep minimal).
Partial work on disk: src/{util,filters,crusher,resampler,echo,output,meter,lib}.rs and tests/chain.rs all exist. The crate currently FAILS to compile with 4 borrow-check errors (filters.rs: E0499 "cannot borrow p.nes[_] as mutable more than once"; echo.rs: E0499 "cannot borrow self.core as mutable more than once"). A Q7 8-tap FIR preset search tool (Rust) from the previous run is at ${SCRATCH}/firdesign (main.rs + outputs) — reuse/adapt it, but verify its numbers yourself.
Spec summary: crusher.rs (quantizer + fractional S&H with band-limited (polyBLEP or minBLEP) step rendering, smoothed rate), filters.rs (TPT one-pole HPF/LPF, DC blocker, NES 90 Hz/440 Hz HPF + 14 kHz LPF, GB 28 Hz HPF, click-free mode switching), resampler.rs (arbitrary-ratio polyphase Kaiser-windowed-sinc, streaming, for host↔32 kHz at 22.05–192 kHz), echo.rs (SNES SPC700-style: 32 kHz domain, stereo lines up to 2 s, 8-tap Q7 FIR presets FLAT/WARM/DARK/CAVE/BRIGHT/LOFI that you design and document with their magnitude responses, DC gain ≤ 1, feedback ≤ 95 % through a soft clipper and 16-bit quantization, width = independent→ping-pong, click-free time changes via 30 ms crossfade, tempo sync via EchoDiv, natural tail when switched off), output.rs (smoothed gain, safety clipper transparent below −1 dBFS with ceiling −0.01 dBFS), meter.rs (peak with −40 dB/s release, 300 ms RMS, 1.5 s peak hold).
Fix the build FIRST (the plugin engineer starts right after you and depends on your crate), then complete and harden: all tests in SPEC §5.6 with measured numbers (resampler THD+N and passband ripple for 44.1/48/96/192 kHz, FIR responses at key frequencies, echo stability 60 s at fb 95 % for all presets, clipper transparency, meter ballistics, NaN/Inf fuzz). Add examples/render_fx.rs that synthesizes test material (impulse, sine sweep, a short square-wave melody) and writes WAVs for each echo preset and several crusher settings into target/pixl-renders/. Report measured numbers.`

const PLUGIN = (fx) => `${COMMON('plugin')}

YOUR TASK: implement the plugin crate \`pixl-plugin\` (SPEC §8): nih-plug glue, parameters, state, MIDI, process loop, presets store, editor integration.
You own: crates/pixl-plugin/** and wrapper/** (only if really needed; keep the clap-wrapper clap-first approach). Do NOT edit the Makefile (put requests in open_issues). You MAY run \`make build\` (writes only into build/ and target/) but never install targets.
The fx engineer (finished just before you) reported: ${fx ? fx.summary : '(fx agent produced no report — inspect crates/pixl-fx yourself; if it does not compile, report that and stop early)'}
FX open issues: ${fx ? JSON.stringify(fx.open_issues) : '[]'}
The dsp engine (pixl-dsp) is being finished CONCURRENTLY by another engineer behind its frozen API — code against that API; if pixl-dsp temporarily fails to compile, wait and retry.
Partial work on disk: src/{bridge,editor,midi,params,presets,processor,state}.rs, src/lib.rs, tests/render.rs, tests/clap_host.rs and Cargo.toml — never compiled (it was blocked by fx/gfx errors).
GUI GATING (new requirement): pixl-gfx/pixl-gui will only be finished in a LATER stage and do not compile now. Make \`pixl-gui\` an optional dependency behind a cargo feature \`gui\` that is in \`default\`. With the feature off: no editor (Plugin::editor returns None → the host shows its generic parameter UI), no prewarm, everything else identical. Build, test and clippy with \`--no-default-features\` in this stage (e.g. \`cargo test -p pixl-plugin --release --no-default-features\`). The code behind \`gui\` must still be written against pixl-gui's frozen API (crates/pixl-gui/src/lib.rs: open_editor, prewarm, DESIGN_SIZE, EditorHandle) so it compiles once gfx exists — you cannot compile it now, so keep it small and obviously correct. The Makefile passes CARGO_FLAGS to cargo: \`CARGO_FLAGS=--no-default-features make build\`.
nih-plug is pinned in the workspace (git rev de42101, default-features = false; VST3/AU come from clap-wrapper, so do not enable nih-plug's vst3 feature). Its source is at ~/.cargo/git/checkouts/nih-plug-d4210f82e920295f/de42101/ — src/params.rs (Params trait), src/params/internals.rs (ParamPtr), src/params/{float,integer,boolean}.rs, src/editor.rs, src/context/gui.rs (GuiContext incl. set_state/get_state/request_resize), src/wrapper/state.rs, src/plugin.rs, plugins/examples.
Implement per SPEC §8: manual \`unsafe impl Params\` built from pixl_core::PARAMS (single source of truth; stable keys; groups; formatting via ParamId::format/parse; enums as IntParam with labels); persistent fields wave/macros/preset/ui via serialize_fields/deserialize_fields (robust to garbage); a framework-free \`Processor\` (engine + fx + sub-block splitting at events and every ≤64 samples + macro application via pixl_core::apply_macros + telemetry publishing) that the nih-plug Plugin wraps; MIDI mapping (NoteOn/Off/Choke, pitch bend, CC1, CC64, CC120, CC121, CC123, channel pressure); SAMPLE_ACCURATE_AUTOMATION; tail reporting; presets store (factory library via include_str! of presets/factory.json — currently a 1-preset placeholder, the 128-preset library lands in a later stage, format is fixed; user presets in ~/Library/Application Support/PIXL/Presets/*.pixlpreset with atomic writes and sanitized names; favorites.json) — in tests use a temp directory via an injectable base path, never touch the real user folder; PluginBridge implementing pixl_core::Bridge (begin/set/end via ParamSetter/GuiContext, wave/macros through pixl_core::sync atomics, presets via GuiContext::set_state so the host is notified, modified flag, telemetry, UiState, request_resize); nih_plug Editor impl behind \`gui\` (spawn → pixl_gui::open_editor with the AppKit NSView parent; size = 900×560 × UiState.scale; handle must drop on the main thread); fresh instance = factory 014 "Hyrule Fanfare" unmodified (defaults + default_wave + MacroAssignments::factory_default()).
Tests (cargo test -p pixl-plugin --release --no-default-features): param map ↔ table equivalence (keys, ranges via preview_normalized/preview_plain, defaults, formatting); state roundtrip; preset load/save roundtrip in a temp dir; MIDI mapping; Processor sample accuracy (event at offset N starts sound at N), macro application, telemetry; an end-to-end render of a short MIDI phrase through the full Processor checking the output is musical (non-silent, correct pitch via zero crossings/FFT, no NaN/Inf, peak ≤ ceiling, no clicks at note boundaries); the factory library loads with zero warnings. The end-to-end sound tests need the real engine: if the dsp engineer is still working when you are otherwise done, poll every ~3 minutes (Monitor/until-loop on \`cargo check -p pixl-dsp\` + your test) for up to ~45 minutes, then report the state honestly.
Finally build the bundles with \`CARGO_FLAGS=--no-default-features make build\` and run \`clap-validator validate build/wrapper/out/PIXL.clap\` (clap-validator 0.4.1 is installed at ~/.cargo/bin/clap-validator; source for reading its tests: ~/.cargo/git/checkouts/clap-validator-d84e531ad80a8d4f/b2f1d9b). Fix every failure in your crate. Report the validator results.`

phase('Playable')
const [dsp, chain] = await parallel([
  () => agent(DSP, { label: 'dsp', phase: 'Playable', schema: REPORT }),
  async () => {
    const fx = await agent(FX, { label: 'fx', phase: 'Playable', schema: REPORT })
    log(fx ? 'fx finished — starting plugin glue' : 'fx agent failed — plugin agent will inspect pixl-fx itself')
    const plugin = await agent(PLUGIN(fx), { label: 'plugin', phase: 'Playable', schema: REPORT })
    return { fx, plugin }
  },
])
return { dsp, fx: chain ? chain.fx : null, plugin: chain ? chain.plugin : null }
