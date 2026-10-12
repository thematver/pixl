# PIXL AU / Logic integration harness

A standalone Objective-C++ AUv2 host using AudioToolbox and CoreFoundation. It discovers the **registered** `aumu/PIXL/PXLa` component through AudioComponentFindNext; it does not load the Rust library or simulate the wrapper. Requires macOS, Xcode Command Line Tools, and an already installed/registered PIXL AU matching the current implementation. No audio device, dependencies, installs, or Logic launch are needed.

From the repository root:

```sh
clang++ -std=c++17 -O2 -Wall -Wextra tools/logic-integration/au_host.mm \
  -framework AudioToolbox -framework CoreFoundation \
  -o tools/logic-integration/au_host
mkdir -p tools/logic-integration/results
tools/logic-integration/au_host tools/logic-integration/results
```

The optional argument is an existing artifact directory. It receives binary-plist `state.aupreset` and `parameters.tsv`. Without it, no result files are written. Exit 0 means all checks passed; exit 1 prints the failing operation and OSStatus where applicable. A missing registration is a failure, never a skipped success. The registered component can differ from the checkout; its version is printed for traceability. Run with an architecture matching the installed component (use `-arch arm64` or `-arch x86_64` when needed).

The harness also selects all 128 native AU factory presets, verifies PresentPreset numbers/names and saved class-info names, checks native custom setting names, and rejects wrong plist types, foreign component IDs and truncated state chunks. Full factory parameter/wave/macro equality and preservation of instance UI preferences are covered by the Rust CLAP host test.

Checks are bounded and deterministic: 48 kHz, stereo noninterleaved float, 256-frame blocks, no random input. The harness enumerates exactly 91 parameters, prints numeric IDs/names/ranges, and compares IDs, names, ranges and flags across independent instances. This establishes instance stability; save and compare TSV files across plugin versions to establish release stability. It exercises Output set/get and an immediate automation event scheduled at frame 64, then checks the post-render value. It does not claim to prove sample-accurate automation timing or ramp support.

A controlled FREE-mode pulse patch disables modulation, unison, other channels and echo. MIDI note-on and note-off use nonzero frame offsets. Audio must be finite and audible; zero-crossing measurements must show approximately A4, an octave up at full pitch bend with a 12-semitone range, and return to center. Sustain must keep a released note audible and pedal release must reach silence after settling. This verifies audible MIDI behavior rather than merely successful API calls; exact MIDI onset timing is not asserted.

The AU's continuous host controls are normalized, while stepped controls use step units (the nih-plug CLAP mapping forwarded by clap-wrapper); they are not the core table's plain engineering values. The patch setup intentionally uses host range fractions. State is saved through AU ClassInfo, serialized to binary plist, decoded, restored into a separately initialized instance with changed values, and all 91 parameter values compared with range-relative tolerance. Restored audio must retain the expected pitch. Non-parameter wave/macros/UI state travels in the opaque class-info chunk but is not independently inspected by this harness.

## Remaining Logic acceptance checks

Passing this executable verifies the actual AU boundary, not full Logic integration. In Logic Pro, insert PIXL on a software-instrument track; verify editor open/close, resizing, keyboard focus and playback. Record/read automation from both Logic controls and plugin gestures (including undo), and confirm pitch wheel, sustain and note offsets during playback. Save a project with changed parameters, custom wave RAM/macro routes and UI settings; close/reopen Logic and verify recall, including a bounced phrase. Check offline bounce and transport/tempo-synced arp/echo. Those behaviors require Logic and its project/GUI lifecycle and cannot be certified by this headless host.
