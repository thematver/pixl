export const meta = {
  name: 'pixl-build',
  description: 'Implement PIXL crates in parallel: DSP engine, FX chain, gfx platform then GUI, factory presets, plugin glue (restart after interrupted run)',
  phases: [
    { title: 'Build', detail: 'dsp, fx, gfx, presets, plugin in parallel' },
    { title: 'GUI', detail: 'full editor UI on top of the finished gfx platform' },
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

const TARGET_NOTE = {
  dsp: 'Use your own cargo target dir to avoid blocking on other engineers\' heavy wgpu/vello builds: prefix every cargo command with `CARGO_TARGET_DIR=target/wf-dsp` (renders still go to target/pixl-renders/).',
  fx: 'Use your own cargo target dir to avoid blocking on other engineers\' heavy wgpu/vello builds: prefix every cargo command with `CARGO_TARGET_DIR=target/wf-fx` (renders still go to target/pixl-renders/).',
  presets: 'Use your own cargo target dir to avoid blocking on other engineers\' heavy wgpu/vello builds: prefix every cargo command with `CARGO_TARGET_DIR=target/wf-presets`.',
  plugin: 'Use the default target dir (shared with the gfx/gui engineers); expect occasional "Blocking waiting for file lock" — that is normal, just wait.',
  gfx: 'Use the default target dir (the GUI engineer builds on it after you); expect occasional "Blocking waiting for file lock" while others build — that is normal, just wait.',
  gui: 'Use the default target dir; expect occasional "Blocking waiting for file lock" while the plugin engineer builds — that is normal, just wait.',
}

const COMMON = (key) => `You are a senior engineer on PIXL, a premium (~$3000-class) chiptune / 8-bit synthesizer plugin written in Rust for Logic Pro (AU), plus VST3/CLAP. Repository: /Users/anomatver/Develop/PIXL.

FIRST read docs/SPEC.md completely (it is the binding contract) and the implemented shared crate crates/pixl-core/src/*.rs (parameter table, enums, macros, wave RAM, preset format, Bridge trait, lock-free sync). Do not modify pixl-core unless your task explicitly allows it; if you need a change there, put it in open_issues.

Restart note: an earlier run of this exact task was interrupted (account usage limit) before anyone finished. Files you own may already contain partial work from that run — inspect them, keep what is sound, and finish the job completely.

Environment facts (already probed — do not waste time re-discovering): Apple M4 Max, 14 cores, macOS; rustc/cargo 1.99. There is NO \`timeout\` command on macOS. System python3 is 3.9 WITHOUT numpy/scipy — do numeric analysis in Rust (tests, examples, or scratch binaries in your scratchpad), not Python. The git repo has no commits yet (git log fails; that is expected). ${TARGET_NOTE[key]}

Working-tree rules (several engineers work concurrently in this same tree):
- ONLY edit files inside the paths you own (listed in your task). Never edit other crates.
- Never run \`make\` targets that install (\`make\`, \`make install\`, \`make universal\`) or anything writing into ~/Library. Never run git commands that change the index/history (no add/commit/stash/checkout/reset).
- Verify with \`cargo test -p <your-crate> --release\` and \`cargo clippy -p <your-crate> --all-targets --release -- -D warnings\` (building only your crate and its dependencies avoids interference). If compilation fails inside a crate you do not own (another engineer is mid-edit), wait ~60 s (use a Monitor/until-loop, not a bare sleep longer than needed) and retry; never "fix" their files.
- Quality bar is SPEC §0: real-time safety (no alloc/locks/IO on the audio thread), no panics on runtime data, no NaN/Inf escaping, determinism, thorough tests, doc comments, rustfmt (rustfmt.toml: max_width 120). This is a commercial product judged by professional sound designers and developers: write production-grade code, not a prototype, and do not leave TODO stubs for spec'd features.
- Finish with: your crate builds with zero warnings, all its tests pass, clippy is clean. Then report honestly (including anything not done) using the structured output.`

const DSP = `${COMMON('dsp')}

YOUR TASK: implement the synthesis engine crate \`pixl-dsp\` (SPEC §3 and §4) behind the frozen public API already stubbed in crates/pixl-dsp/src/lib.rs (keep every public signature source-compatible; you may add public items).
You own: crates/pixl-dsp/** (you may add dependencies to its Cargo.toml; keep runtime deps minimal — e.g. rustfft is fine for init-time kernel design or as a dev-dependency for tests; prefer a tiny hand-written PRNG over rand).
Suggested structure: blip.rs (min-phase kernel design + BlipBuffer), chip.rs (NES/GB clocks, tables, register quantization, duty sequences, NES non-linear mixer), osc/{pulse,wave,noise}.rs, env.rs, modulation.rs (sweep, vibrato, glide, arp), voice.rs (voice with unison sub-voices × 4 channels, time-ordered event merging per sub-voice, frame-tick handling), alloc.rs (poly/mono/legato, note stack, sustain, stealing with 2 ms fade), smooth.rs, engine.rs.
Priorities in order: (1) chip-accurate behaviour per SPEC tables; (2) alias-free zero-latency min-phase BLEP rendering (acceptance test in SPEC §4.2); (3) real-time safety + CPU targets (§4.10); (4) musical feel: glide/arp/mono/legato/sustain/velocity behave like a pro instrument.
Write all tests in SPEC §4.11 (FFT alias test with a Blackman-Harris window and harmonics masked; register quantization; LFSR periods; envelope timing; allocation; glide timing; arp; fuzzing for NaN; determinism; a benchmark test printing CPU % per §4.10 — mark slow ones #[ignore] only if they take >10 s in release).
Also add examples/render_wav.rs that writes WAVs (write a tiny WAV writer or use hound as a dev-dependency) into target/pixl-renders/: a C-major melody and chords for each chip mode, an NES drum pattern (kick = triangle sweep down + short noise, snare = noise + pulse burst, hats = short-mode noise), an arp demo, a unison/glide lead. You cannot listen, so verify numerically: pitch (FFT/zero crossings), levels (RMS/peak, calibration ±1.5 dB across chip modes per §4.3), envelope timing, spectra. Report measured CPU numbers, alias levels and level calibration.`

const FX = `${COMMON('fx')}

YOUR TASK: implement the effect-chain crate \`pixl-fx\` (SPEC §5) behind the frozen public API stubbed in crates/pixl-fx/src/lib.rs (keep signatures source-compatible; you may add public items).
You own: crates/pixl-fx/** (deps allowed in its Cargo.toml; keep minimal).
The interrupted earlier run started a Q7 8-tap FIR preset search tool (Rust) at /private/tmp/claude-501/-Users-anomatver-Develop-PIXL/54b1a79a-e4f8-4741-bf08-d269b0d2a3fa/scratchpad/firdesign/main.rs — if it still exists you may reuse or adapt it (copy into your scratchpad or the crate); verify its output yourself.
Suggested modules: crusher.rs (quantizer + fractional S&H with band-limited (polyBLEP or minBLEP) step rendering, smoothed rate), filters.rs (TPT one-pole HPF/LPF, DC blocker, NES 90 Hz/440 Hz HPF + 14 kHz LPF, GB 28 Hz HPF, click-free mode switching), resampler.rs (arbitrary-ratio polyphase Kaiser-windowed-sinc, streaming, for host↔32 kHz at 22.05–192 kHz), echo.rs (SNES SPC700-style: 32 kHz domain, stereo lines up to 2 s, 8-tap Q7 FIR presets FLAT/WARM/DARK/CAVE/BRIGHT/LOFI that you design and document with their magnitude responses, DC gain ≤ 1, feedback ≤ 95 % through a soft clipper and 16-bit quantization, width = independent→ping-pong, click-free time changes via 30 ms crossfade, tempo sync via EchoDiv, natural tail when switched off), output.rs (smoothed gain, safety clipper transparent below −1 dBFS with ceiling −0.01 dBFS), meter.rs (peak with −40 dB/s release, 300 ms RMS, 1.5 s peak hold).
Write all tests in SPEC §5.6 with measured numbers (resampler THD+N and passband ripple for 44.1/48/96/192 kHz, FIR responses at key frequencies, echo stability 60 s at fb 95 % for all presets, clipper transparency, meter ballistics, NaN/Inf fuzz). Add examples/render_fx.rs that synthesizes test material (impulse, sine sweep, a short square-wave melody) and writes WAVs for each echo preset and several crusher settings into target/pixl-renders/. Report measured numbers.`

const PRESETS = `${COMMON('presets')}

YOUR TASK: be PIXL's sound designer and author the 128-preset factory library (SPEC §9; preset file format in crates/pixl-core/src/preset.rs; parameter semantics in crates/pixl-core/src/params.rs, enums.rs and SPEC §3–4; read crates/pixl-core/src/macros.rs for MacroAssignments::factory_default()).
You own: presets/factory.json (currently a one-preset placeholder — replace it entirely), tools/presets/** (a generator script, e.g. Python 3.9 stdlib only, that produces the JSON deterministically — keep it in the repo so presets can be maintained), and the new test file crates/pixl-core/tests/factory_library.rs.
Requirements: exactly 128 presets: Lead 001–032, Bass 033–056, Arp 057–074, Pad 075–086, Drum 087–108, SFX 109–128 (category counts 32/24/18/12/22/20). Include the design's twelve with their Russian descriptions verbatim from docs/design/source/PIXL.dc.html (the PR array in renderVals) plus English translations: Hyrule Fanfare (LEAD 014, GB, wav) MUST equal the init state: "params": {} (all defaults), no "wave" key (default wave), "macros" exactly equal to MacroAssignments::factory_default(). Others: Overworld 1-1, Ocarina Lead, Pocket Arp, Castle Bass, Coin Square, Lavender Wave, Power-Up, Static Snare, Dungeon Drone, Kick 8, Rupee Blip — implement their descriptions faithfully with the parameters. All other names: evocative of 8/16-bit adventure/platformer games, original (no third-party trademarks), unique.
Sound design: every preset musically purposeful and distinct; set explicitly every parameter that matters (all four *_on flags, levels, envelopes, duties, effect on/off and values, voice mode, glide, chip). Use the hardware idioms from SPEC §9 (NES drums layering noise + triangle with sweep; pulse duty choices; delayed vibrato; legato glide; arp_chord fake chords at frame rates; GB wave basses; pads with slow attack + unison + echo; SFX with sweeps/arps/short envelopes). Include custom wave RAM shapes where the wave channel is used (32 values 0..15). Every preset needs meaningful routings for all 4 macros (BRIGHT, DIRT, MOTION, SPACE) with depths that sound good at the default macro positions 62/30/45/20 (set macro1..macro4 values per preset where useful). Descriptions: 1–2 sentences, RU (primary, same style as the design) + EN. Tags: [CHIP, CATEGORY, WAVE-TYPE] like the design. Icon matches the main channel (p12/p50/tri/wav/noi).
The validation test (cargo test -p pixl-core --release) must check: parses as PresetLibrary with zero warnings from Preset::to_state for every preset; 128 presets; category counts and number ranges; unique names and numbers; valid icons; non-empty ru/en descriptions and tags; ≥1 enabled channel each; every macro has ≥1 routing; preset 014 equals the init state (values == ParamValues::defaults(), wave == default_wave(), macros == factory_default()).`

const PLUGIN = `${COMMON('plugin')}

YOUR TASK: implement the plugin crate \`pixl-plugin\` (SPEC §8): nih-plug glue, parameters, state, MIDI, process loop, presets store, editor integration.
You own: crates/pixl-plugin/** and wrapper/** (only if really needed; keep the clap-wrapper clap-first approach). Do NOT edit the Makefile (put requests in open_issues). You MAY run \`make build\` (it only writes into build/ and target/; it fails if another crate is mid-edit — retry later) but never install targets.
Context: crates/pixl-plugin/src/lib.rs is a placeholder square-wave plugin — replace it. pixl-dsp (Engine), pixl-fx (FxChain) and pixl-gui (open_editor/prewarm/DESIGN_SIZE) are being implemented concurrently behind FROZEN APIs that are already stubbed (engine renders silence, open_editor returns None) — code against those APIs; never edit them. nih-plug is pinned in the workspace (git rev de42101, default-features = false; VST3/AU come from clap-wrapper, so do not enable nih-plug's vst3 feature). Its source is at ~/.cargo/git/checkouts/nih-plug-*/de42101/ — read src/params.rs (Params trait), src/params/internals.rs (ParamPtr), src/params/{float,integer,boolean}.rs, src/editor.rs, src/context/gui.rs (GuiContext incl. set_state/get_state/request_resize), src/wrapper/state.rs (PluginState/ParamValue), src/plugin.rs, and plugins/examples for patterns.
Implement per SPEC §8: manual \`unsafe impl Params\` built from pixl_core::PARAMS (single source of truth; stable keys; groups; formatting via ParamId::format/parse; enums as IntParam with labels); persistent fields wave/macros/preset/ui via serialize_fields/deserialize_fields (robust to garbage); a framework-free \`Processor\` (engine + fx + sub-block splitting at events and every ≤64 samples + macro application via pixl_core::apply_macros + telemetry publishing) that the nih-plug Plugin wraps; MIDI mapping (NoteOn/Off/Choke, pitch bend, CC1, CC64, CC120, CC121, CC123, channel pressure); SAMPLE_ACCURATE_AUTOMATION; tail reporting; presets store (factory library via include_str! of presets/factory.json — that file is being rewritten concurrently by the sound designer, its format is fixed; user presets in ~/Library/Application Support/PIXL/Presets/*.pixlpreset with atomic writes and sanitized names; favorites.json) — in tests, use a temp directory via an injectable base path, never touch the real user folder; PluginBridge implementing pixl_core::Bridge (begin/set/end via ParamSetter/GuiContext, wave/macros through pixl_core::sync atomics, presets via GuiContext::set_state so the host is notified, modified flag, telemetry, UiState, request_resize); nih_plug Editor impl (spawn → pixl_gui::open_editor with the AppKit NSView parent; size = 900×560 × UiState.scale; handle must drop on the main thread); call pixl_gui::prewarm() when an instance is created; fresh instance = factory 014 "Hyrule Fanfare" unmodified (defaults + default_wave + MacroAssignments::factory_default()).
Tests (cargo test -p pixl-plugin --release): param map ↔ table equivalence (keys, ranges via preview_normalized/preview_plain, defaults, formatting); state roundtrip; preset load/save roundtrip in a temp dir; MIDI mapping; Processor sample accuracy (event at offset N starts sound at N), macro application, telemetry; the factory library loads with zero warnings. Then build the bundles with \`make build\` and run \`clap-validator validate build/wrapper/out/PIXL.clap\` — clap-validator 0.4.1 is ALREADY installed at ~/.cargo/bin/clap-validator (source checkout for reading test code: ~/.cargo/git/checkouts/clap-validator-d84e531ad80a8d4f/b2f1d9b), do not reinstall it; fix every failure in your crate. Report the validator results.`

const GFX = `${COMMON('gfx')}

YOUR TASK: implement the native rendering platform crate \`pixl-gfx\` (SPEC §6) — the foundation another engineer will use immediately after you to build the entire PIXL editor.
You own: crates/pixl-gfx/** — it already contains a WORKING spike (examples/spike.rs) showing vello 0.11 + parley 0.12 + wgpu 30 offscreen rendering with the embedded Geist fonts in assets/fonts (use it as API reference: e.g. LayoutContext needs a Brush type that is Default; FontFamily::named; draw_glyphs; Buffer mapping returns Result in wgpu 30). The interrupted earlier run already added dependencies to crates/pixl-gfx/Cargo.toml (objc2 family incl. objc2-quartz-core/core-foundation/core-graphics, skrifa, log, optional png feature) and a scratch glyph-coverage probe examples/zz_coverage.rs (run it for coverage data, then delete it or turn it into a test before you finish). Dependencies you may add: vello 0.11 (re-exports wgpu 30, kurbo, peniko), parley 0.12 (default-features = false, features = ["std"]), skrifa (glyph coverage), objc2 0.6, objc2-foundation 0.3, objc2-app-kit 0.3, objc2-quartz-core 0.3, objc2-core-foundation/core-graphics 0.3 if needed (cursor association/warp), block2 0.6, raw-window-handle 0.6, pollster, png (dev or feature for render_to_png).
Deliver per SPEC §6: (1) NSView subclass child-view platform (layer-backed Metal surface; flipped; acceptsFirstMouse; tracking areas; mouse/scroll/keys/modifiers; key events forwarded up the responder chain unless a text field is focused; insertText via interpretKeyEvents for Cyrillic; backing-scale + resize handling; 60 Hz main-runloop timer in common modes; redraw only when needed); (2) process-wide shared GpuContext (device, queue, vello Renderer) with prewarm() on a background thread, view shows #F7F6F3 until ready, never blocks main thread > 50 ms, recovers from surface/device loss; (3) Canvas drawing API ergonomic for immediate-mode UI code (rects, rounded rects, circles, BezPath fill/stroke with dashes and joins, alpha, clips, opacity layers, transforms) + text (family Sans/Mono, weight, size, color, letter-spacing in em, alignment, wrapping width, measurement; layout cache) + an icons module drawing any glyph missing from Geist as vectors (check coverage of ✎ ╱ ∿ ⇅ ★ ‹ › ← → ↓ ↑ − · × ● ⏎ with skrifa/parley); (4) App trait + Event/EventCtx (redraw, cursor, infinite drag with hidden cursor and relative deltas restored on release, text focus, native NSMenu context menus with submenus/checkmarks/disabled items returning an item id); (5) open_child_view(parent_nsview, Box<dyn App>) -> ViewHandle whose Drop detaches safely; (6) render_to_png(app, scale, path) offscreen for golden tests; (7) examples/harness.rs: NSWindow → plain NSView as fake host parent → your view as child, interactive demo (hover, drag, scroll, typing in a text field, context menu), with \`--auto-quit <secs>\`.
Write a crate-level guide (lib.rs docs with examples) because the GUI engineer builds on it right after you. Verify: unit tests; an offscreen PNG of a demo App (Read the PNG yourself: crisp text, correct colors, 2× retina); performance test: a 900×560 @2× frame with ~300 primitives + ~80 text runs encodes+renders in < 4 ms on this M-series Mac (report numbers); run the harness briefly with --auto-quit 5 and confirm no panics/wgpu validation errors; two views open simultaneously share one GPU context; dropping a view mid-animation is safe. Do not leave windows open.`

const GUI = (gfx) => `${COMMON('gui')}

YOUR TASK: build the complete PIXL editor UI crate \`pixl-gui\` (SPEC §7) on top of the just-finished rendering platform \`pixl-gfx\` (read its lib.rs guide and source first; you may NOT edit pixl-gfx — if it has a bug that blocks you, work around it inside pixl-gui and list it in open_issues).
The gfx engineer reported:
SUMMARY: ${gfx ? gfx.summary : '(gfx agent produced no report — inspect crates/pixl-gfx yourself)'}
OPEN ISSUES: ${gfx ? JSON.stringify(gfx.open_issues) : '[]'}
You own: crates/pixl-gui/** (frozen public API stub in src/lib.rs: open_editor, prewarm, DESIGN_SIZE, EditorHandle — keep it; add MockBridge and the examples harness + snapshots).
The visual source of truth is docs/design/source/PIXL.dc.html — read ALL of it (template markup with exact sizes/fonts/colors/paddings AND the renderVals() generators for every hero view and tile; port the geometry faithfully) — and the reference renders docs/design/ref/*.png (3600×2240 = 900×560 at 4×; Read them). Your \`snapshots\` example must render the same 11 states as tools/design-ref/snapshot.swift (wave, pulse1, noise, nes-triangle, sweep, vibrato, crush, glide, unison, browser, macro-assign) plus the extensions, to target/gui-snapshots/*.png at 2× or 4×. Read your PNGs next to the references and iterate until they match: typography (Geist/Geist Mono weights, sizes, letter-spacing), spacing, radii, colors, line weights, dashes, the stepped orange lines and fills.
Then implement everything else in SPEC §7.3 in the same visual language so it looks native to the design: tab enable dots + channel strip (VOL/PAN/ST/CT drag-values, divider, chip name), ENV editor for every channel, 6th tile ARP + ARP editor, ECHO button + ECHO editor, settings menu on the logo (native NSMenu via pixl-gfx), preset browser with live search/categories/counts/chip filters/favorites/sort/virtualized scroll/keyboard nav/load/save form/delete, assign mode on tiles and channel-strip chips with badges, right-click context menus, value tooltips while dragging, Shift fine-drag, double-click/Option-click reset, infinite drags, hover/pressed states and short transitions, Esc handling, i18n RU (default) + EN.
Everything is wired to pixl_core::Bridge: live (modulated) values, begin/set/end gestures for every parameter change (critical for Logic automation and undo), wave RAM edits, macro routings, presets (list/current/load/save/delete/favorite), telemetry (meter bars + peak-hold label, wave playhead, live LFSR register), UiState persistence (channel tab, focus, wave tool, scale, language, selected macro), request_resize for UI scale. MockBridge: in-memory params with the real table defaults, macro math via pixl_core::apply_macros, an animated fake telemetry, a preset list built from presets/factory.json (via include_str! or reading the file in examples).
Tests per SPEC §7.4 (synthetic events: drawing the wave calls set_wave correctly; tile drag emits begin/set/end with sane values; assign-mode drag creates routings; browser search filters; no text overflow in RU/EN at 1×). Report the snapshot paths and remaining fidelity differences honestly.`

phase('Build')
const tasks = [
  { key: 'dsp', prompt: DSP },
  { key: 'fx', prompt: FX },
  { key: 'presets', prompt: PRESETS },
  { key: 'plugin', prompt: PLUGIN },
]
const gfxThenGui = async () => {
  const gfx = await agent(GFX, { label: 'gfx', phase: 'Build', schema: REPORT })
  log(gfx ? 'gfx finished — starting GUI build' : 'gfx agent failed — GUI agent will inspect pixl-gfx itself')
  const gui = await agent(GUI(gfx), { label: 'gui', phase: 'GUI', schema: REPORT })
  return { gfx, gui }
}
const [dsp, fx, presets, plugin, ui] = await parallel([
  ...tasks.map(t => () => agent(t.prompt, { label: t.key, phase: 'Build', schema: REPORT })),
  gfxThenGui,
])
return { dsp, fx, presets, plugin, gfx: ui ? ui.gfx : null, gui: ui ? ui.gui : null }
