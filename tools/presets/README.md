# Factory library tools

The 128 factory presets (`presets/factory.json`, SPEC §9) are generated, never edited by hand.

| File | What it is |
|---|---|
| `library.py` | The presets: name, category, chip, icon, RU/EN description, parts, wave RAM, macro routings. |
| `dsl.py` | Sound-design vocabulary (`pulse()`, `wave()`, `noise()`, `sweep()`, `arp()`, `echo()`…), the preset model and macro resolution. |
| `waves.py` | Wave RAM shapes (32 × 4 bit). |
| `pixl_params.py` | Python mirror of `crates/pixl-core/src/params.rs` and `macros.rs` (keys, ranges, skews, defaults, snapping, `apply_macros`). |
| `param_table.txt` | Canonical listing of that mirror, written by `generate.py`. `crates/pixl-core/tests/factory_library.rs` rebuilds it from the Rust table and fails on any difference. |
| `generate.py` | Validates the library and writes `presets/factory.json` + `param_table.txt`. |
| `levels.json` | Per-preset output gain (dB) measured by the audition tool; read by `generate.py`. |
| `audition/` | Stand-alone Rust tool that renders every preset through the real plugin `Processor` and measures it. |

Python 3.9, standard library only. Rust: the audition crate is its own workspace (it is not a
member of the PIXL workspace) and builds against the repository crates by path.
It is built with its own cargo profile (`--profile audition`), so its artifacts stay in
`<target>/audition/` even when it shares a target directory with the PIXL workspace.

## Conventions (see `dsl.py` and the header of `library.py`)

- Every preset stores every parameter. 014 "Hyrule Fanfare" is the init state: `"params": {}`,
  no `"wave"`, macros = `MacroAssignments::factory_default()`.
- A macro routing is written as `(parameter, value_at_100)`; the stored value is what the
  target does with the macro at 0 %. `generate.py` turns that into pixl-core's normalized depth.
- Macro roles: BRIGHT (duty, layers, noise colour, attack), DIRT (crusher, detune, grit),
  MOTION (vibrato, glide, arp, sweep, envelope length), SPACE (echo, width, release tails).
- BRIGHT/MOTION/SPACE load at 62/45/20 % unless a preset stores other positions. DIRT is stored
  at 0 % on every factory preset (014 excepted), so presets load clean and DIRT adds the grit;
  a preset written with DIRT elsewhere is rebased automatically (`Preset._rebase_dirt`).
- One-shot drums and effects (sustain 0) use a release about as long as the decay (half of it
  for the volume-less NES triangle, whose envelope only gates), so short MIDI notes do not choke
  the designed decay.
- Detuned pairs are detuned symmetrically around the note, so the preset is heard in tune.
- `analog` stays on (authentic DAC + output filter) unless it costs the sound: NES basses and
  kicks keep their fundamental with it off (the 2A03 high-passes at 90/440 Hz); elsewhere it
  goes off where the DAC's note-on step (unipolar levels through the output capacitor) would cost
  several dB of headroom or pop under a staccato bass, a hat or a dense arp.

## generate.py

```sh
python3 tools/presets/generate.py                 # validate, write presets/factory.json and param_table.txt
python3 tools/presets/generate.py --check         # validate only; exit 1 if anything is wrong or stale
python3 tools/presets/generate.py --report        # effective values at load + every routing at 0 % | stored | 100 %
python3 tools/presets/generate.py --report 14 57-60
python3 tools/presets/generate.py --out /tmp/x.json   # write elsewhere (experiments with the audition)
```

Validation covers SPEC §9 (counts, number ranges, the design's twelve verbatim, 014 = init,
unique trademark-free names, tags, icons matching the main channel, 1–2 sentence RU/EN
descriptions) and sound sanity: every value in range and snapped, ≥ 1 channel, every macro
1–8 routings to modulatable targets with no dead routing (including GB hard-pan quantization
of unison width and the NES triangle's missing volume), no value on a chip rounding boundary,
and no NES/GB pulse pitched below the chip's lowest pulse frequency over the category's range.

## Audition

```sh
CARGO_TARGET_DIR=target/wf-presets cargo build --profile audition --manifest-path tools/presets/audition/Cargo.toml
target/wf-presets/audition/audition                 # audition all 128 presets (≈ 1 min on 14 cores)
target/wf-presets/audition/audition --only 1,57-60 --details
target/wf-presets/audition/audition --calibrate     # also write tools/presets/levels.json
target/wf-presets/audition/audition --wav DIR       # write each preset's test phrase as 24-bit WAV
target/wf-presets/audition/audition --trace 76 [--phrase] [--gain DB]   # telemetry every 50 ms
target/wf-presets/audition/audition --library PATH  # audition another library file
```

Each preset is rendered at 48 kHz through `pixl::processor::Processor` (engine + effect chain,
exactly as in the plugin) with category material: leads and basses play a melody with 30 ms
legato overlaps, arps a held triad and a held note, pads two four-note chords, drums hits across
the keyboard, SFX single notes held 1 s. Reported per preset:

- `gain` stored output gain; `pk1` worst single-note peak at full velocity; `pkP` phrase peak;
  `rms` phrase RMS; `lufs` integrated loudness (sustained categories) or `lufsM` maximum
  momentary loudness (drums, SFX); `wid` side/mid ratio; `cent` spectral centroid. Levels are
  measured at a fixed −24 dB output gain and referred to the stored gain (the chain is linear
  up to the output gain), so the safety clipper never biases them; `pkP > 0` means the phrase
  would drive the clipper.
- `pitch c` median deviation (octave-folded, after sweep/vibrato settle, unison collapsed) ±
  spread; `att`/`dec` envelope timing of a held probe note; `tail` time to −60 dB after the
  last note-off, `NO` when the engine never reports silence.
- One column per macro: the most prominent change between the macro at 0 % and 100 % (others at
  their stored positions): `lvl` level dB, `cen` centroid %, `wid` width, `dur` audible length ms,
  `pit` pitch c, `vib` pitch wobble c, `mod` pitch-modulation difference from the telemetry c,
  `dst`/`d95` mean / 95th-percentile spectro-temporal distance dB. A `!` marks a negligible
  macro (every measure below its threshold).

Targets (the tool exits 1 when one fails): finite output, no output-guard recovery, audible
(peak > −30 dBFS), single note ≤ −1 dBFS, phrase ≤ 0 dBFS before the clipper, pitched leads,
basses and pads within ±15 c, tails that end, every macro audible, and loudness spread ≤ 6 LU per
category (sustained categories also ≤ 6 dB phrase RMS). `--calibrate` sets each gain to its
category's loudness target (014's loudness, offsets in `CATEGORY_OFFSETS`), bounded by the peak
limits (single note −1.5 dBFS, phrase −1 dBFS).

Findings on 014 that follow from its SPEC-fixed init state (its gain is the parameter default,
its macros are `factory_default()`) are printed as `~~ known` and do not fail the run: at
velocity 127 its GB wave peaks above −1 dBFS (the analog DAC's note-on step through the 28 Hz
output filter, plus unison), and its SPACE macro is inaudible (it drives `echo_mix` with the
echo off and a unison width that GB hard panning already saturates). Both need an engine or
product decision, not a library change.

## Rebuilding the library after engine changes

Gains depend on the engine and effect chain, so recalibrate whenever their levels change. One
pass is exact: every level is measured at the fixed reference gain, so the suggested gain does
not depend on the gain currently stored.

```sh
CARGO_TARGET_DIR=target/wf-presets cargo build --profile audition --manifest-path tools/presets/audition/Cargo.toml
target/wf-presets/audition/audition --calibrate   # measure, write tools/presets/levels.json
python3 tools/presets/generate.py                  # bake the gains into presets/factory.json
target/wf-presets/audition/audition                # must report no problems (exit status 0)
CARGO_TARGET_DIR=target/wf-presets cargo test -p pixl-core --release --test factory_library
CARGO_TARGET_DIR=target/wf-presets cargo test -p pixl-plugin --release --no-default-features --test render
```
