"""Sound-design vocabulary for the factory library.

A preset is written as a list of *parts* (channel and effect blocks) merged over a neutral
baseline, plus four macro routing lists. Conventions:

* Every factory preset stores **every** parameter (self-describing, immune to future default
  changes). The neutral baseline has all channels and effects switched off; a part switches
  its block on and sets all of the block's values.
* Macro routings are written as ``(key, value_at_100)``: the preset's stored parameter value is
  what the target does with that macro at 0 %, ``value_at_100`` is where the macro takes it at
  100 %. The generator converts this into the normalized bipolar depth that pixl-core stores
  (``depth = norm(value_at_100) - norm(base)``), so depths always match the target's range,
  skew and the sound the designer heard. What the preset sounds like on load is the base moved
  by every macro at its stored position (``macro1..4``, default 62/30/45/20); the ``--report``
  option of generate.py prints those effective values.
* DIRT is stored at 0 % on every factory preset: the preset loads exactly as designed (a
  crusher wired for DIRT sits at its base, transparent 16 bit / 48 kHz unless the preset is
  crushed by design) and turning DIRT up adds the grit. A preset written with DIRT at another
  position (the 30 % default included) is *rebased*: every DIRT target other than the crusher
  moves to the value it had at that position (so detune and noise sound the same on load) and
  the stored DIRT becomes 0. Only the init preset 014 keeps the parameter defaults (DIRT 30 %,
  which there only adds unison detune: its crusher is off).
"""

import math

import waves
from pixl_params import KEYS, SPECS, apply_macros, denormalize, normalize, sanitize, to_plain

CATEGORY_RANGES = {
    "LEAD": (1, 32),
    "BASS": (33, 56),
    "ARP": (57, 74),
    "PAD": (75, 86),
    "DRUM": (87, 108),
    "SFX": (109, 128),
}
ICON_TAG = {"p12": "PULSE", "p50": "PULSE", "tri": "TRI", "wav": "WAVE", "noi": "NOISE"}
AUTHOR = "PIXL Factory"
DEFAULT_MACRO_VALUES = (62, 30, 45, 20)
DIRT = 1  # macro index
# DIRT targets that are not rebased (the crusher must sit at its base when DIRT is at 0).
DIRT_CRUSHER_KEYS = ("crush_bits", "crush_rate")

# ------------------------------------------------------------------------------------------
# Parts
# ------------------------------------------------------------------------------------------


def pulse(n, level, duty, a=0, d=0, s=15, r=60, pan=0, coarse=0, fine=0):
    """Pulse channel `n` (1 or 2). Times in ms, level/sustain 0..15, duty in %."""
    p = "p%d" % n
    return {
        p + "_on": True,
        p + "_level": level,
        p + "_pan": pan,
        p + "_coarse": coarse,
        p + "_fine": fine,
        p + "_attack": a,
        p + "_decay": d,
        p + "_sustain": s,
        p + "_release": r,
        p + "_duty": duty,
    }


def wave(level=15, a=0, d=0, s=15, r=60, pan=0, coarse=0, fine=0):
    """Wave channel (GB/FREE wave RAM, NES triangle)."""
    return {
        "wv_on": True,
        "wv_level": level,
        "wv_pan": pan,
        "wv_coarse": coarse,
        "wv_fine": fine,
        "wv_attack": a,
        "wv_decay": d,
        "wv_sustain": s,
        "wv_release": r,
    }


def noise(level, period, mode="LONG", a=0, d=0, s=0, r=30, track=False, pan=0, coarse=0, fine=0):
    """Noise channel: period index 0 (brightest) .. 15, LFSR mode LONG/SHORT."""
    return {
        "ns_on": True,
        "ns_level": level,
        "ns_pan": pan,
        "ns_coarse": coarse,
        "ns_fine": fine,
        "ns_attack": a,
        "ns_decay": d,
        "ns_sustain": s,
        "ns_release": r,
        "ns_mode": mode,
        "ns_period": period,
        "ns_track": track,
    }


def sweep(amt, time, direction="DOWN", curve="EXP"):
    return {"sweep_on": True, "sweep_amt": amt, "sweep_dir": direction, "sweep_time": time, "sweep_curve": curve}


def vib(rate, depth, delay=0, fade=0, shape="SINE"):
    return {
        "vib_on": True,
        "vib_rate": rate,
        "vib_depth": depth,
        "vib_delay": delay,
        "vib_fade": fade,
        "vib_shape": shape,
    }


def arp(chord, rate=20, mode="UP", octaves=1, sync=False, div="1/16"):
    return {
        "arp_on": True,
        "arp_mode": mode,
        "arp_rate": rate,
        "arp_sync": sync,
        "arp_div": div,
        "arp_oct": octaves,
        "arp_chord": chord,
    }


def crush(bits=16, rate=48000):
    """Bitcrusher. The default (16 bit, 48 kHz) is transparent: a DIRT macro drives it."""
    return {"crush_on": True, "crush_bits": bits, "crush_rate": rate}


def glide(time, mode="LEGATO", curve="EXP"):
    return {"glide_on": True, "glide_time": time, "glide_mode": mode, "glide_curve": curve}


def unison(voices, detune, width):
    return {"uni_on": True, "uni_voices": voices, "uni_detune": detune, "uni_width": width}


def echo(mix, fb, fir="WARM", div="1/8D", width=50, sync=True, time=250):
    return {
        "echo_on": True,
        "echo_time": time,
        "echo_sync": sync,
        "echo_div": div,
        "echo_fb": fb,
        "echo_mix": mix,
        "echo_fir": fir,
        "echo_width": width,
    }


def voice(mode="POLY", poly=8):
    return {"voice_mode": mode, "polyphony": poly}


# Ready-made DIRT routings for the crush() part (value at 100 %).
def dirt_crush(bits=4, rate=3500):
    return [("crush_bits", bits), ("crush_rate", rate)]


# ------------------------------------------------------------------------------------------
# Baseline
# ------------------------------------------------------------------------------------------


def neutral_baseline():
    """Parameter defaults with every channel and effect switched off (plain JSON values)."""
    base = {}
    for spec in SPECS.values():
        if spec.kind == "bool":
            base[spec.key] = spec.default >= 0.5
        elif spec.kind == "enum":
            base[spec.key] = spec.variants[int(spec.default)]
        elif spec.kind == "int":
            base[spec.key] = int(spec.default)
        else:
            base[spec.key] = spec.default
    for key in (
        "p1_on",
        "p2_on",
        "wv_on",
        "ns_on",
        "sweep_on",
        "vib_on",
        "arp_on",
        "crush_on",
        "glide_on",
        "uni_on",
        "echo_on",
    ):
        base[key] = False
    return base


# ------------------------------------------------------------------------------------------
# Preset model
# ------------------------------------------------------------------------------------------


class Preset:
    """One factory preset. `init=True` marks the init preset (014), stored as `params: {}`."""

    def __init__(
        self,
        number,
        name,
        category,
        chip,
        icon,
        ru,
        en,
        parts=(),
        m1=(),
        m2=(),
        m3=(),
        m4=(),
        wave_ram=None,
        mv=DEFAULT_MACRO_VALUES,
        gain=None,
        vel=50,
        pb=2,
        analog=True,
        frame="NTSC",
        init=False,
    ):
        self.number = number
        self.name = name
        self.category = category
        self.chip = chip
        self.icon = icon
        self.ru = ru
        self.en = en
        self.init = init
        self.tags = [chip, category, ICON_TAG[icon]]
        if init:
            self.params = {}
            self.wave = None
            self.macro_specs = None
            self.routings = []  # filled by resolve()
            return
        params = neutral_baseline()
        explicit = set()
        for part in [{"chip": chip, "vel_sens": vel, "pb_range": pb, "analog": analog, "frame_rate": frame}] + list(
            parts
        ):
            for key, value in part.items():
                if key not in SPECS:
                    raise ValueError("%03d %s: unknown parameter %r" % (number, name, key))
                if key in explicit:
                    raise ValueError("%03d %s: parameter %r set twice" % (number, name, key))
                explicit.add(key)
                params[key] = value
        for i, value in enumerate(mv):
            params["macro%d" % (i + 1)] = value
        if "voice_mode" not in explicit:
            params["voice_mode"] = "POLY"
        self.params = params
        self.gain_override = gain
        if wave_ram is None:
            # NES presets keep a triangle in wave RAM so switching the chip keeps the timbre.
            wave_ram = waves.TRI if chip == "NES" else waves.SOFT
        self.wave = list(wave_ram)
        self.macro_specs = [list(m1), list(m2), list(m3), list(m4)]
        self.routings = []
        self._rebase_dirt()

    def _rebase_dirt(self):
        """Store DIRT at 0 % without changing what its non-crusher targets do on load."""
        position = sanitize("macro2", to_plain("macro2", self.params["macro2"])) / 100.0
        if position <= 0.0:
            return
        for key, target in self.macro_specs[DIRT]:
            if key in DIRT_CRUSHER_KEYS or key not in SPECS:
                continue
            base = sanitize(key, to_plain(key, self.params[key]))
            to = sanitize(key, to_plain(key, target))
            norm = normalize(key, base) + position * (normalize(key, to) - normalize(key, base))
            value = denormalize(key, norm)
            self.params[key] = round(value, 1) if SPECS[key].kind == "float" else int(value)
        self.params["macro2"] = 0

    # -- resolution ------------------------------------------------------------------------

    def base_plain(self):
        """Plain float values of the stored parameters (defaults for the init preset)."""
        out = {k: SPECS[k].default for k in KEYS}
        for key, value in self.params.items():
            out[key] = sanitize(key, to_plain(key, value))
        return out

    def macro_values(self):
        base = self.base_plain()
        return [base["macro%d" % (i + 1)] / 100.0 for i in range(4)]

    def resolve(self):
        """Compute macro depths from (key, value_at_100) specs, then the output gain."""
        from pixl_params import FACTORY_DEFAULT_MACROS

        if self.init:
            self.routings = [list(m) for m in FACTORY_DEFAULT_MACROS]
            return
        base = self.base_plain()
        self.routings = []
        for m, specs in enumerate(self.macro_specs):
            slots = []
            for key, target in specs:
                if key not in SPECS:
                    raise ValueError("%03d %s: macro %d targets unknown %r" % (self.number, self.name, m + 1, key))
                to = sanitize(key, to_plain(key, target))
                depth = round(normalize(key, to) - normalize(key, base[key]), 4)
                slots.append((key, depth))
            self.routings.append(slots)
        if self.gain_override is not None:
            self.params["out_gain"] = self.gain_override
        else:
            self.params["out_gain"] = auto_gain(self)

    def effective(self, macro_values=None):
        """Effective plain values with the macros at `macro_values` (default: stored)."""
        mv = self.macro_values() if macro_values is None else macro_values
        return apply_macros(self.base_plain(), mv, self.routings)


# ------------------------------------------------------------------------------------------
# Analysis used by validation and the auto gain
# ------------------------------------------------------------------------------------------

CHIP_DUTIES = (12.5, 25.0, 50.0, 75.0)
CHIP_NAMES = ("NES", "GB", "FREE")


def snap_duty(duty):
    return min(CHIP_DUTIES, key=lambda d: abs(d - duty))


def gb_pan_zone(pan):
    return -1 if pan < -33.0 else (1 if pan > 33.0 else 0)


def module_enabled(v, key):
    """Whether a macro target can be heard given the (non-modulatable) switches in `v`."""
    chip = int(v["chip"])  # 0 NES, 1 GB, 2 FREE
    for prefix in ("p1", "p2", "wv", "ns"):
        if key.startswith(prefix + "_"):
            if v[prefix + "_on"] < 0.5:
                return False
            if key == "wv_level" and chip == 0:
                return False  # NES triangle has no volume control
            return True
    if key in ("sweep_amt", "sweep_time"):
        return v["sweep_on"] >= 0.5
    if key.startswith("vib_"):
        return v["vib_on"] >= 0.5
    if key == "arp_rate":
        return v["arp_on"] >= 0.5 and v["arp_sync"] < 0.5
    if key == "arp_oct":
        return v["arp_on"] >= 0.5
    if key.startswith("crush_"):
        return v["crush_on"] >= 0.5
    if key == "glide_time":
        return v["glide_on"] >= 0.5
    if key == "uni_voices":
        return v["uni_on"] >= 0.5
    if key in ("uni_detune", "uni_width"):
        return v["uni_on"] >= 0.5 and v["uni_voices"] >= 2
    if key == "echo_time":
        return v["echo_on"] >= 0.5 and v["echo_sync"] < 0.5
    if key.startswith("echo_"):
        return v["echo_on"] >= 0.5
    return True


def audible_difference(v, key, a, b):
    """Whether two values of `key` sound different (chip snapping taken into account)."""
    chip = int(v["chip"])
    if key in ("p1_duty", "p2_duty") and chip != 2:
        return snap_duty(a) != snap_duty(b)
    if key.endswith("_pan") and chip == 1:
        return gb_pan_zone(a) != gb_pan_zone(b)
    return abs(a - b) > 1e-3


def routing_is_live(preset, m, key):
    """Moving macro `m` from 0 to 100 % audibly changes `key`."""
    mv = preset.macro_values()
    lo = list(mv)
    hi = list(mv)
    lo[m] = 0.0
    hi[m] = 1.0
    v_lo = preset.effective(lo)
    v_hi = preset.effective(hi)
    return module_enabled(v_lo, key) and audible_difference(v_lo, key, v_lo[key], v_hi[key])


def channel_amplitudes(v):
    """Per-channel peak amplitude 0..1 at full velocity and envelope peak (chip quantized)."""
    chip = int(v["chip"])
    amps = {}
    for ch in ("p1", "p2", "wv", "ns"):
        if v[ch + "_on"] < 0.5:
            continue
        lvl = v[ch + "_level"] / 15.0
        if ch == "wv" and chip == 0:
            lvl = 1.0
        elif ch == "wv" and chip == 1:
            lvl = 1.0 if lvl >= 0.75 else 0.5 if lvl >= 0.375 else 0.25 if lvl >= 0.125 else 0.0
        elif chip != 2:
            lvl = round(lvl * 15.0) / 15.0
        amps[ch] = lvl
    return amps


def estimated_peak(v):
    """Rough worst-case single-note peak (linear, 1.0 = 0 dBFS) before the output gain.

    Mirrors SPEC §4.3 calibration: one channel at level 15 ~ 0.5 peak; NES uses the 2A03
    mixer formulas normalized so a lone pulse or triangle at 15 also gives 0.5; unison
    sub-voices are scaled 1/sqrt(n) but can align, so the worst case grows by sqrt(n).
    """
    amps = channel_amplitudes(v)
    chip = int(v["chip"])
    if chip == 0:
        p = 15.0 * (amps.get("p1", 0.0) + amps.get("p2", 0.0))
        pulse_out = 95.88 / (8128.0 / p + 100.0) if p > 0 else 0.0
        t = 15.0 * amps.get("wv", 0.0)
        n = 15.0 * amps.get("ns", 0.0)
        tnd = 159.79 / (1.0 / (t / 8227.0 + n / 12241.0) + 100.0) if (t + n) > 0 else 0.0
        single_pulse = 95.88 / (8128.0 / 15.0 + 100.0)
        single_tri = 159.79 / (1.0 / (15.0 / 8227.0) + 100.0)
        peak = 0.5 * pulse_out / single_pulse + 0.5 * tnd / single_tri
    else:
        peak = 0.5 * sum(amps.values())
    if v["uni_on"] >= 0.5:
        peak *= math.sqrt(max(1.0, v["uni_voices"]))
    if v["echo_on"] >= 0.5:
        peak *= 1.0 + 0.6 * v["echo_mix"] / 100.0
    return peak


# Target single-note peak after the output gain. Polyphonic melodic presets leave room for
# chords (the output stage's soft clipper protects, but should rarely have to).
GAIN_TARGET = {"LEAD": 0.6, "BASS": 0.63, "ARP": 0.56, "PAD": 0.45, "DRUM": 0.7, "SFX": 0.6}
POLY_TARGET = 0.5


def auto_gain(preset):
    v = preset.effective()
    peak = estimated_peak(v)
    if peak <= 0.0:
        return 0
    target = GAIN_TARGET[preset.category]
    held_arp = v["arp_on"] >= 0.5 and int(v["arp_chord"]) == 0  # HELD collapses to one voice
    if int(v["voice_mode"]) == 0 and preset.category in ("LEAD", "ARP") and not held_arp:
        target = min(target, POLY_TARGET)
    gain = 20.0 * math.log10(target / peak)
    gain = max(-12.0, min(4.0, gain))
    return round(gain * 2.0) / 2.0
