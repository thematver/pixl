//! Spectral analysis helpers: a radix-2 FFT, analysis windows and small signal statistics.
//!
//! These are used at initialization time (minimum-phase kernel design) and by the test suite
//! and offline tools (alias measurements, pitch and level checks). They allocate and are
//! **not** meant for the audio thread.

use std::f64::consts::PI;

/// In-place complex FFT of `re`/`im` (radix-2, decimation in time).
///
/// `inverse = true` computes the inverse transform including the `1/n` scaling. Inputs whose
/// length is not a power of two (or whose halves differ in length) are left untouched.
pub fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    if n < 2 || !n.is_power_of_two() || im.len() != n {
        return;
    }
    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    // Twiddles computed directly (no recurrence) for full double precision.
    let sign = if inverse { 1.0 } else { -1.0 };
    let half_n = n / 2;
    let tw: Vec<(f64, f64)> = (0..half_n)
        .map(|k| {
            let a = 2.0 * PI * k as f64 / n as f64;
            (a.cos(), sign * a.sin())
        })
        .collect();
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let stride = n / len;
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let (wr, wi) = tw[k * stride];
                let a = start + k;
                let b = a + half;
                let xr = re[b] * wr - im[b] * wi;
                let xi = re[b] * wi + im[b] * wr;
                re[b] = re[a] - xr;
                im[b] = im[a] - xi;
                re[a] += xr;
                im[a] += xi;
            }
        }
        len <<= 1;
    }
    if inverse {
        let s = 1.0 / n as f64;
        for (r, i) in re.iter_mut().zip(im.iter_mut()) {
            *r *= s;
            *i *= s;
        }
    }
}

/// 4-term Blackman-Harris window (−92 dB sidelobes) of length `n` (periodic form).
pub fn blackman_harris(n: usize) -> Vec<f64> {
    const A: [f64; 4] = [0.35875, 0.48829, 0.14128, 0.01168];
    (0..n)
        .map(|i| {
            let x = 2.0 * PI * i as f64 / n as f64;
            A[0] - A[1] * x.cos() + A[2] * (2.0 * x).cos() - A[3] * (3.0 * x).cos()
        })
        .collect()
}

/// 7-term Blackman-Harris window (≈ −180 dB sidelobes, main lobe ±8 bins) of length `n`
/// (periodic form). Used for alias measurements that must see components far below −90 dB.
pub fn blackman_harris7(n: usize) -> Vec<f64> {
    const A: [f64; 7] = [
        0.271_051_400_693_42,
        0.433_297_939_234_48,
        0.218_122_999_543_11,
        0.065_925_446_388_03,
        0.010_811_742_098_37,
        0.000_776_584_825_22,
        0.000_013_887_217_35,
    ];
    (0..n)
        .map(|i| {
            let x = 2.0 * PI * i as f64 / n as f64;
            A.iter()
                .enumerate()
                .map(|(k, a)| if k % 2 == 0 { a * (k as f64 * x).cos() } else { -a * (k as f64 * x).cos() })
                .sum()
        })
        .collect()
}

/// Result of [`alias_report`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AliasReport {
    /// Level of the strongest non-harmonic component relative to the fundamental (dB).
    pub worst_db: f64,
    /// Frequency of that component (Hz).
    pub worst_hz: f64,
    /// Fundamental level (dBFS, window-corrected).
    pub fundamental_db: f64,
}

/// Measures aliasing of a periodic tone with fundamental `f0`: a 7-term Blackman-Harris FFT
/// of `signal` (power-of-two length) with all harmonic positions `h·f0 < fs/2` (and DC)
/// masked; reports the strongest remaining component relative to the fundamental.
pub fn alias_report(signal: &[f32], sample_rate: f64, f0: f64) -> AliasReport {
    alias_report_band(signal, sample_rate, f0, sample_rate / 2.0)
}

/// [`alias_report`] restricted to components below `max_hz` (e.g. the audible band).
pub fn alias_report_band(signal: &[f32], sample_rate: f64, f0: f64, max_hz: f64) -> AliasReport {
    let n = signal.len().next_power_of_two().min(1 << 20);
    let win = blackman_harris7(n);
    let coherent: f64 = win.iter().sum::<f64>() / 2.0;
    let dc = remove_dc(&signal[..n.min(signal.len())]);
    let mag = magnitude_spectrum(&dc, &win);
    let bin_hz = sample_rate / n as f64;
    const GUARD: f64 = 12.0; // bins around each masked line (main lobe ±8 + margin)
    let mut masked = vec![false; mag.len()];
    let mut mark = |f: f64| {
        let c = f / bin_hz;
        let lo = (c - GUARD).floor().max(0.0) as usize;
        let hi = ((c + GUARD).ceil() as usize).min(mag.len() - 1);
        for m in masked.iter_mut().take(hi + 1).skip(lo) {
            *m = true;
        }
    };
    mark(0.0);
    let mut h = 1.0;
    while h * f0 < sample_rate / 2.0 {
        mark(h * f0);
        h += 1.0;
    }
    let k0 = (f0 / bin_hz).round() as usize;
    let fund = mag[k0.saturating_sub(GUARD as usize)..(k0 + GUARD as usize).min(mag.len() - 1)]
        .iter()
        .fold(0.0f64, |a, b| a.max(*b));
    let (mut worst, mut worst_k) = (0.0f64, 0usize);
    let k_max = ((max_hz / bin_hz).floor() as usize).min(mag.len() - 1);
    for (k, (m, is_masked)) in mag.iter().zip(masked.iter()).enumerate().take(k_max + 1) {
        if !is_masked && *m > worst {
            worst = *m;
            worst_k = k;
        }
    }
    AliasReport {
        worst_db: db(worst / fund.max(1e-300)),
        worst_hz: worst_k as f64 * bin_hz,
        fundamental_db: db(fund / coherent),
    }
}

/// Magnitude spectrum (linear, bins `0..=n/2`) of `signal` after applying `window`.
///
/// `signal` is truncated or zero-padded to the window length, which must be a power of two.
pub fn magnitude_spectrum(signal: &[f32], window: &[f64]) -> Vec<f64> {
    let n = window.len();
    let mut re: Vec<f64> = (0..n).map(|i| signal.get(i).copied().unwrap_or(0.0) as f64 * window[i]).collect();
    let mut im = vec![0.0; n];
    fft(&mut re, &mut im, false);
    (0..=n / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect()
}

/// Root-mean-square of a signal.
pub fn rms(x: &[f32]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt()
}

/// Peak absolute value of a signal.
pub fn peak(x: &[f32]) -> f64 {
    x.iter().fold(0.0f64, |m, v| m.max((*v as f64).abs()))
}

/// Linear amplitude → dB (floored at −300 dB).
pub fn db(x: f64) -> f64 {
    20.0 * x.max(1e-15).log10()
}

/// Removes the mean of a signal (simple DC removal for level measurements).
pub fn remove_dc(x: &[f32]) -> Vec<f32> {
    if x.is_empty() {
        return Vec::new();
    }
    let mean = x.iter().map(|v| *v as f64).sum::<f64>() / x.len() as f64;
    x.iter().map(|v| (*v as f64 - mean) as f32).collect()
}

/// Estimates the fundamental frequency of a periodic signal (Hz) from the strongest spectral
/// peak, refined by parabolic interpolation of the log magnitude. Returns `None` for silence.
pub fn estimate_frequency(x: &[f32], sample_rate: f64) -> Option<f64> {
    let mut n = 1usize;
    while n * 2 <= x.len() && n < (1 << 18) {
        n *= 2;
    }
    if n < 256 {
        return None;
    }
    let win = blackman_harris(n);
    let dc = remove_dc(&x[..n]);
    let mag = magnitude_spectrum(&dc, &win);
    let (k, m) =
        mag.iter().enumerate().skip(2).fold((0, 0.0), |(bk, bm), (k, &m)| if m > bm { (k, m) } else { (bk, bm) });
    if m <= 1e-9 || k + 1 >= mag.len() {
        return None;
    }
    let (a, b, c) = (mag[k - 1].max(1e-30).ln(), mag[k].ln(), mag[k + 1].max(1e-30).ln());
    let den = a - 2.0 * b + c;
    let delta = if den.abs() > 1e-12 { 0.5 * (a - c) / den } else { 0.0 };
    Some((k as f64 + delta) * sample_rate / n as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_roundtrip_and_tone() {
        let n = 1024;
        let mut re: Vec<f64> = (0..n).map(|i| (2.0 * PI * 37.0 * i as f64 / n as f64).cos()).collect();
        let orig = re.clone();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im, false);
        assert!((re[37] - n as f64 / 2.0).abs() < 1e-9);
        assert!(re[36].abs() < 1e-9 && im[38].abs() < 1e-9);
        fft(&mut re, &mut im, true);
        for (a, b) in re.iter().zip(orig.iter()) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    #[test]
    fn frequency_estimate() {
        let sr = 48000.0;
        let x: Vec<f32> = (0..16384).map(|i| (2.0 * PI * 440.0 * i as f64 / sr).sin() as f32).collect();
        let f = estimate_frequency(&x, sr).unwrap();
        assert!((f - 440.0).abs() < 0.5, "{f}");
    }
}
