#!/usr/bin/env python3
"""Build, validate and report the PIXL factory library (SPEC §9).

    python3 tools/presets/generate.py            validate library.py and write presets/factory.json
    python3 tools/presets/generate.py --check    validate only; exit status 1 if anything is wrong
                                                 or presets/factory.json is out of date
    python3 tools/presets/generate.py --report   validate and print, per preset, the effective values
                                                 the engine plays on load (base values moved by every
                                                 macro at its stored position) and every routing at
                                                 0 % | stored | 100 % (writes nothing)
    python3 tools/presets/generate.py --report 14 57-60   report only these preset numbers
    python3 tools/presets/generate.py --out PATH          write the library to PATH instead (experiments;
                                                          param_table.txt is left alone)

Python 3.9, standard library only. The output is deterministic: presets in number order,
parameters in table order (crates/pixl-core/src/params.rs), floats rounded to at most four
decimals, one parameter group per line so diffs stay readable.

The generator also writes tools/presets/param_table.txt, a canonical listing of its mirror
of the parameter table (pixl_params.py); crates/pixl-core/tests/factory_library.rs rebuilds
the listing from the Rust table and fails if the two differ, so the mirror cannot go stale.

Output levels: every preset's `out_gain` comes from tools/presets/levels.json, which the
audition tool (tools/presets/audition, `--calibrate`) writes from loudness measurements of the
real engine. A preset that is missing from that file falls back to a rough analytic estimate
(dsl.auto_gain) and the run says so.
"""

import argparse
import ast
import json
import math
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import dsl  # noqa: E402
import library  # noqa: E402
import waves  # noqa: E402
from pixl_params import table_listing  # noqa: E402
from pixl_params import (  # noqa: E402
    FACTORY_DEFAULT_MACROS,
    KEYS,
    MACRO_COUNT,
    MACRO_NAMES,
    MACRO_SLOTS,
    MIN_DEPTH,
    PARAMS,
    SPECS,
    normalize,
    sanitize,
    to_plain,
)

ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
OUT_PATH = os.path.join(ROOT, "presets", "factory.json")
LEVELS_PATH = os.path.join(HERE, "levels.json")
TABLE_PATH = os.path.join(HERE, "param_table.txt")
DESIGN_PATH = os.path.join(ROOT, "docs", "design", "source", "PIXL.dc.html")

LIBRARY_FORMAT = "pixl-library"
PRESET_FORMAT = "pixl-preset"
FORMAT_VERSION = 1
INIT_NUMBER = 14
CATEGORY_COUNTS = {"LEAD": 32, "BASS": 24, "ARP": 18, "PAD": 12, "DRUM": 22, "SFX": 20}
ICONS = ("p12", "p50", "tri", "wav", "noi")
MAX_NAME_LEN = 18
MAX_DESCRIPTION_LEN = 120

# The design's twelve (docs/design/source/PIXL.dc.html, `PR` in renderVals): name, category,
# chip, icon, Russian description. Used when the design file is not available; when it is,
# the file is parsed and must agree with this copy.
DESIGN_TWELVE = [
    ("Hyrule Fanfare", "LEAD", "GB", "wav",
     "Яркий лид на самодельной волне, лёгкий sweep вниз на атаке и вибрато с задержкой."),
    ("Overworld 1-1", "LEAD", "NES", "p50", "Квадрат 50% без эффектов — классика платформеров."),
    ("Ocarina Lead", "LEAD", "GB", "tri", "Мягкий треугольник, медленное вибрато, длинный глайд."),
    ("Pocket Arp", "ARP", "GB", "p12", "Узкий пульс 12.5% для быстрых арпеджио."),
    ("Castle Bass", "BASS", "NES", "tri", "Треугольный бас NES, без громкости — только высота."),
    ("Coin Square", "SFX", "NES", "p12", "Короткий скачок вверх на кварту."),
    ("Lavender Wave", "PAD", "GB", "wav", "Тёмная волна, 3 голоса унисона."),
    ("Power-Up", "SFX", "NES", "p50", "Восходящий sweep на октаву."),
    ("Static Snare", "DRUM", "NES", "noi", "Длинный LFSR, быстрый спад."),
    ("Dungeon Drone", "PAD", "GB", "wav", "Низкая волна с crush 4 бита."),
    ("Kick 8", "DRUM", "NES", "noi", "Короткий шум с резким sweep вниз."),
    ("Rupee Blip", "SFX", "GB", "p12", "Две ноты с быстрым глайдом."),
]
DESIGN_NAMES = {d[0] for d in DESIGN_TWELVE}

# Third-party trademarks and franchise vocabulary that must not appear in preset names
# (whole words, case-insensitive). The design's twelve are exempt (SPEC §9).
FORBIDDEN_NAME_WORDS = [
    "nintendo", "famicom", "nes", "snes", "gameboy", "game boy", "sega", "atari", "konami", "capcom",
    "namco", "taito", "hudson", "square enix", "zelda", "hyrule", "link", "ganon", "triforce", "ocarina",
    "rupee", "mario", "luigi", "bowser", "koopa", "goomba", "yoshi", "toad", "peach", "kirby", "metroid",
    "samus", "pokemon", "pokémon", "pikachu", "sonic", "megaman", "mega man", "rockman", "castlevania",
    "belmont", "contra", "tetris", "pac-man", "pacman", "galaga", "donkey kong", "final fantasy",
    "chocobo", "moogle", "dragon quest", "earthbound", "kid icarus", "star fox", "bomberman", "excitebike",
    "punch-out", "duck hunt", "ninja gaiden", "street fighter", "space invaders", "dig dug", "lemmings",
    "overworld", "1-1", "warp zone", "8-bit", "8 bit", "chiptune", "roland", "808", "909",
]
# Words that must not appear in any description either.
FORBIDDEN_TEXT_WORDS = [w for w in FORBIDDEN_NAME_WORDS if w not in ("nes", "snes", "8-bit", "8 bit", "chiptune")]


class LibraryError(Exception):
    pass


# ------------------------------------------------------------------------------------------
# Number formatting and JSON emission
# ------------------------------------------------------------------------------------------


def fmt_number(x):
    """Shortest stable decimal for a float rounded to four places ("12", "12.5", "-0.3333")."""
    if isinstance(x, bool):
        raise TypeError("bool is not a number here")
    r = round(float(x), 4)
    if r == 0.0:
        return "0"
    if abs(r - round(r)) < 1e-9:
        return str(int(round(r)))
    s = "%.4f" % r
    return s.rstrip("0").rstrip(".")


def json_value(key, plain):
    """Plain value of `key` -> its JSON text in a preset file."""
    spec = SPECS[key]
    if spec.kind == "bool":
        return "true" if plain >= 0.5 else "false"
    if spec.kind == "enum":
        return json.dumps(spec.variants[int(plain)])
    if spec.kind == "int":
        return str(int(plain))
    return fmt_number(plain)


def jstr(s):
    return json.dumps(s, ensure_ascii=False)


def emit_library(presets):
    """The library document as text (2-space indentation, one parameter group per line)."""
    out = ['{', '  "format": %s,' % jstr(LIBRARY_FORMAT), '  "version": %d,' % FORMAT_VERSION, '  "presets": [']
    for i, p in enumerate(presets):
        out.extend(emit_preset(p, last=(i == len(presets) - 1)))
    out.append("  ]")
    out.append("}")
    return "\n".join(out) + "\n"


def emit_preset(p, last):
    ind = "      "
    lines = ["    {"]
    lines.append(ind + '"format": %s,' % jstr(PRESET_FORMAT))
    lines.append(ind + '"version": %d,' % FORMAT_VERSION)
    lines.append(ind + '"name": %s,' % jstr(p.name))
    lines.append(ind + '"category": %s,' % jstr(p.category))
    lines.append(ind + '"number": %d,' % p.number)
    lines.append(ind + '"icon": %s,' % jstr(p.icon))
    lines.append(ind + '"tags": [%s],' % ", ".join(jstr(t) for t in p.tags))
    lines.append(ind + '"description": {')
    lines.append(ind + '  "ru": %s,' % jstr(p.ru))
    lines.append(ind + '  "en": %s' % jstr(p.en))
    lines.append(ind + "},")
    lines.append(ind + '"author": %s,' % jstr(dsl.AUTHOR))
    if p.init:
        lines.append(ind + '"params": {},')
    else:
        base = p.base_plain()
        groups = []
        for spec in PARAMS:
            if not groups or groups[-1][0] != spec.group:
                groups.append((spec.group, []))
            groups[-1][1].append('"%s": %s' % (spec.key, json_value(spec.key, base[spec.key])))
        lines.append(ind + '"params": {')
        for gi, (_, items) in enumerate(groups):
            sep = "," if gi < len(groups) - 1 else ""
            lines.append(ind + "  " + ", ".join(items) + sep)
        lines.append(ind + "},")
        lines.append(ind + '"wave": [%s],' % ", ".join(str(v) for v in p.wave))
    macro_lines = []
    for m in range(MACRO_COUNT):
        slots = p.routings[m]
        body = ", ".join('{"target": %s, "depth": %s}' % (jstr(k), fmt_number(d)) for k, d in slots)
        macro_lines.append(ind + "  [" + body + "]" + ("," if m < MACRO_COUNT - 1 else ""))
    lines.append(ind + '"macros": [')
    lines.extend(macro_lines)
    lines.append(ind + "]")
    lines.append("    }" + ("" if last else ","))
    return lines


# ------------------------------------------------------------------------------------------
# Levels (written by the audition tool)
# ------------------------------------------------------------------------------------------


def load_levels():
    if not os.path.exists(LEVELS_PATH):
        return {}
    with open(LEVELS_PATH, encoding="utf-8") as f:
        doc = json.load(f)
    gains = doc.get("out_gain", {})
    return {int(k): float(v) for k, v in gains.items()}


# ------------------------------------------------------------------------------------------
# Resolution
# ------------------------------------------------------------------------------------------


def build(levels):
    """Resolve every preset (macro depths, output gain). Returns (presets, notices)."""
    notices = []
    presets = sorted(library.PRESETS, key=lambda p: p.number)
    estimated = []
    for p in presets:
        p.resolve()
        if p.init:
            continue
        if p.gain_override is not None:
            continue
        if p.number in levels:
            gain = levels[p.number]
            spec = SPECS["out_gain"]
            if not spec.lo <= gain <= spec.hi:
                raise LibraryError("%03d %s: levels.json gain %.2f dB out of range" % (p.number, p.name, gain))
            p.params["out_gain"] = round(gain, 1)
        else:
            estimated.append(p.number)
    if estimated:
        notices.append(
            "output gain estimated (not calibrated by the audition tool) for %d preset(s): %s"
            % (len(estimated), ", ".join("%03d" % n for n in estimated))
        )
    return presets, notices


# ------------------------------------------------------------------------------------------
# Validation
# ------------------------------------------------------------------------------------------


def design_twelve():
    """The design's twelve, parsed from PIXL.dc.html when present (must match the copy)."""
    if not os.path.exists(DESIGN_PATH):
        return DESIGN_TWELVE
    with open(DESIGN_PATH, encoding="utf-8") as f:
        html = f.read()
    m = re.search(r"const PR = (\[\[.*?\]\]);", html)
    if not m:
        raise LibraryError("could not find the PR array in %s" % DESIGN_PATH)
    parsed = [tuple(x) for x in ast.literal_eval(m.group(1))]
    if parsed != DESIGN_TWELVE:
        raise LibraryError("generate.py's DESIGN_TWELVE is out of date with %s" % DESIGN_PATH)
    return parsed


def sentence_count(text):
    return len(re.findall(r"[.!?…](?=\s|$)", text.strip()))


def word_present(text, word):
    pattern = r"(?<![\w-])" + re.escape(word) + r"(?![\w-])"
    return re.search(pattern, text, flags=re.IGNORECASE) is not None


def wave_rms(chip, wave_ram):
    """RMS of the wave channel's waveform relative to a full-scale square (1.0)."""
    if chip == 0:
        return 1.0 / math.sqrt(3.0)  # the NES triangle
    mean = sum(wave_ram) / len(wave_ram)
    return math.sqrt(sum((x - mean) ** 2 for x in wave_ram) / len(wave_ram)) / 7.5


def envelope_mean(attack, decay, sustain, span=1000.0):
    """Mean ADSR level (0..1) over the first `span` ms of a held note (linear segments)."""
    t, acc = 0.0, 0.0
    a = min(attack, span)
    acc += 0.5 * a
    t += a
    if t < span:
        d = min(decay, span - t)
        frac = d / decay if decay > 0 else 1.0
        end = 1.0 - (1.0 - sustain) * frac
        acc += 0.5 * (1.0 + end) * d
        t += d
    if t < span:
        acc += sustain * (span - t)
    return acc / span


def main_channel(v, wave_ram):
    """(channel, icon) the preset is heard as: the enabled channel with the most energy at
    load (level × waveform RMS × mean envelope over the first second; pulses and noise have
    the RMS of a square)."""
    amps = dsl.channel_amplitudes(v)
    if not amps:
        return None, None
    chip = int(v["chip"])
    if "wv" in amps:
        amps["wv"] *= wave_rms(chip, wave_ram)
    for ch in amps:
        amps[ch] *= envelope_mean(v[ch + "_attack"], v[ch + "_decay"], v[ch + "_sustain"] / 15.0)
    order = ["p1", "p2", "wv", "ns"]
    best = max(order, key=lambda ch: (amps.get(ch, -1.0), -order.index(ch)))
    if best in ("p1", "p2"):
        duty = v[best + "_duty"]
        if chip != 2:
            duty = dsl.snap_duty(duty)
            return best, ("p50" if duty == 50.0 else "p12")
        return best, ("p50" if abs(duty - 50.0) < 13.0 else "p12")
    if best == "wv":
        return best, ("tri" if chip == 0 or list(wave_ram) == waves.TRI else "wav")
    return best, "noi"


DEPENDENCIES = {
    # target: (condition on effective values, explanation)
    "sweep_time": (lambda v: v["sweep_on"] >= 0.5 and v["sweep_amt"] > 0.0, "sweep is off or its amount is 0"),
    "sweep_amt": (lambda v: v["sweep_on"] >= 0.5, "sweep is off"),
    "vib_rate": (lambda v: v["vib_on"] >= 0.5 and v["vib_depth"] > 0.0, "vibrato is off or its depth is 0"),
    "vib_delay": (lambda v: v["vib_on"] >= 0.5 and v["vib_depth"] > 0.0, "vibrato is off or its depth is 0"),
    "vib_fade": (lambda v: v["vib_on"] >= 0.5 and v["vib_depth"] > 0.0, "vibrato is off or its depth is 0"),
    "echo_fb": (lambda v: v["echo_on"] >= 0.5 and v["echo_mix"] > 0.0, "echo is off or its mix is 0"),
    "echo_width": (lambda v: v["echo_on"] >= 0.5 and v["echo_mix"] > 0.0, "echo is off or its mix is 0"),
    "echo_time": (
        lambda v: v["echo_on"] >= 0.5 and v["echo_mix"] > 0.0 and v["echo_sync"] < 0.5,
        "echo is off, silent or tempo-synced",
    ),
}


def channel_audible(v, prefix):
    if v[prefix + "_on"] < 0.5:
        return False
    if prefix == "wv" and int(v["chip"]) == 0:
        return True  # NES triangle ignores the level
    return v[prefix + "_level"] > 0.0


# Lowest frequency the chip's pulse channels can play (11-bit timer / frequency register at
# its limit, SPEC §4.3) and the corresponding MIDI note (fractional).
PULSE_FLOOR_HZ = {0: 1789773.0 / (16.0 * 2048.0), 1: 131072.0 / 2048.0}


def hz_to_note(f):
    return 69.0 + 12.0 * math.log2(f / 440.0)


# Lowest note each category is expected to be played at (the audition's test material).
CATEGORY_LOW_NOTE = {"BASS": 36, "LEAD": 55, "ARP": 48, "PAD": 45, "DRUM": 36, "SFX": 48}


def pulse_range_problems(p, v):
    """Pulse channels pitched below what NES/GB pulses can play over the category's range."""
    chip = int(v["chip"])
    if chip not in PULSE_FLOOR_HZ:
        return []
    floor = hz_to_note(PULSE_FLOOR_HZ[chip])
    low = CATEGORY_LOW_NOTE[p.category]
    out = []
    for ch in ("p1", "p2"):
        if not channel_audible(v, ch):
            continue
        note = low + v[ch + "_coarse"]
        if note < floor - 0.05:
            out.append("%s plays note %d at the category's lowest note %d, below the %s pulse floor "
                       "(%.1f Hz, note %.1f): the chip clamps it to a wrong pitch"
                       % (ch, note, low, dsl.CHIP_NAMES[chip], PULSE_FLOOR_HZ[chip], floor))
    return out


# The note each category is auditioned at (tools/presets/audition: the probe note).
CATEGORY_PROBE_NOTE = {"LEAD": 72, "BASS": 40, "ARP": 60, "PAD": 60, "DRUM": 48, "SFX": 72}


def tracked_noise_problems(p, v):
    """Key-tracked noise whose period index clamps at the category's typical note.

    With `ns_track` the index is `ns_period - (note + ns_coarse - 60)` (SPEC §4.3); if that is
    outside 0..15 where the preset is played, the noise neither tracks the keyboard nor
    responds to its period (macro routings on it are dead). `ns_coarse` recentres it.
    """
    if not channel_audible(v, "ns") or v["ns_track"] < 0.5:
        return []
    note = CATEGORY_PROBE_NOTE[p.category]
    index = v["ns_period"] - (note + v["ns_coarse"] - 60)
    if -0.5 <= index <= 15.5:
        return []
    return ["tracked noise index %.0f at note %d is clamped to %d: shift it with ns_coarse"
            % (index, note, 0 if index < 0 else 15)]


def gb_pan_pattern(v, width=None):
    """GB hard-panning zones (L/C/R per channel and unison layer) for unison width `width`."""
    w = v["uni_width"] if width is None else width
    n = int(v["uni_voices"]) if v["uni_on"] >= 0.5 else 1
    lin = [(-1.0 + 2.0 * i / (n - 1)) if n > 1 else 0.0 for i in range(n)]
    out = []
    for ch in ("p1", "p2", "wv", "ns"):
        if not channel_audible(v, ch):
            continue
        out.append(tuple(dsl.gb_pan_zone(min(max(v[ch + "_pan"] + w * x, -100.0), 100.0)) for x in lin))
    return tuple(out)


def routing_changes(v_lo, v_hi, key):
    """Whether moving `key` from v_lo[key] to v_hi[key] changes what the chip plays."""
    chip = int(v_lo["chip"])
    if chip == 1 and (key == "uni_width" or key.endswith("_pan")):
        a = dict(v_lo)
        b = dict(v_lo)
        b[key] = v_hi[key]
        return gb_pan_pattern(a) != gb_pan_pattern(b)
    return dsl.audible_difference(v_lo, key, v_lo[key], v_hi[key])


def target_audible(v, key):
    """Whether a change of `key` can be heard with the effective values `v`."""
    if not dsl.module_enabled(v, key):
        return False
    for prefix in ("p1", "p2", "wv", "ns"):
        if key.startswith(prefix + "_") and key != prefix + "_level" and not channel_audible(v, prefix):
            return False
    dep = DEPENDENCIES.get(key)
    return dep is None or dep[0](v)


def snap_hazards(p, v_pre):
    """Effective values that sit on a rounding boundary (f32 vs f64 could disagree)."""
    problems = []
    chip = int(v_pre["chip"])
    for key, value in v_pre.items():
        spec = SPECS[key]
        if spec.kind != "float":
            frac = value - math.floor(value)
            if abs(frac - 0.5) < 2e-3:
                problems.append("%s = %.4f is on a rounding boundary" % (key, value))
        elif key in ("p1_duty", "p2_duty") and chip != 2:
            for edge in (18.75, 37.5, 62.5):
                if abs(value - edge) < 0.05:
                    problems.append("%s = %.4f is on a duty snapping boundary" % (key, value))
        elif key.endswith("_pan") and chip == 1 and abs(abs(value) - 33.0) < 0.05:
            problems.append("%s = %.4f is on the GB pan boundary" % (key, value))
    if chip != 2 and v_pre["arp_on"] >= 0.5 and v_pre["arp_sync"] < 0.5:
        # Chip modes play the arp in whole frames: round(frame rate / arp_rate) (SPEC §4.6).
        frame_hz = 59.7275 if chip == 1 else (50.007 if v_pre["frame_rate"] >= 0.5 else 60.0988)
        frames = frame_hz / max(v_pre["arp_rate"], 1e-6)
        if abs(frames - math.floor(frames) - 0.5) < 0.02:
            problems.append("arp_rate = %.3f Hz is on a frame rounding boundary (%.3f frames)"
                            % (v_pre["arp_rate"], frames))
    return problems


def effective_unsnapped(p, mv):
    """Like Preset.effective but without snapping discrete parameters (boundary checks)."""
    base = p.base_plain()
    offsets = {}
    for m, slots in enumerate(p.routings):
        if mv[m] == 0.0:
            continue
        for key, depth in slots:
            offsets[key] = offsets.get(key, 0.0) + min(max(mv[m], 0.0), 1.0) * depth
    out = dict(base)
    for key, off in offsets.items():
        spec = SPECS[key]
        n = min(max(normalize(key, base[key]) + off, 0.0), 1.0)
        lin = n ** (1.0 / spec.skew) if (spec.kind == "float" and spec.skew != 1.0) else n
        out[key] = min(max(spec.lo + lin * (spec.hi - spec.lo), spec.lo), spec.hi)
    return out


def validate(presets):
    """All SPEC §9 / format invariants. Returns a list of error strings (empty = valid)."""
    errors = []

    def err(p, msg):
        errors.append("%03d %s: %s" % (p.number, p.name, msg))

    # --- library shape ---------------------------------------------------------------
    if len(presets) != 128:
        errors.append("library has %d presets, expected 128" % len(presets))
    numbers = [p.number for p in presets]
    if sorted(numbers) != list(range(1, 129)):
        missing = sorted(set(range(1, 129)) - set(numbers))
        dup = sorted({n for n in numbers if numbers.count(n) > 1})
        errors.append("numbers must be exactly 1..128 (missing %s, duplicate %s)" % (missing, dup))
    counts = {}
    for p in presets:
        counts[p.category] = counts.get(p.category, 0) + 1
        lo, hi = dsl.CATEGORY_RANGES.get(p.category, (0, -1))
        if not lo <= p.number <= hi:
            err(p, "category %s must be numbered %03d-%03d" % (p.category, lo, hi))
    for cat, n in CATEGORY_COUNTS.items():
        if counts.get(cat, 0) != n:
            errors.append("category %s has %d presets, expected %d" % (cat, counts.get(cat, 0), n))
    seen = {}
    for p in presets:
        key = p.name.strip().lower()
        if key in seen:
            err(p, "name already used by %03d" % seen[key])
        seen[key] = p.number

    # --- the design's twelve -------------------------------------------------------------
    by_name = {p.name: p for p in presets}
    for name, cat, chip, icon, ru in design_twelve():
        p = by_name.get(name)
        if p is None:
            errors.append("design preset '%s' is missing" % name)
            continue
        if (p.category, p.chip, p.icon, p.ru) != (cat, chip, icon, ru):
            err(p, "must match the design: %s / %s / %s / %r" % (cat, chip, icon, ru))
    init = [p for p in presets if p.init]
    if len(init) != 1 or init[0].number != INIT_NUMBER or init[0].name != "Hyrule Fanfare":
        errors.append("exactly one init preset, 014 'Hyrule Fanfare', must be marked init=True")
    for p in init:
        if p.params or p.wave is not None:
            err(p, "the init preset must store no params and no wave")
        if [list(m) for m in p.routings] != [list(m) for m in FACTORY_DEFAULT_MACROS]:
            err(p, "the init preset's macros must equal MacroAssignments::factory_default()")

    for p in presets:
        # --- metadata ----------------------------------------------------------------------
        if not p.name.strip() or p.name != p.name.strip():
            err(p, "name must be non-empty without surrounding spaces")
        if len(p.name) > MAX_NAME_LEN:
            err(p, "name longer than %d characters" % MAX_NAME_LEN)
        if p.name not in DESIGN_NAMES:
            for w in FORBIDDEN_NAME_WORDS:
                if word_present(p.name, w):
                    err(p, "name uses the protected or generic word %r" % w)
        if p.icon not in ICONS:
            err(p, "invalid icon %r" % p.icon)
        if p.chip not in ("NES", "GB", "FREE"):
            err(p, "invalid chip %r" % p.chip)
        if p.tags != [p.chip, p.category, dsl.ICON_TAG.get(p.icon, "?")]:
            err(p, "tags must be [CHIP, CATEGORY, WAVE-TYPE], got %r" % p.tags)
        for lang, text in (("ru", p.ru), ("en", p.en)):
            if not text.strip():
                err(p, "empty %s description" % lang)
                continue
            n = sentence_count(text)
            if not 1 <= n <= 2 or not re.search(r"[.!?…]$", text.strip()):
                err(p, "%s description must be one or two complete sentences" % lang)
            if len(text) > MAX_DESCRIPTION_LEN:
                err(p, "%s description longer than %d characters" % (lang, MAX_DESCRIPTION_LEN))
            if p.name not in DESIGN_NAMES:
                for w in FORBIDDEN_TEXT_WORDS:
                    if word_present(text, w):
                        err(p, "%s description uses the protected word %r" % (lang, w))
        if re.search(r"[А-Яа-яЁё]", p.en):
            err(p, "the English description contains Cyrillic")
        if not re.search(r"[А-Яа-яЁё]", p.ru):
            err(p, "the Russian description has no Cyrillic")
        if p.init:
            continue

        # --- parameters ---------------------------------------------------------------------
        if sorted(p.params) != sorted(KEYS):
            err(p, "must store every parameter exactly once")
        for key, value in p.params.items():
            try:
                plain = to_plain(key, value)
            except (KeyError, ValueError):
                err(p, "invalid value %r for %s" % (value, key))
                continue
            if abs(sanitize(key, plain) - plain) > 1e-4:
                err(p, "%s = %r is out of range or not snapped (would load as %r)" % (key, value, sanitize(key, plain)))
        if p.params.get("chip") != p.chip:
            err(p, "params.chip %r differs from the declared chip %s" % (p.params.get("chip"), p.chip))
        if len(p.wave) != 32 or any((not isinstance(x, int)) or x < 0 or x > 15 for x in p.wave):
            err(p, "wave RAM must be 32 integers 0..15")
        base = p.base_plain()
        if not any(base[ch + "_on"] >= 0.5 for ch in ("p1", "p2", "wv", "ns")):
            err(p, "no channel enabled")

        # --- macros ---------------------------------------------------------------------------
        mv = p.macro_values()
        v_load = p.effective()
        for m in range(MACRO_COUNT):
            slots = p.routings[m]
            label = "macro %d %s" % (m + 1, MACRO_NAMES[m])
            if not slots:
                err(p, "%s has no routing" % label)
            if len(slots) > MACRO_SLOTS:
                err(p, "%s has %d routings (max %d)" % (label, len(slots), MACRO_SLOTS))
            targets = [k for k, _ in slots]
            if len(set(targets)) != len(targets):
                err(p, "%s routes a parameter twice" % label)
            lo = list(mv)
            hi = list(mv)
            lo[m] = 0.0
            hi[m] = 1.0
            v_lo = p.effective(lo)
            v_hi = p.effective(hi)
            live = 0
            for key, depth in slots:
                spec = SPECS[key]
                if not spec.modulatable:
                    err(p, "%s targets non-modulatable %s" % (label, key))
                    continue
                if key == "out_gain":
                    err(p, "%s must not target out_gain (the audition calibrates it)" % label)
                if abs(depth) < MIN_DEPTH:
                    err(p, "%s -> %s depth %.4f is below MIN_DEPTH (pixl-core drops it)" % (label, key, depth))
                    continue
                audible = target_audible(v_lo, key) or target_audible(v_hi, key)
                changes = routing_changes(v_lo, v_hi, key)
                if audible and changes:
                    live += 1
                else:
                    err(p, "%s -> %s is a dead routing (%s)" % (label, key, "no audible change" if audible else "target inaudible"))
            if slots and live == 0:
                err(p, "%s does nothing" % label)
        for msg in snap_hazards(p, effective_unsnapped(p, mv)):
            err(p, msg)
        for msg in pulse_range_problems(p, v_load) + tracked_noise_problems(p, v_load):
            err(p, msg)

        # --- icon -------------------------------------------------------------------------------
        channel, icon = main_channel(v_load, p.wave)
        if p.name not in DESIGN_NAMES and icon != p.icon:
            err(p, "icon %r does not match the main channel %s (expected %r)" % (p.icon, channel, icon))
        family = {"p12": ("p1", "p2"), "p50": ("p1", "p2"), "tri": ("wv",), "wav": ("wv",), "noi": ("ns",)}
        if not any(channel_audible(v_load, ch) for ch in family.get(p.icon, ())):
            err(p, "icon %r but no such channel is audible" % p.icon)
        if p.chip == "NES" and p.icon == "wav":
            err(p, "NES has no wave RAM: use the 'tri' icon")
    return errors


# ------------------------------------------------------------------------------------------
# Report
# ------------------------------------------------------------------------------------------

GROUP_ORDER = ["Global", "Pulse 1", "Pulse 2", "Wave", "Noise", "Sweep", "Vibrato", "Arp", "Crush", "Glide", "Unison",
               "Echo", "Macros"]
GROUP_SWITCH = {"Pulse 1": "p1_on", "Pulse 2": "p2_on", "Wave": "wv_on", "Noise": "ns_on", "Sweep": "sweep_on",
                "Vibrato": "vib_on", "Arp": "arp_on", "Crush": "crush_on", "Glide": "glide_on", "Unison": "uni_on",
                "Echo": "echo_on"}


def display(key, plain):
    spec = SPECS[key]
    if spec.kind == "bool":
        return "on" if plain >= 0.5 else "off"
    if spec.kind == "enum":
        return spec.variants[int(plain)]
    return fmt_number(plain)


def short_label(key):
    """`p1_duty` -> `duty`, `sweep_amt` -> `amt`, `out_gain` -> `out_gain`."""
    for prefix in ("p1_", "p2_", "wv_", "ns_", "sweep_", "vib_", "arp_", "crush_", "glide_", "uni_", "echo_"):
        if key.startswith(prefix):
            return key[len(prefix):]
    return key


def report(p, out):
    mv = p.macro_values()
    base = p.base_plain()
    eff = p.effective()
    out.write("%03d %-16s %-4s %-4s icon %-3s  macros %s  out %s dB\n" % (
        p.number, p.name, p.category, p.chip, p.icon,
        "/".join(fmt_number(x * 100) for x in mv), fmt_number(base["out_gain"])))
    out.write("    ru: %s\n    en: %s\n" % (p.ru, p.en))
    for group in GROUP_ORDER:
        switch = GROUP_SWITCH.get(group)
        if switch and eff[switch] < 0.5:
            out.write("    %-8s off\n" % group)
            continue
        items = []
        for spec in PARAMS:
            if spec.group != group or spec.key == switch:
                continue
            b, e = base[spec.key], eff[spec.key]
            text = display(spec.key, e)
            if abs(b - e) > 1e-6:
                text = "%s->%s" % (display(spec.key, b), text)
            items.append("%s %s" % (short_label(spec.key), text))
        out.write("    %-8s %s\n" % (group, "  ".join(items)))
    for m in range(MACRO_COUNT):
        lo = list(mv)
        hi = list(mv)
        lo[m] = 0.0
        hi[m] = 1.0
        v_lo, v_hi = p.effective(lo), p.effective(hi)
        parts = []
        for key, depth in p.routings[m]:
            parts.append("%s %+.3f (%s | %s | %s)" % (
                key, depth, display(key, v_lo[key]), display(key, eff[key]), display(key, v_hi[key])))
        out.write("    M%d %-6s %s\n" % (m + 1, MACRO_NAMES[m], "; ".join(parts)))
    out.write("\n")


def parse_selection(items):
    sel = set()
    for item in items:
        for part in item.split(","):
            part = part.strip()
            if not part:
                continue
            if "-" in part:
                a, b = part.split("-", 1)
                sel.update(range(int(a), int(b) + 1))
            else:
                sel.add(int(part))
    return sel


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="validate only; fail if factory.json is out of date")
    ap.add_argument("--report", action="store_true", help="print effective values per preset")
    ap.add_argument("--out", metavar="PATH", help="write the library to PATH instead of presets/factory.json")
    ap.add_argument("numbers", nargs="*", help="preset numbers or ranges for --report (default: all)")
    args = ap.parse_args(argv)

    try:
        presets, notices = build(load_levels())
        errors = validate(presets)
    except LibraryError as e:
        print("error: %s" % e, file=sys.stderr)
        return 1
    if args.report:
        sel = parse_selection(args.numbers)
        for p in presets:
            if not sel or p.number in sel:
                report(p, sys.stdout)
    elif args.numbers:
        print("error: preset numbers are only meaningful with --report", file=sys.stderr)
        return 2
    for n in notices:
        print("note: %s" % n, file=sys.stderr)
    if errors:
        for e in errors:
            print("error: %s" % e, file=sys.stderr)
        print("%d error(s); %s not written" % (len(errors), os.path.relpath(OUT_PATH, ROOT)), file=sys.stderr)
        return 1
    text = emit_library(presets)
    # Round-trip sanity: the emitted text is valid JSON describing the same library.
    doc = json.loads(text)
    assert len(doc["presets"]) == len(presets)
    if args.report and not args.check:
        return 0
    table = table_listing()
    if args.check:
        stale = []
        for path, want in ((OUT_PATH, text), (TABLE_PATH, table)):
            current = open(path, encoding="utf-8").read() if os.path.exists(path) else ""
            if current != want:
                stale.append(os.path.relpath(path, ROOT))
        if stale:
            print("%s out of date: run tools/presets/generate.py" % " and ".join(stale), file=sys.stderr)
            return 1
        print("ok: %d presets valid, presets/factory.json and tools/presets/param_table.txt up to date"
              % len(presets))
        return 0
    if args.out:
        write_atomic(os.path.abspath(args.out), text)
        print("wrote %s: %d presets" % (args.out, len(presets)))
        return 0
    write_atomic(OUT_PATH, text)
    write_atomic(TABLE_PATH, table)
    print("wrote %s: %d presets, %d bytes; %s" % (os.path.relpath(OUT_PATH, ROOT), len(presets),
                                                  len(text.encode("utf-8")), os.path.relpath(TABLE_PATH, ROOT)))
    return 0


def write_atomic(path, text):
    with open(path + ".tmp", "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    os.replace(path + ".tmp", path)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
