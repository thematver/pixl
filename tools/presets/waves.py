"""Wave RAM shapes for the factory library (32 steps, 4-bit values 0..15).

The first few mirror the generators in crates/pixl-core/src/wave.rs exactly (same formulas,
same rounding), the rest are additive "drawbar" designs normalized to the full 0..15 range so
the 4-bit RAM's resolution is used completely.
"""

import math

from pixl_params import round_half_away

WAVE_LEN = 32
WAVE_MAX = 15


def _from_fn(f):
    return [int(min(max(round_half_away(f(i)), 0), WAVE_MAX)) for i in range(WAVE_LEN)]


def _harmonics(partials, phases=None):
    """Additive wave: partials {harmonic: amplitude}, scaled so min -> 0 and max -> 15."""
    phases = phases or {}
    raw = []
    for i in range(WAVE_LEN):
        t = 2.0 * math.pi * i / WAVE_LEN
        raw.append(sum(a * math.sin(k * t + phases.get(k, 0.0)) for k, a in partials.items()))
    lo, hi = min(raw), max(raw)
    return _from_fn(lambda i: (raw[i] - lo) / (hi - lo) * WAVE_MAX)


# --- pixl-core shapes (wave.rs) ---------------------------------------------------------

SINE = _from_fn(lambda i: 7.5 + 7.5 * math.sin(i / WAVE_LEN * 2.0 * math.pi))
TRI = _from_fn(lambda i: float(i) if i < 16 else float(31 - i))
SAW = _from_fn(lambda i: i / 2.0)
SQUARE = _from_fn(lambda i: 15.0 if i < 16 else 0.0)

# --- designed shapes ----------------------------------------------------------------------

# Rounded sine with a touch of 2nd/3rd harmonic: warm, slightly vocal.
SOFT = _harmonics({1: 1.0, 2: 0.25, 3: 0.1})
# Almost pure: flute / whistle.
FLUTE = _harmonics({1: 1.0, 2: 0.15, 3: 0.05})
# Odd harmonics only: hollow, clarinet-like.
HOLLOW = _harmonics({1: 1.0, 3: 0.4, 5: 0.22, 7: 0.1})
# Strong 2nd/3rd: nasal double-reed.
REED = _harmonics({1: 0.55, 2: 1.0, 3: 0.75, 4: 0.45, 5: 0.3, 6: 0.18})
# Formant-ish "aah".
VOX_A = _harmonics({1: 1.0, 2: 0.75, 3: 0.9, 4: 0.35, 5: 0.2, 6: 0.12})
# Darker formant "ooh".
VOX_O = _harmonics({1: 1.0, 2: 0.6, 3: 0.25, 4: 0.08})
# Drawbars 8' 4' 2 2/3' 2' 1 1/3'.
ORGAN = _harmonics({1: 1.0, 2: 0.55, 3: 0.3, 4: 0.25, 6: 0.12})
# Fundamental plus bright upper partials: glassy.
GLASS = _harmonics({1: 1.0, 3: 0.35, 4: 0.45, 7: 0.2})
# Bell-like partial stack.
BELL = _harmonics({1: 1.0, 3: 0.5, 5: 0.35, 8: 0.25})
# Dark, smooth saw-ish (the "lavender" colour).
DARK = _harmonics({1: 1.0, 2: 0.35, 3: 0.12})
# Sub bass: sine with a little 2nd harmonic so it reads on small speakers.
SUB = _harmonics({1: 1.0, 2: 0.12})
# Band-limited saw: full but not fizzy.
BASS_SAW = _harmonics({k: 1.0 / k for k in range(1, 9)})
# Saw + square blend: growl.
GROWL = _from_fn(lambda i: 0.6 * (i / 2.0) + 0.4 * (15.0 if i < 16 else 0.0))
# Notched square: chunky, "stepped" bass.
CHUNK = [15] * 10 + [9] * 6 + [0] * 10 + [6] * 6
# Half-sine hump: fundamental + even harmonics, bubbly.
HALF_SINE = _from_fn(lambda i: 15.0 * math.sin(math.pi * i / WAVE_LEN))
# Tom body: sine with a 2nd harmonic knock.
TOM = _harmonics({1: 1.0, 2: 0.3})

ALL = {
    "SINE": SINE,
    "TRI": TRI,
    "SAW": SAW,
    "SQUARE": SQUARE,
    "SOFT": SOFT,
    "FLUTE": FLUTE,
    "HOLLOW": HOLLOW,
    "REED": REED,
    "VOX_A": VOX_A,
    "VOX_O": VOX_O,
    "ORGAN": ORGAN,
    "GLASS": GLASS,
    "BELL": BELL,
    "DARK": DARK,
    "SUB": SUB,
    "BASS_SAW": BASS_SAW,
    "GROWL": GROWL,
    "CHUNK": CHUNK,
    "HALF_SINE": HALF_SINE,
    "TOM": TOM,
}

for _name, _w in ALL.items():
    assert len(_w) == WAVE_LEN and all(0 <= v <= WAVE_MAX for v in _w), _name
    assert max(_w) - min(_w) >= 12, f"{_name} does not use the 4-bit range"
