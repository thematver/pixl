//! Signal measurements used by the audition: levels, loudness, pitch, spectrum, stereo
//! image, envelope timing and a spectro-temporal distance between two renders.
//!
//! Everything works on 48 kHz renders (`SR`); the K-weighting coefficients are the
//! ITU-R BS.1770-4 values for that rate.

use std::f64::consts::TAU;

/// Sample rate of every render (Hz).
pub const SR: f32 = 48_000.0;

/// Linear amplitude → dBFS (floored at −200 dB).
pub fn db(x: f64) -> f64 {
    20.0 * x.max(1e-10).log10()
}

/// Largest absolute sample of a channel pair.
pub fn peak(l: &[f32], r: &[f32]) -> f64 {
    l.iter().chain(r).fold(0.0f64, |m, v| m.max(v.abs() as f64))
}

/// RMS over both channels.
pub fn rms(l: &[f32], r: &[f32]) -> f64 {
    let n = l.len() + r.len();
    if n == 0 {
        return 0.0;
    }
    let e: f64 = l.iter().chain(r).map(|v| (*v as f64) * (*v as f64)).sum();
    (e / n as f64).sqrt()
}

/// Whether every sample is finite.
pub fn all_finite(l: &[f32], r: &[f32]) -> bool {
    l.iter().chain(r).all(|v| v.is_finite())
}

/// Mid signal `(l + r) / 2`.
pub fn mono(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r).map(|(a, b)| 0.5 * (a + b)).collect()
}

/// Stereo width: RMS of the side signal relative to the mid signal (0 = mono, 1 = equal
/// energy, e.g. fully decorrelated channels).
pub fn width(l: &[f32], r: &[f32]) -> f64 {
    let (mut m, mut s) = (0.0f64, 0.0f64);
    for (a, b) in l.iter().zip(r) {
        let (a, b) = (*a as f64, *b as f64);
        m += (0.5 * (a + b)).powi(2);
        s += (0.5 * (a - b)).powi(2);
    }
    if m <= 1e-20 {
        if s <= 1e-20 {
            0.0
        } else {
            f64::INFINITY
        }
    } else {
        (s / m).sqrt()
    }
}

// ---------------------------------------------------------------------------------------
// Loudness (ITU-R BS.1770-4 / EBU R128 integrated loudness)
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, z: [0.0; 2] }
    }

    fn tick(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

fn k_weighted(x: &[f32]) -> Vec<f64> {
    // BS.1770-4 pre-filter (high shelf, +4 dB) and RLB high-pass, 48 kHz.
    let mut shelf = Biquad::new(
        [1.535_124_859_586_97, -2.691_696_189_406_38, 1.198_392_810_852_85],
        [-1.690_659_293_182_41, 0.732_480_774_215_85],
    );
    let mut hp = Biquad::new([1.0, -2.0, 1.0], [-1.990_047_454_833_98, 0.990_072_250_366_21]);
    x.iter().map(|v| hp.tick(shelf.tick(*v as f64))).collect()
}

/// Integrated loudness in LUFS (gated, 400 ms blocks with 75 % overlap). Returns
/// `f64::NEG_INFINITY` for silence.
pub fn loudness_lufs(l: &[f32], r: &[f32]) -> f64 {
    let (kl, kr) = (k_weighted(l), k_weighted(r));
    let block = (0.4 * SR as f64) as usize;
    let hop = block / 4;
    let mut powers = Vec::new();
    let mut start = 0;
    while start + block <= kl.len() {
        let e: f64 = kl[start..start + block].iter().chain(&kr[start..start + block]).map(|v| v * v).sum();
        powers.push(e / block as f64);
        start += hop;
    }
    let lufs = |p: f64| -0.691 + 10.0 * p.max(1e-30).log10();
    let abs_gated: Vec<f64> = powers.iter().copied().filter(|p| lufs(*p) > -70.0).collect();
    if abs_gated.is_empty() {
        return f64::NEG_INFINITY;
    }
    let rel = lufs(abs_gated.iter().sum::<f64>() / abs_gated.len() as f64) - 10.0;
    let gated: Vec<f64> = abs_gated.into_iter().filter(|p| lufs(*p) > rel).collect();
    lufs(gated.iter().sum::<f64>() / gated.len() as f64)
}

/// Maximum momentary loudness in LUFS (BS.1770 400 ms window, 100 ms hop): how loud the
/// loudest moment is. Used for one-shot material (drums, effects), whose integrated
/// loudness depends more on note length than on how loud each hit sounds.
pub fn momentary_max_lufs(l: &[f32], r: &[f32]) -> f64 {
    let (kl, kr) = (k_weighted(l), k_weighted(r));
    let block = (0.4 * SR as f64) as usize;
    let hop = block / 4;
    let mut best = f64::NEG_INFINITY;
    let mut start = 0;
    while start + block <= kl.len() {
        let e: f64 = kl[start..start + block].iter().chain(&kr[start..start + block]).map(|v| v * v).sum();
        best = best.max(-0.691 + 10.0 * (e / block as f64).max(1e-30).log10());
        start += hop;
    }
    best
}

/// 95th percentile of the absolute difference (cents) between two pitch-modulation traces
/// (semitones per host block), over the blocks where both renders have sounding voices.
pub fn pitch_mod_difference(a: &[f32], a_voices: &[u8], b: &[f32], b_voices: &[u8]) -> f64 {
    let mut d: Vec<f64> = a
        .iter()
        .zip(b)
        .zip(a_voices.iter().zip(b_voices))
        .filter(|(_, (va, vb))| **va > 0 && **vb > 0)
        .map(|((x, y), _)| 100.0 * (*x as f64 - *y as f64).abs())
        .collect();
    if d.is_empty() {
        return 0.0;
    }
    d.sort_by(f64::total_cmp);
    d[((d.len() - 1) as f64 * 0.95).round() as usize]
}

// ---------------------------------------------------------------------------------------
// Spectrum
// ---------------------------------------------------------------------------------------

/// In-place iterative radix-2 FFT (`re.len()` must be a power of two).
pub fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
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
    let mut len = 2;
    while len <= n {
        let ang = -TAU / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

/// Short-time magnitude spectra of `x` (Hann window, `size` points, hop `size / 2`).
pub fn stft(x: &[f32], size: usize) -> Vec<Vec<f64>> {
    let hop = size / 2;
    let window: Vec<f64> = (0..size).map(|i| 0.5 - 0.5 * (TAU * i as f64 / size as f64).cos()).collect();
    let mut frames = Vec::new();
    let mut start = 0;
    let (mut re, mut im) = (vec![0.0; size], vec![0.0; size]);
    while start + size <= x.len() {
        for i in 0..size {
            re[i] = x[start + i] as f64 * window[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        frames.push((0..size / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect());
        start += hop;
    }
    frames
}

/// Spectral centroid (Hz) of the long-term magnitude spectrum, 20 Hz..20 kHz.
pub fn centroid(x: &[f32]) -> f64 {
    let size = 4096;
    let frames = stft(x, size);
    if frames.is_empty() {
        return 0.0;
    }
    let bin_hz = SR as f64 / size as f64;
    let mut avg = vec![0.0f64; size / 2];
    for f in &frames {
        for (a, v) in avg.iter_mut().zip(f) {
            *a += v;
        }
    }
    let (mut num, mut den) = (0.0, 0.0);
    for (k, a) in avg.iter().enumerate() {
        let f = k as f64 * bin_hz;
        if (20.0..=20_000.0).contains(&f) {
            num += f * a;
            den += a;
        }
    }
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

/// Band edges for the spectro-temporal distance: 1/6-octave bands from 40 Hz to 16 kHz.
fn band_edges() -> Vec<f64> {
    let mut edges = vec![40.0f64];
    while *edges.last().unwrap_or(&40.0) < 16_000.0 {
        let next = edges.last().copied().unwrap_or(40.0) * 2f64.powf(1.0 / 6.0);
        edges.push(next);
    }
    edges
}

/// Band energies (dB) per frame.
fn band_frames(x: &[f32]) -> Vec<Vec<f64>> {
    let size = 2048;
    let bin_hz = SR as f64 / size as f64;
    let edges = band_edges();
    stft(x, size)
        .into_iter()
        .map(|frame| {
            edges
                .windows(2)
                .map(|e| {
                    let (lo, hi) =
                        (((e[0] / bin_hz).floor() as usize).max(1), ((e[1] / bin_hz).ceil() as usize).max(2));
                    let energy: f64 = frame[lo..hi.min(frame.len())].iter().map(|v| v * v).sum();
                    10.0 * (energy + 1e-20).log10()
                })
                .collect()
        })
        .collect()
}

/// Difference of two renders' 1/6-octave band energies over time: `(mean, p95)` of the
/// per-frame mean absolute dB difference.
///
/// Frames where both renders are quiet are skipped; each band is floored at 70 dB below the
/// louder render's loudest band so inaudible detail does not count. The mean catches level,
/// timbre and pitch changes; the 95th percentile catches changes confined to a few moments
/// (glide between legato notes, a longer decay, a sweep).
pub fn band_distance(a: &[f32], b: &[f32]) -> (f64, f64) {
    let (fa, fb) = (band_frames(a), band_frames(b));
    let n = fa.len().min(fb.len());
    if n == 0 {
        return (0.0, 0.0);
    }
    let top = fa.iter().chain(&fb).flat_map(|f| f.iter()).fold(f64::MIN, |m, v| m.max(*v));
    let floor = top - 70.0;
    let mut per_frame = Vec::new();
    for i in 0..n {
        let loud = fa[i].iter().chain(&fb[i]).any(|v| *v > floor + 10.0);
        if !loud {
            continue;
        }
        let d: f64 = fa[i].iter().zip(&fb[i]).map(|(x, y)| (x.max(floor) - y.max(floor)).abs()).sum();
        per_frame.push(d / fa[i].len().max(1) as f64);
    }
    if per_frame.is_empty() {
        return (0.0, 0.0);
    }
    let mean = per_frame.iter().sum::<f64>() / per_frame.len() as f64;
    per_frame.sort_by(f64::total_cmp);
    let p95 = per_frame[((per_frame.len() - 1) as f64 * 0.95).round() as usize];
    (mean, p95)
}

// ---------------------------------------------------------------------------------------
// Pitch
// ---------------------------------------------------------------------------------------

/// Frequency of a MIDI note (A4 = 440 Hz).
pub fn note_hz(note: f64) -> f64 {
    440.0 * 2f64.powf((note - 69.0) / 12.0)
}

/// Deviation of `f` from `reference` in cents.
pub fn cents(f: f64, reference: f64) -> f64 {
    1200.0 * (f / reference).log2()
}

/// Fold a cent deviation into −600..600 (octave equivalence).
pub fn fold_octave(c: f64) -> f64 {
    (c + 600.0).rem_euclid(1200.0) - 600.0
}

/// Lowest frequency searched by [`acf_pitch`], relative to the guess. Layered presets
/// (a sub-octave, a parallel fifth) repeat only every two periods of the played note, so
/// the search reaches a little below half the guess; the result is octave-folded anyway.
const ACF_LOWEST: f64 = 0.45;
/// Highest frequency searched by [`acf_pitch`], relative to the guess.
const ACF_HIGHEST: f64 = 1.33;

/// Frame length (samples) [`pitch_track`] uses for a note of `guess` Hz: at least 40 ms and
/// long enough to hold 2.5 of the longest searched periods.
pub fn pitch_frame_len(guess: f64) -> usize {
    let longest = SR as f64 / (guess * ACF_LOWEST);
    ((0.040 * SR as f64).max(2.5 * longest)).ceil() as usize
}

/// Repetition frequency of `x` near `guess` (`0.45 × guess ..= 1.33 × guess`): the shortest
/// lag whose normalized autocorrelation is within 0.08 of the best one, refined
/// parabolically. A single note repeats at its period; a note layered with its sub-octave
/// or fifth repeats at twice the period (half the frequency), which octave folding maps
/// back onto the note. `None` when the segment is silent or aperiodic.
pub fn acf_pitch(x: &[f32], guess: f64) -> Option<f64> {
    let sr = SR as f64;
    let lo = (sr / (guess * ACF_HIGHEST)).floor().max(2.0) as usize;
    let hi = (sr / (guess * ACF_LOWEST)).ceil() as usize;
    if x.len() < 2 * hi + 4 {
        return None;
    }
    let n = x.len() - hi - 2;
    let acf = |lag: usize| -> f64 {
        let (mut num, mut e0, mut e1) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..n {
            let (a, b) = (x[i] as f64, x[i + lag] as f64);
            num += a * b;
            e0 += a * a;
            e1 += b * b;
        }
        if e0 <= 1e-18 || e1 <= 1e-18 {
            0.0
        } else {
            num / (e0 * e1).sqrt()
        }
    };
    let values: Vec<f64> = (lo - 1..=hi + 1).map(acf).collect();
    let peaks: Vec<usize> =
        (1..values.len() - 1).filter(|&i| values[i] >= values[i - 1] && values[i] >= values[i + 1]).collect();
    let best_v = peaks.iter().map(|&i| values[i]).fold(f64::MIN, f64::max);
    if best_v < 0.5 {
        return None;
    }
    let best = peaks.into_iter().find(|&i| values[i] >= best_v - 0.08)?;
    let (a, b, c) = (values[best - 1], values[best], values[best + 1]);
    let denom = a - 2.0 * b + c;
    let shift = if denom.abs() > 1e-12 { 0.5 * (a - c) / denom } else { 0.0 };
    let lag = (lo - 1 + best) as f64 + shift.clamp(-0.5, 0.5);
    Some(sr / lag)
}

/// Pitch track of `x[start..end]`: frames of [`pitch_frame_len`] every 10 ms, in cents
/// relative to `expected` (octave-folded). Frames below `floor` RMS or without a clear
/// period are skipped.
pub fn pitch_track(x: &[f32], start: usize, end: usize, expected: f64, floor: f64) -> Vec<f64> {
    let frame = pitch_frame_len(expected);
    let hop = (0.010 * SR) as usize;
    let end = end.min(x.len());
    let mut out = Vec::new();
    let mut pos = start;
    while pos + frame <= end {
        let seg = &x[pos..pos + frame];
        let r = (seg.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / frame as f64).sqrt();
        if r >= floor {
            if let Some(f) = acf_pitch(seg, expected) {
                out.push(fold_octave(cents(f, expected)));
            }
        }
        pos += hop;
    }
    out
}

/// Mean and standard deviation.
pub fn mean_sd(v: &[f64]) -> Option<(f64, f64)> {
    if v.is_empty() {
        return None;
    }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64;
    Some((m, var.sqrt()))
}

/// Median of a slice (`None` if empty).
pub fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    Some(s[s.len() / 2])
}

// ---------------------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------------------

/// RMS envelope in 5 ms windows (mono), with the window length in samples.
pub fn envelope(x: &[f32]) -> (Vec<f64>, usize) {
    envelope_windowed(x, 0.005)
}

/// RMS envelope in windows of `window_s` seconds (at least one sample), with the window
/// length in samples.
pub fn envelope_windowed(x: &[f32], window_s: f64) -> (Vec<f64>, usize) {
    let w = ((window_s * SR as f64) as usize).max(1);
    let env =
        x.chunks(w).map(|c| (c.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / c.len() as f64).sqrt()).collect();
    (env, w)
}

/// Envelope window for timing a note of `hz` (0 = unpitched): 5 ms, or two periods of a
/// low note, so the window RMS of a bass does not swing with the waveform's phase.
pub fn timing_window(hz: f64) -> f64 {
    if hz > 0.0 {
        (2.0 / hz).max(0.005)
    } else {
        0.005
    }
}

/// Note timing measured on a single held note.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    /// Note-on → 90 % of the maximum envelope (ms).
    pub attack_ms: f64,
    /// Maximum → −20 dB while the key is held (ms); `None` = sustains.
    pub decay_ms: Option<f64>,
    /// Note-off → −60 dB below the maximum (ms); `None` = still sounding at the end.
    pub release_ms: Option<f64>,
}

/// Attack / decay / release of a note held from sample 0 to `off`, on an RMS envelope with
/// windows of `window_s` seconds (see [`timing_window`]).
pub fn timing(x: &[f32], off: usize, window_s: f64) -> Timing {
    let (env, w) = envelope_windowed(x, window_s);
    let max = env.iter().cloned().fold(0.0f64, f64::max);
    if max <= 1e-9 {
        return Timing::default();
    }
    let ms = |i: usize| (i * w) as f64 * 1000.0 / SR as f64;
    let ia = env.iter().position(|v| *v >= 0.9 * max).unwrap_or(0);
    let imax = env.iter().enumerate().fold((0, 0.0f64), |b, (i, v)| if *v > b.1 { (i, *v) } else { b }).0;
    let off_i = off / w;
    let held = off_i.min(env.len());
    let decay_ms = if imax < held { env[imax..held].iter().position(|v| *v <= 0.1 * max).map(ms) } else { None };
    let release_ms = if off_i < env.len() {
        let after = &env[off_i..];
        // The first window from which the envelope stays below −60 dB re. max.
        let quiet = 0.001 * max;
        let last_loud = after.iter().rposition(|v| *v > quiet);
        match last_loud {
            Some(i) if i + 1 >= after.len() => None,
            Some(i) => Some(ms(i + 1)),
            None => Some(0.0),
        }
    } else {
        None
    };
    Timing { attack_ms: ms(ia), decay_ms, release_ms }
}

/// Time (ms) from `from` until the 50 ms RMS stays below `threshold_db` dBFS; `None` if it
/// never does within the render.
pub fn settle_time(l: &[f32], r: &[f32], from: usize, threshold_db: f64) -> Option<f64> {
    let w = (0.050 * SR) as usize;
    let thr = 10f64.powf(threshold_db / 20.0);
    let mut last_loud: Option<usize> = None;
    let mut pos = from;
    while pos < l.len() {
        let end = (pos + w).min(l.len());
        if rms(&l[pos..end], &r[pos..end]) > thr {
            last_loud = Some(end);
        }
        pos = end;
    }
    match last_loud {
        None => Some(0.0),
        Some(e) if e >= l.len() => None,
        Some(e) => Some((e - from) as f64 * 1000.0 / SR as f64),
    }
}

/// Duration (ms) during which the 20 ms RMS is above `threshold_db` dBFS.
pub fn active_ms(l: &[f32], r: &[f32], threshold_db: f64) -> f64 {
    let w = (0.020 * SR) as usize;
    let thr = 10f64.powf(threshold_db / 20.0);
    let n = l.chunks(w).zip(r.chunks(w)).filter(|(a, b)| rms(a, b) > thr).count();
    (n * w) as f64 * 1000.0 / SR as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, secs: f64, amp: f64) -> Vec<f32> {
        (0..(secs * SR as f64) as usize).map(|i| (amp * (TAU * f * i as f64 / SR as f64).sin()) as f32).collect()
    }

    #[test]
    fn loudness_matches_ebu_tech_3341() {
        // A 1 kHz sine at −23 dBFS in both channels reads −23 LUFS (EBU Tech 3341, case 1);
        // in one channel only it reads 3 dB less.
        let amp = 10f64.powf(-23.0 / 20.0);
        let x = sine(1000.0, 3.0, amp);
        let l = loudness_lufs(&x, &x);
        assert!((l - (-23.0)).abs() < 0.1, "{l}");
        let silent = vec![0.0f32; x.len()];
        let l1 = loudness_lufs(&x, &silent);
        assert!((l1 - (-26.01)).abs() < 0.1, "{l1}");
    }

    #[test]
    fn pitch_of_a_sine() {
        let x = sine(261.6256, 0.5, 0.5);
        let f = acf_pitch(&x, 261.6).unwrap();
        assert!(cents(f, 261.6256).abs() < 1.0, "{f}");
        let track = pitch_track(&x, 0, x.len(), 261.6256, 1e-3);
        let (m, sd) = mean_sd(&track).unwrap();
        assert!(m.abs() < 1.0 && sd < 1.0, "{m} {sd}");
    }

    #[test]
    fn pitch_of_layered_notes_folds_onto_the_root() {
        // A note with its sub-octave, and a note with a parallel fifth above: both repeat at
        // twice the note's period, which folds back onto the note.
        let n = (0.5 * SR as f64) as usize;
        let f = 110.0;
        let tone = |k: f64, a: f64, i: usize| a * (TAU * f * k * i as f64 / SR as f64).sin();
        let sub: Vec<f32> = (0..n).map(|i| (tone(1.0, 0.4, i) + tone(0.5, 0.4, i)) as f32).collect();
        let fifth: Vec<f32> = (0..n).map(|i| (tone(1.0, 0.4, i) + tone(1.5, 0.4, i)) as f32).collect();
        for x in [sub, fifth] {
            let track = pitch_track(&x, 0, x.len(), f, 1e-3);
            let m = median(&track).unwrap();
            assert!(m.abs() < 1.0, "{m}");
        }
    }

    #[test]
    fn fft_matches_dft() {
        let n = 64;
        let x: Vec<f64> = (0..n).map(|i| ((i * 7 % 13) as f64 - 6.0) * 0.1).collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; n]);
        fft(&mut re, &mut im);
        for k in 0..n {
            let (mut r, mut i) = (0.0, 0.0);
            for (t, v) in x.iter().enumerate() {
                let a = -TAU * (k * t) as f64 / n as f64;
                r += v * a.cos();
                i += v * a.sin();
            }
            assert!((r - re[k]).abs() < 1e-9 && (i - im[k]).abs() < 1e-9);
        }
    }

    #[test]
    fn width_and_distance() {
        let a = sine(440.0, 0.5, 0.5);
        let b: Vec<f32> = a.iter().map(|v| -v).collect();
        assert!(width(&a, &a) < 1e-9);
        assert!(width(&a, &b).is_infinite());
        assert!(band_distance(&a, &a).0 < 1e-9);
        let c = sine(880.0, 0.5, 0.5);
        assert!(band_distance(&a, &c).0 > 3.0);
    }
}
