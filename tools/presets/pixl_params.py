"""Mirror of the PIXL parameter table (crates/pixl-core/src/params.rs) for the preset generator.

Only what the generator needs: stable keys, kinds, ranges, skews, defaults and the
`modulatable` flag, plus the normalization / macro maths of `pixl-core`, reproduced exactly
so that the generator can compute macro depths and the sound a preset makes at load time.

If the Rust table changes, update this file and rerun generate.py. A stale mirror cannot go
unnoticed: generate.py writes `table_listing()` to tools/presets/param_table.txt, and
`cargo test -p pixl-core` (tests/factory_library.rs) rebuilds the same listing from
`pixl_core::PARAMS` / `pixl_core::macros` and requires the two to be identical.
"""

import math

# Enum variant labels, in index order (crates/pixl-core/src/enums.rs).
CHIP = ["NES", "GB", "FREE"]
VOICE_MODE = ["POLY", "MONO", "LEGATO"]
FRAME_RATE = ["NTSC", "PAL"]
NOISE_MODE = ["LONG", "SHORT"]
SWEEP_DIR = ["DOWN", "UP"]
CURVE = ["EXP", "LIN"]
VIB_SHAPE = ["SINE", "TRI", "SQUARE"]
ARP_MODE = ["UP", "DOWN", "UPDOWN", "RANDOM", "ORDER"]
ARP_DIV = ["1/4", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/32T", "1/64"]
ARP_CHORD = ["HELD", "MAJ", "MIN", "SUS2", "SUS4", "MAJ7", "MIN7", "DOM7", "DIM", "AUG", "POW", "OCT"]
ECHO_DIV = ["1/4", "1/8", "1/8D", "1/8T", "1/16", "1/16D", "1/16T", "1/32"]
ECHO_FIR = ["FLAT", "WARM", "DARK", "CAVE", "BRIGHT", "LOFI"]

MS_SKEW = 0.25
HZ_SKEW = 0.354


class Spec:
    """One parameter: kind is 'float' | 'int' | 'bool' | 'enum'."""

    def __init__(self, key, group, kind, default, lo=0.0, hi=1.0, skew=1.0, variants=None, modulatable=False):
        self.key = key
        self.group = group
        self.kind = kind
        self.default = default
        self.lo = lo
        self.hi = hi
        self.skew = skew
        self.variants = variants
        self.modulatable = modulatable
        if kind == "bool":
            self.lo, self.hi = 0.0, 1.0
        elif kind == "enum":
            self.lo, self.hi = 0.0, float(len(variants) - 1)


def _float(key, group, lo, hi, default, skew=1.0, mod=True):
    return Spec(key, group, "float", default, lo, hi, skew, modulatable=mod)


def _int(key, group, lo, hi, default, mod=True):
    return Spec(key, group, "int", default, lo, hi, modulatable=mod)


def _bool(key, group, default):
    return Spec(key, group, "bool", default)


def _enum(key, group, variants, default):
    return Spec(key, group, "enum", default, variants=variants)


def _channel(prefix, group, default_on):
    return [
        _bool(prefix + "_on", group, default_on),
        _float(prefix + "_level", group, 0.0, 15.0, 12.0),
        _float(prefix + "_pan", group, -100.0, 100.0, 0.0),
        _int(prefix + "_coarse", group, -24, 24, 0.0),
        _float(prefix + "_fine", group, -100.0, 100.0, 0.0),
        _float(prefix + "_attack", group, 0.0, 5000.0, 2.0, MS_SKEW),
        _float(prefix + "_decay", group, 0.0, 5000.0, 400.0, MS_SKEW),
        _float(prefix + "_sustain", group, 0.0, 15.0, 12.0),
        _float(prefix + "_release", group, 0.0, 5000.0, 150.0, MS_SKEW),
    ]


PARAMS = (
    [
        _enum("chip", "Global", CHIP, 1.0),
        _float("out_gain", "Global", -36.0, 12.0, 0.0),
        _enum("voice_mode", "Global", VOICE_MODE, 0.0),
        _int("polyphony", "Global", 1, 16, 8.0, mod=False),
        _enum("frame_rate", "Global", FRAME_RATE, 0.0),
        _int("pb_range", "Global", 0, 24, 2.0, mod=False),
        _float("vel_sens", "Global", 0.0, 100.0, 50.0),
        _float("tune", "Global", -100.0, 100.0, 0.0),
        _bool("analog", "Global", 1.0),
    ]
    + _channel("p1", "Pulse 1", 0.0)
    + [_float("p1_duty", "Pulse 1", 2.0, 98.0, 25.0)]
    + _channel("p2", "Pulse 2", 0.0)
    + [_float("p2_duty", "Pulse 2", 2.0, 98.0, 50.0)]
    + _channel("wv", "Wave", 1.0)
    + _channel("ns", "Noise", 0.0)
    + [
        _enum("ns_mode", "Noise", NOISE_MODE, 0.0),
        _int("ns_period", "Noise", 0, 15, 5.0),
        _bool("ns_track", "Noise", 0.0),
        _bool("sweep_on", "Sweep", 1.0),
        _float("sweep_amt", "Sweep", 0.0, 48.0, 12.0),
        _enum("sweep_dir", "Sweep", SWEEP_DIR, 0.0),
        _float("sweep_time", "Sweep", 5.0, 2000.0, 180.0, 0.3),
        _enum("sweep_curve", "Sweep", CURVE, 0.0),
        _bool("vib_on", "Vibrato", 1.0),
        _float("vib_rate", "Vibrato", 0.1, 20.0, 5.5, HZ_SKEW),
        _float("vib_depth", "Vibrato", 0.0, 100.0, 18.0),
        _float("vib_delay", "Vibrato", 0.0, 2000.0, 120.0, 0.5),
        _float("vib_fade", "Vibrato", 0.0, 2000.0, 150.0, 0.5),
        _enum("vib_shape", "Vibrato", VIB_SHAPE, 0.0),
        _bool("arp_on", "Arp", 0.0),
        _enum("arp_mode", "Arp", ARP_MODE, 0.0),
        _float("arp_rate", "Arp", 1.0, 60.0, 20.0, 0.5),
        _bool("arp_sync", "Arp", 0.0),
        _enum("arp_div", "Arp", ARP_DIV, 3.0),
        _int("arp_oct", "Arp", 1, 4, 1.0),
        _enum("arp_chord", "Arp", ARP_CHORD, 0.0),
        _bool("crush_on", "Crush", 0.0),
        _float("crush_bits", "Crush", 1.0, 16.0, 6.0),
        _float("crush_rate", "Crush", 1000.0, 48000.0, 11025.0, 0.3),
        _bool("glide_on", "Glide", 1.0),
        _float("glide_time", "Glide", 1.0, 2000.0, 90.0, 0.3),
        _enum("glide_mode", "Glide", ["ALWAYS", "LEGATO"], 1.0),
        _enum("glide_curve", "Glide", CURVE, 0.0),
        _bool("uni_on", "Unison", 1.0),
        _int("uni_voices", "Unison", 1, 4, 3.0),
        _float("uni_detune", "Unison", 0.0, 50.0, 14.0),
        _float("uni_width", "Unison", 0.0, 100.0, 60.0),
        _bool("echo_on", "Echo", 0.0),
        _float("echo_time", "Echo", 10.0, 1000.0, 250.0, 0.4),
        _bool("echo_sync", "Echo", 1.0),
        _enum("echo_div", "Echo", ECHO_DIV, 2.0),
        _float("echo_fb", "Echo", 0.0, 95.0, 40.0),
        _float("echo_mix", "Echo", 0.0, 100.0, 25.0),
        _enum("echo_fir", "Echo", ECHO_FIR, 1.0),
        _float("echo_width", "Echo", 0.0, 100.0, 50.0),
        _float("macro1", "Macros", 0.0, 100.0, 62.0, mod=False),
        _float("macro2", "Macros", 0.0, 100.0, 30.0, mod=False),
        _float("macro3", "Macros", 0.0, 100.0, 45.0, mod=False),
        _float("macro4", "Macros", 0.0, 100.0, 20.0, mod=False),
    ]
)

SPECS = {p.key: p for p in PARAMS}
KEYS = [p.key for p in PARAMS]
assert len(PARAMS) == 91, len(PARAMS)
assert len(SPECS) == len(PARAMS), "duplicate key in the mirror"

MACRO_COUNT = 4
MACRO_SLOTS = 8
MIN_DEPTH = 0.005
MACRO_NAMES = ["BRIGHT", "DIRT", "MOTION", "SPACE"]

# MacroAssignments::factory_default(), in slot order (order matters for equality).
FACTORY_DEFAULT_MACROS = [
    [("sweep_amt", 0.10), ("uni_detune", -0.10)],
    [("crush_bits", -0.40), ("uni_detune", 0.20)],
    [("vib_depth", 0.30), ("glide_time", 0.20)],
    [("uni_width", 0.40), ("echo_mix", 0.50), ("wv_release", 0.15)],
]


def round_half_away(x):
    """Rust's f32::round (half away from zero); Python's round() is banker's rounding."""
    return math.floor(x + 0.5) if x >= 0 else -math.floor(-x + 0.5)


def to_plain(key, value):
    """Preset JSON value (bool / enum label / number) -> plain float."""
    spec = SPECS[key]
    if isinstance(value, bool):
        return 1.0 if value else 0.0
    if spec.kind == "enum":
        if isinstance(value, str):
            return float(spec.variants.index(value))
        return float(value)
    return float(value)


def sanitize(key, plain):
    """ParamSpec::sanitize: clamp, snap discrete kinds (half away from zero); non-finite -> default."""
    spec = SPECS[key]
    if not math.isfinite(plain):
        return spec.default
    v = min(max(plain, spec.lo), spec.hi)
    return v if spec.kind == "float" else float(round_half_away(v))


def normalize(key, plain):
    """ParamSpec::normalize (nih-plug FloatRange::Skewed): ((v - min) / (max - min)) ** skew."""
    spec = SPECS[key]
    v = sanitize(key, plain)
    lin = (v - spec.lo) / (spec.hi - spec.lo)
    if spec.kind == "float" and spec.skew != 1.0:
        return lin ** spec.skew
    return lin


def denormalize(key, norm):
    """ParamSpec::denormalize: inverse skew, then sanitize (snaps Int/Enum/Bool)."""
    spec = SPECS[key]
    n = min(max(norm, 0.0), 1.0) if math.isfinite(norm) else normalize(key, spec.default)
    lin = n ** (1.0 / spec.skew) if (spec.kind == "float" and spec.skew != 1.0) else n
    return sanitize(key, spec.lo + lin * (spec.hi - spec.lo))


def apply_macros(base, macro_values, routings):
    """pixl_core::apply_macros: base plain dict, macro values 0..1, routings [[(key, depth)]]."""
    offsets = {}
    for m, slots in enumerate(routings):
        mv = macro_values[m]
        if mv == 0.0:
            continue
        for key, depth in slots:
            offsets[key] = offsets.get(key, 0.0) + min(max(mv, 0.0), 1.0) * depth
    out = dict(base)
    for key, off in offsets.items():
        out[key] = denormalize(key, normalize(key, base[key]) + off)
    return out


# ------------------------------------------------------------------------------------------
# Canonical listing (checked against the Rust table by crates/pixl-core/tests/factory_library.rs)
# ------------------------------------------------------------------------------------------


def listing_number(x):
    """Six-decimal rendering without trailing zeros, identical to the Rust test's formatter."""
    s = "%.6f" % float(x)
    s = s.rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def table_listing():
    """Every fact of the mirror the generator depends on, one per line, in table order."""
    lines = [
        "# The PIXL parameter table as mirrored by tools/presets/pixl_params.py.",
        "# Written by tools/presets/generate.py; crates/pixl-core/tests/factory_library.rs",
        "# rebuilds this listing from pixl_core::PARAMS and requires it to be identical.",
    ]
    for spec in PARAMS:
        fields = [
            "param",
            spec.key,
            spec.group.replace(" ", "_"),
            spec.kind,
            listing_number(spec.lo),
            listing_number(spec.hi),
            listing_number(spec.skew if spec.kind == "float" else 1.0),
            listing_number(spec.default),
            "mod" if spec.modulatable else "fixed",
        ]
        if spec.kind == "enum":
            fields.append("|".join(spec.variants))
        lines.append(" ".join(fields))
    lines.append("macro_count %d" % MACRO_COUNT)
    lines.append("macro_slots %d" % MACRO_SLOTS)
    lines.append("min_depth %s" % listing_number(MIN_DEPTH))
    lines.append("macro_names %s" % " ".join(MACRO_NAMES))
    for m, slots in enumerate(FACTORY_DEFAULT_MACROS):
        for key, depth in slots:
            lines.append("factory_default %d %s %s" % (m, key, listing_number(depth)))
    return "\n".join(lines) + "\n"
