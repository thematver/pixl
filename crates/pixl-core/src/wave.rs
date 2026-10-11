//! The 32 × 4-bit wave RAM and its shape generators (as specified by the design).

/// Number of steps in the wave RAM.
pub const WAVE_LEN: usize = 32;
/// Maximum step value (4-bit).
pub const WAVE_MAX: u8 = 15;

/// Wave RAM contents: 32 steps, each 0..=15.
pub type WaveRam = [u8; WAVE_LEN];

fn from_fn(f: impl Fn(usize) -> f64) -> WaveRam {
    let mut w = [0u8; WAVE_LEN];
    for (i, v) in w.iter_mut().enumerate() {
        *v = f(i).round().clamp(0.0, WAVE_MAX as f64) as u8;
    }
    w
}

/// The factory default wave (design: 0.62·sin + 0.38·sign(sin(2t + 0.4))).
pub fn default_wave() -> WaveRam {
    from_fn(|i| {
        let t = i as f64 / WAVE_LEN as f64 * std::f64::consts::TAU;
        let s = (2.0 * t + 0.4).sin();
        let sign = if s > 0.0 {
            1.0
        } else if s < 0.0 {
            -1.0
        } else {
            0.0
        };
        7.5 + 7.5 * (0.62 * t.sin() + 0.38 * sign)
    })
}

/// SIN preset.
pub fn sine() -> WaveRam {
    from_fn(|i| 7.5 + 7.5 * (i as f64 / WAVE_LEN as f64 * std::f64::consts::TAU).sin())
}

/// TRI preset.
pub fn triangle() -> WaveRam {
    from_fn(|i| if i < 16 { i as f64 } else { (31 - i) as f64 })
}

/// SAW preset.
pub fn saw() -> WaveRam {
    from_fn(|i| i as f64 / 2.0)
}

/// SQR preset.
pub fn square() -> WaveRam {
    from_fn(|i| if i < 16 { 15.0 } else { 0.0 })
}

/// SMOOTH tool: circular [1 2 1]/4 filter.
pub fn smooth(w: &WaveRam) -> WaveRam {
    from_fn(|i| {
        let a = w[(i + WAVE_LEN - 1) % WAVE_LEN] as f64;
        let b = w[i] as f64;
        let c = w[(i + 1) % WAVE_LEN] as f64;
        (a + 2.0 * b + c) / 4.0
    })
}

/// INVERT tool.
pub fn invert(w: &WaveRam) -> WaveRam {
    let mut out = *w;
    for v in out.iter_mut() {
        *v = WAVE_MAX - (*v).min(WAVE_MAX);
    }
    out
}

/// The fixed NES 2A03 triangle sequence (15..0, 0..15).
pub const NES_TRIANGLE: WaveRam =
    [15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Clamp arbitrary data into a valid wave RAM (values > 15 clamp; short input pads with 0).
pub fn sanitize(data: &[u8]) -> WaveRam {
    let mut w = [0u8; WAVE_LEN];
    for (dst, src) in w.iter_mut().zip(data.iter()) {
        *dst = (*src).min(WAVE_MAX);
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_in_range() {
        for w in [default_wave(), sine(), triangle(), saw(), square()] {
            assert!(w.iter().all(|v| *v <= WAVE_MAX));
        }
        assert_eq!(square()[0], 15);
        assert_eq!(square()[31], 0);
        assert_eq!(triangle()[15], 15);
        assert_eq!(invert(&square())[0], 0);
        assert_eq!(saw()[31], 16u8.min(WAVE_MAX)); // 15.5 rounds to 16 → clamped 15
    }

    #[test]
    fn default_wave_matches_design() {
        // First steps of the design's defWave(): computed by the reference formula.
        let w = default_wave();
        assert_eq!(&w[..4], &[10, 11, 12, 13]);
    }
}
