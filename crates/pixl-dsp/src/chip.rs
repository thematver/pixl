//! Chip constants, tables and register models of the Ricoh 2A03 (NES) and the Sharp DMG
//! (Game Boy) as used by PIXL (SPEC §4.3).
//!
//! Everything here is a pure function or constant so that the GUI, tests and tools can reuse
//! the exact quantization the voices apply.

/// NES CPU clock, NTSC (Hz).
pub const NES_CPU_NTSC: f64 = 1_789_773.0;
/// NES CPU clock, PAL (Hz).
pub const NES_CPU_PAL: f64 = 1_662_607.0;
/// NES frame rate, NTSC (Hz).
pub const NES_FRAME_NTSC: f64 = 60.0988;
/// NES frame rate, PAL (Hz).
pub const NES_FRAME_PAL: f64 = 50.007;
/// Game Boy master clock (Hz).
pub const GB_CLOCK: f64 = 4_194_304.0;
/// Game Boy frame rate (Hz).
pub const GB_FRAME_HZ: f64 = 59.7275;

/// Lowest NES pulse timer value that is not silenced by the sweep unit.
pub const NES_PULSE_MIN_TIMER: u16 = 8;
/// Lowest NES triangle timer value that is still clocked (below: ultrasonic, step held).
pub const NES_TRIANGLE_MIN_TIMER: u16 = 2;
/// Largest 11-bit timer / frequency register value.
pub const REG_MAX: u16 = 2047;

/// NES pulse duty sequences (output bit per step) for 12.5 / 25 / 50 / 75 %.
pub const NES_DUTY: [[u8; 8]; 4] =
    [[0, 1, 0, 0, 0, 0, 0, 0], [0, 1, 1, 0, 0, 0, 0, 0], [0, 1, 1, 1, 1, 0, 0, 0], [1, 0, 0, 1, 1, 1, 1, 1]];
/// Game Boy pulse duty sequences for 12.5 / 25 / 50 / 75 %.
pub const GB_DUTY: [[u8; 8]; 4] =
    [[0, 0, 0, 0, 0, 0, 0, 1], [1, 0, 0, 0, 0, 0, 0, 1], [1, 0, 0, 0, 0, 1, 1, 1], [0, 1, 1, 1, 1, 1, 1, 0]];

/// NES noise LFSR clock periods in CPU cycles (NTSC), index 0 = brightest.
pub const NES_NOISE_NTSC: [u16; 16] = [4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068];
/// NES noise LFSR clock periods in CPU cycles (PAL), index 0 = brightest.
pub const NES_NOISE_PAL: [u16; 16] = [4, 8, 14, 30, 60, 88, 118, 148, 188, 236, 354, 472, 708, 944, 1890, 3778];

/// Game Boy noise (divisor code `r`, clock shift `s`) pairs, index 0 = brightest.
///
/// The LFSR clock is `524288 / r / 2^(s+1)` Hz with `r = 0.5` for code 0. The 16 entries are
/// the strictly decreasing best fit (least squares in octaves) to a half-octave ladder from
/// 262 144 Hz down to ≈ 1.45 kHz using only register values the DMG can express; the top of the
/// range can only step in coarser intervals because no register pair lies between 262 144 and
/// 131 072 Hz.
pub const GB_NOISE: [(u8, u8); 16] = [
    (1, 0),
    (1, 1),
    (3, 0),
    (1, 2),
    (5, 0),
    (3, 1),
    (1, 3),
    (3, 2),
    (1, 4),
    (3, 3),
    (1, 5),
    (3, 4),
    (1, 6),
    (3, 5),
    (1, 7),
    (3, 6),
];

/// Brightest FREE-mode noise clock (Hz); index `x` maps to `FREE_NOISE_TOP · 2^(−x/2)`.
pub const FREE_NOISE_TOP: f64 = 262_144.0;

/// Rising / falling edge positions (fractions of a cycle) of the NES duty sequences.
///
/// Every chip duty sequence contains exactly one 0→1 and one 1→0 transition per cycle, so a
/// pulse is fully described by the cycle positions of these two edges (verified against
/// [`NES_DUTY`] in the tests). The output is high on `[rise, fall)` (cyclically).
pub const NES_EDGES: [(f64, f64); 4] = [(0.125, 0.25), (0.125, 0.375), (0.125, 0.625), (0.375, 0.125)];
/// Rising / falling edge positions of the Game Boy duty sequences (see [`NES_EDGES`]).
pub const GB_EDGES: [(f64, f64); 4] = [(0.875, 0.0), (0.875, 0.125), (0.625, 0.125), (0.125, 0.875)];

/// Edge positions `(rise, fall)` of an arbitrary 8-step duty sequence, or `None` when the
/// sequence does not have exactly one rising and one falling edge.
pub fn sequence_edges(seq: &[u8; 8]) -> Option<(f64, f64)> {
    let mut rise = None;
    let mut fall = None;
    for i in 0..8 {
        let prev = seq[(i + 7) % 8] != 0;
        let cur = seq[i] != 0;
        match (prev, cur) {
            (false, true) if rise.is_none() => rise = Some(i as f64 / 8.0),
            (true, false) if fall.is_none() => fall = Some(i as f64 / 8.0),
            (false, true) | (true, false) => return None,
            _ => {}
        }
    }
    Some((rise?, fall?))
}

/// Fraction of the cycle a pulse with edges `(rise, fall)` is high.
#[inline]
pub fn edges_duty(rise: f64, fall: f64) -> f64 {
    let d = fall - rise;
    if d <= 0.0 {
        d + 1.0
    } else {
        d
    }
}

/// Equal-power pan law normalised to unity at the centre: `pan` −100 (L) … +100 (R) →
/// `(√2·cos θ, √2·sin θ)` with `θ = (pan + 100)/200 · π/2`. A centred channel keeps its
/// calibrated level on both sides; a hard-panned one is +3 dB on its side (constant power).
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let p = if pan.is_finite() { pan.clamp(-100.0, 100.0) } else { 0.0 };
    let theta = (p + 100.0) / 200.0 * std::f32::consts::FRAC_PI_2;
    let (s, c) = theta.sin_cos();
    (std::f32::consts::SQRT_2 * c, std::f32::consts::SQRT_2 * s)
}

/// Game Boy NR51 panning: a channel is routed left, right or to both sides. |pan| > 33 is hard.
#[inline]
pub fn gb_quantize_pan(pan: f32) -> f32 {
    if !pan.is_finite() || pan.abs() <= 33.0 {
        0.0
    } else {
        100.0f32.copysign(pan)
    }
}

/// NES CPU clock for a frame-rate standard.
#[inline]
pub fn nes_cpu(pal: bool) -> f64 {
    if pal {
        NES_CPU_PAL
    } else {
        NES_CPU_NTSC
    }
}

/// Index of the chip duty (12.5 / 25 / 50 / 75 %) nearest to `percent`.
pub fn duty_index(percent: f32) -> usize {
    let p = if percent.is_finite() { percent } else { 50.0 };
    if p < 18.75 {
        0
    } else if p < 37.5 {
        1
    } else if p < 62.5 {
        2
    } else {
        3
    }
}

/// Duty fraction of a chip duty index.
pub fn duty_fraction(index: usize) -> f32 {
    [0.125, 0.25, 0.5, 0.75][index.min(3)]
}

#[inline]
fn sane_freq(freq: f64) -> f64 {
    if freq.is_finite() && freq > 1e-3 {
        freq
    } else {
        1e-3
    }
}

#[inline]
fn clamp_reg(v: f64) -> u16 {
    if v.is_finite() {
        v.round().clamp(0.0, REG_MAX as f64) as u16
    } else {
        REG_MAX
    }
}

/// NES pulse timer for a target frequency: `t = round(CPU/(16 f) − 1)`, clamped to 0..2047.
pub fn nes_pulse_timer(freq: f64, cpu: f64) -> u16 {
    clamp_reg(cpu / (16.0 * sane_freq(freq)) - 1.0)
}

/// Frequency produced by NES pulse timer `t`.
pub fn nes_pulse_freq(t: u16, cpu: f64) -> f64 {
    cpu / (16.0 * (t as f64 + 1.0))
}

/// NES triangle timer for a target frequency: `t = round(CPU/(32 f) − 1)`, clamped to 0..2047.
pub fn nes_triangle_timer(freq: f64, cpu: f64) -> u16 {
    clamp_reg(cpu / (32.0 * sane_freq(freq)) - 1.0)
}

/// Frequency produced by NES triangle timer `t`.
pub fn nes_triangle_freq(t: u16, cpu: f64) -> f64 {
    cpu / (32.0 * (t as f64 + 1.0))
}

/// Game Boy pulse frequency register: `x = round(2048 − 131072/f)`, clamped to 0..2047.
pub fn gb_pulse_reg(freq: f64) -> u16 {
    clamp_reg(2048.0 - 131072.0 / sane_freq(freq))
}

/// Frequency produced by Game Boy pulse register `x`.
pub fn gb_pulse_freq(x: u16) -> f64 {
    131072.0 / (2048.0 - x.min(REG_MAX) as f64)
}

/// Game Boy wave frequency register: `x = round(2048 − 65536/f)`, clamped to 0..2047.
pub fn gb_wave_reg(freq: f64) -> u16 {
    clamp_reg(2048.0 - 65536.0 / sane_freq(freq))
}

/// Frequency produced by Game Boy wave register `x`.
pub fn gb_wave_freq(x: u16) -> f64 {
    65536.0 / (2048.0 - x.min(REG_MAX) as f64)
}

/// Game Boy noise LFSR clock (Hz) of table index `i` (clamped to 0..15).
pub fn gb_noise_clock(i: usize) -> f64 {
    let (code, s) = GB_NOISE[i.min(15)];
    let r = if code == 0 { 0.5 } else { code as f64 };
    524_288.0 / r / f64::from(1u32 << (s + 1))
}

/// NES noise LFSR clock (Hz) of table index `i` (clamped to 0..15).
pub fn nes_noise_clock(i: usize, pal: bool) -> f64 {
    let (cpu, table) = if pal { (NES_CPU_PAL, &NES_NOISE_PAL) } else { (NES_CPU_NTSC, &NES_NOISE_NTSC) };
    cpu / table[i.min(15)] as f64
}

/// FREE-mode noise clock (Hz) of a continuous index `x` (clamped to 0..15).
pub fn free_noise_clock(x: f64) -> f64 {
    let x = if x.is_finite() { x.clamp(0.0, 15.0) } else { 0.0 };
    FREE_NOISE_TOP * (-x / 2.0).exp2()
}

/// One NES noise LFSR clock: feedback `bit0 ^ bit1` (long) or `bit0 ^ bit6` (short), shift
/// right, feedback into bit 14.
#[inline]
pub fn nes_lfsr_step(lfsr: u16, short: bool) -> u16 {
    let tap = if short { 6 } else { 1 };
    let fb = (lfsr ^ (lfsr >> tap)) & 1;
    ((lfsr >> 1) | (fb << 14)) & 0x7fff
}

/// One Game Boy noise LFSR clock: `xor = bit0 ^ bit1`, shift right with `xor` into bit 14;
/// the short (7-bit) mode also writes `xor` into bit 6.
#[inline]
pub fn gb_lfsr_step(lfsr: u16, short: bool) -> u16 {
    let x = (lfsr ^ (lfsr >> 1)) & 1;
    let mut l = (lfsr >> 1) | (x << 14);
    if short {
        l = (l & !(1 << 6)) | (x << 6);
    }
    l & 0x7fff
}

/// 2A03 pulse group output for DAC levels `p1 + p2` (0..30).
#[inline]
pub fn nes_pulse_mix(sum: f32) -> f32 {
    if sum > 0.0 {
        95.88 / (8128.0 / sum + 100.0)
    } else {
        0.0
    }
}

/// 2A03 triangle/noise group output for the weighted sum `t/8227 + n/12241`.
#[inline]
pub fn nes_tnd_mix(weighted: f32) -> f32 {
    if weighted > 0.0 {
        159.79 / (1.0 / weighted + 100.0)
    } else {
        0.0
    }
}

/// Output scale of the NES mixer chosen so that one pulse channel at level 15 swings exactly
/// 1.0 (−6 dBFS peak after DC removal for a 50 % duty), matching GB and FREE calibration.
pub fn nes_mix_scale() -> f32 {
    1.0 / nes_pulse_mix(15.0)
}

/// Single-channel full-level swing of the NES triangle relative to a pulse (2A03 mixer ratio,
/// ≈ 1.65: equal RMS to a full-level 50 % pulse).
pub fn nes_triangle_weight() -> f32 {
    nes_mix_scale() * nes_tnd_mix(15.0 / 8227.0)
}

/// Single-channel full-level swing of the NES noise relative to a pulse (≈ 1.17).
pub fn nes_noise_weight() -> f32 {
    nes_mix_scale() * nes_tnd_mix(15.0 / 12241.0)
}

/// Game Boy wave-channel volume shift for an effective level `e` (0..1): 100 % (shift 0) at
/// `e ≥ 0.75`, 50 % (1) at `≥ 0.375`, 25 % (2) at `≥ 0.125`, otherwise muted (`None`).
pub fn gb_wave_shift(e: f32) -> Option<u8> {
    if e >= 0.75 {
        Some(0)
    } else if e >= 0.375 {
        Some(1)
    } else if e >= 0.125 {
        Some(2)
    } else {
        None
    }
}

/// Frequency (Hz) of a MIDI pitch in semitones (A4 = 69 = 440 Hz).
#[inline]
pub fn pitch_to_freq(pitch: f64) -> f64 {
    440.0 * ((pitch - 69.0) / 12.0).exp2()
}

/// Number of LFSR clocks until `state` repeats (cycle length), or `None` if `state` is not on a
/// cycle within `limit` steps.
pub fn lfsr_period(state: u16, step: impl Fn(u16) -> u16, limit: usize) -> Option<usize> {
    let mut s = step(state);
    for n in 1..=limit {
        if s == state {
            return Some(n);
        }
        s = step(s);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nes_register_quantization() {
        // A4 → t = 253, f ≈ 440.4 Hz (SPEC §4.11).
        let t = nes_pulse_timer(440.0, NES_CPU_NTSC);
        assert_eq!(t, 253);
        let f = nes_pulse_freq(t, NES_CPU_NTSC);
        assert!((f - 440.4).abs() < 0.05, "{f}");
        // Triangle plays an octave lower per timer value: A4 → t = 126.
        let tt = nes_triangle_timer(440.0, NES_CPU_NTSC);
        assert_eq!(tt, 126);
        assert!((nes_triangle_freq(tt, NES_CPU_NTSC) - 440.4).abs() < 0.05);
        // Below the 11-bit range: clamps to 2047 (lowest playable note).
        assert_eq!(nes_pulse_timer(20.0, NES_CPU_NTSC), 2047);
        assert!((nes_pulse_freq(2047, NES_CPU_NTSC) - 54.6).abs() < 0.1);
        // Very high notes hit the sweep-unit mute region (t < 8 ⇔ f > CPU/(16·8.5) ≈ 13.16 kHz).
        assert_eq!(nes_pulse_timer(13000.0, NES_CPU_NTSC), 8);
        assert!(nes_pulse_timer(13300.0, NES_CPU_NTSC) < NES_PULSE_MIN_TIMER);
        // PAL clock.
        assert_eq!(nes_pulse_timer(440.0, NES_CPU_PAL), 235);
        // Garbage input never panics.
        assert_eq!(nes_pulse_timer(f64::NAN, NES_CPU_NTSC), 2047);
        assert_eq!(nes_pulse_timer(-5.0, NES_CPU_NTSC), 2047);
    }

    #[test]
    fn gb_register_quantization() {
        let x = gb_pulse_reg(440.0);
        assert_eq!(x, 1750);
        assert!((gb_pulse_freq(x) - 439.8).abs() < 0.1);
        let w = gb_wave_reg(440.0);
        assert_eq!(w, 1899);
        assert!((gb_wave_freq(w) - 439.8).abs() < 0.2);
        assert_eq!(gb_pulse_reg(30.0), 0);
        assert!((gb_pulse_freq(0) - 64.0).abs() < 1e-9);
        assert_eq!(gb_pulse_reg(1e9), 2047);
        assert!(gb_pulse_freq(2047).is_finite());
    }

    #[test]
    fn lfsr_periods() {
        assert_eq!(lfsr_period(0x7fff, |s| nes_lfsr_step(s, false), 40000), Some(32767));
        assert_eq!(lfsr_period(0x7fff, |s| gb_lfsr_step(s, false), 40000), Some(32767));
        // GB 7-bit mode: 127 steps.
        let mut s = 0x7fff;
        for _ in 0..200 {
            s = gb_lfsr_step(s, true);
        }
        assert_eq!(lfsr_period(s, |s| gb_lfsr_step(s, true), 40000), Some(127));
        // NES short mode: 93 from the power-on state; 31 for some other states.
        let mut s = 0x7fff;
        for _ in 0..1000 {
            s = nes_lfsr_step(s, true);
        }
        assert_eq!(lfsr_period(s, |s| nes_lfsr_step(s, true), 40000), Some(93));
        let mut seen31 = false;
        for seed in 1..0x7fffu16 {
            let mut s = seed;
            for _ in 0..200 {
                s = nes_lfsr_step(s, true);
            }
            match lfsr_period(s, |s| nes_lfsr_step(s, true), 1000) {
                Some(93) => {}
                Some(31) => seen31 = true,
                other => {
                    // The only other cycle of the short-mode register is the all-zero lock-up.
                    assert!(s == 0 || other == Some(1), "seed {seed:#x}: {other:?}");
                }
            }
        }
        assert!(seen31);
    }

    #[test]
    fn duty_edges_match_sequences() {
        for i in 0..4 {
            assert_eq!(sequence_edges(&NES_DUTY[i]), Some(NES_EDGES[i]), "NES duty {i}");
            assert_eq!(sequence_edges(&GB_DUTY[i]), Some(GB_EDGES[i]), "GB duty {i}");
            let (r, f) = NES_EDGES[i];
            assert!((edges_duty(r, f) - duty_fraction(i) as f64).abs() < 1e-12);
            let (r, f) = GB_EDGES[i];
            assert!((edges_duty(r, f) - duty_fraction(i) as f64).abs() < 1e-12);
        }
        assert_eq!(sequence_edges(&[1, 0, 1, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn pan_law() {
        let (l, r) = pan_gains(0.0);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
        let (l, r) = pan_gains(-100.0);
        assert!((l - std::f32::consts::SQRT_2).abs() < 1e-6 && r.abs() < 1e-6);
        for p in [-80.0f32, -30.0, 10.0, 55.0, 100.0] {
            let (l, r) = pan_gains(p);
            assert!((l * l + r * r - 2.0).abs() < 1e-5, "constant power at {p}");
        }
        assert_eq!(pan_gains(f32::NAN), (pan_gains(0.0).0, pan_gains(0.0).1));
        assert_eq!(gb_quantize_pan(20.0), 0.0);
        assert_eq!(gb_quantize_pan(-40.0), -100.0);
        assert_eq!(gb_quantize_pan(34.0), 100.0);
    }

    #[test]
    fn duty_sequences() {
        let ones = |t: &[[u8; 8]; 4]| t.map(|row| row.iter().filter(|b| **b == 1).count());
        assert_eq!(ones(&NES_DUTY), [1, 2, 4, 6]);
        assert_eq!(ones(&GB_DUTY), [1, 2, 4, 6]);
        assert_eq!(NES_DUTY[3], [1, 0, 0, 1, 1, 1, 1, 1]);
        assert_eq!(GB_DUTY[2], [1, 0, 0, 0, 0, 1, 1, 1]);
        assert_eq!(duty_index(2.0), 0);
        assert_eq!(duty_index(20.0), 1);
        assert_eq!(duty_index(50.0), 2);
        assert_eq!(duty_index(98.0), 3);
        assert_eq!(duty_index(f32::NAN), 2);
    }

    #[test]
    fn noise_tables() {
        let gb: Vec<f64> = (0..16).map(gb_noise_clock).collect();
        assert_eq!(gb[0], 262_144.0);
        assert!((1300.0..1600.0).contains(&gb[15]), "{}", gb[15]);
        assert!(gb.windows(2).all(|w| w[1] < w[0]), "strictly decreasing");
        assert!((nes_noise_clock(0, false) - 447_443.25).abs() < 1.0);
        assert_eq!(free_noise_clock(0.0), FREE_NOISE_TOP);
        assert!((free_noise_clock(15.0) - 1448.15).abs() < 0.1);
    }

    #[test]
    fn nes_mixer_calibration() {
        let k = nes_mix_scale();
        assert!((k * nes_pulse_mix(15.0) - 1.0).abs() < 1e-6);
        assert_eq!(nes_pulse_mix(0.0), 0.0);
        assert_eq!(nes_tnd_mix(0.0), 0.0);
        // Two pulses at 15 compress (non-linear mixer): less than twice one pulse.
        assert!(nes_pulse_mix(30.0) < 2.0 * nes_pulse_mix(15.0) * 0.9);
        assert!((nes_triangle_weight() - 1.649).abs() < 0.01, "{}", nes_triangle_weight());
        assert!((nes_noise_weight() - 1.167).abs() < 0.01, "{}", nes_noise_weight());
    }

    #[test]
    fn gb_wave_volume_shift() {
        assert_eq!(gb_wave_shift(1.0), Some(0));
        assert_eq!(gb_wave_shift(0.75), Some(0));
        assert_eq!(gb_wave_shift(0.5), Some(1));
        assert_eq!(gb_wave_shift(0.2), Some(2));
        assert_eq!(gb_wave_shift(0.1), None);
    }
}
