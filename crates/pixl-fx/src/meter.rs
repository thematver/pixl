//! Output meters (SPEC §5.5): per-channel peak with instant attack and −40 dB/s release,
//! per-channel RMS with a 300 ms exponential window, and a 1.5 s peak hold of the larger
//! channel's sample peak. Readouts are in dBFS, floored at [`METER_FLOOR_DB`].

pub use pixl_core::bridge::METER_FLOOR_DB;

/// Peak release rate (dB per second).
pub const PEAK_RELEASE_DB_PER_S: f32 = 40.0;
/// RMS time constant (seconds).
pub const RMS_TIME_S: f64 = 0.300;
/// Peak-hold time (seconds).
pub const HOLD_TIME_S: f32 = 1.5;

/// Stereo level meter.
#[derive(Clone, Debug)]
pub struct Meter {
    peak: [f32; 2],
    mean_sq: [f64; 2],
    hold: f32,
    hold_left: u32,
    release_mul: f32,
    rms_alpha: f64,
    hold_len: u32,
}

impl Meter {
    /// Creates a meter for `fs`, reading silence.
    pub fn new(fs: f32) -> Self {
        let mut m = Self {
            peak: [0.0; 2],
            mean_sq: [0.0; 2],
            hold: 0.0,
            hold_left: 0,
            release_mul: 1.0,
            rms_alpha: 1.0,
            hold_len: 0,
        };
        m.set_sample_rate(fs);
        m
    }

    /// Updates the ballistics for a new sample rate (keeps readings).
    pub fn set_sample_rate(&mut self, fs: f32) {
        let fs = fs.max(1.0);
        self.release_mul = 10f32.powf(-PEAK_RELEASE_DB_PER_S / 20.0 / fs);
        self.rms_alpha = 1.0 - (-1.0 / (RMS_TIME_S * fs as f64)).exp();
        self.hold_len = (HOLD_TIME_S * fs) as u32;
    }

    /// Resets all readings to silence.
    pub fn reset(&mut self) {
        self.peak = [0.0; 2];
        self.mean_sq = [0.0; 2];
        self.hold = 0.0;
        self.hold_left = 0;
    }

    /// Feeds a stereo block.
    pub fn process(&mut self, l: &[f32], r: &[f32]) {
        let n = l.len().min(r.len());
        let (rm, a) = (self.release_mul, self.rms_alpha);
        for i in 0..n {
            let mut both = 0f32;
            for ((x, peak), ms) in [l[i], r[i]].into_iter().zip(&mut self.peak).zip(&mut self.mean_sq) {
                let v = if x.is_finite() { x.abs() } else { 0.0 };
                *peak = (*peak * rm).max(v);
                *ms += a * ((v as f64) * (v as f64) - *ms);
                both = both.max(v);
            }
            if both >= self.hold {
                self.hold = both;
                self.hold_left = self.hold_len;
            } else if self.hold_left > 0 {
                self.hold_left -= 1;
            } else {
                // Hold expired: fall with the ballistic peak.
                self.hold = self.peak[0].max(self.peak[1]);
            }
        }
        for c in 0..2 {
            if self.peak[c] < 1e-12 {
                self.peak[c] = 0.0;
            }
            if self.mean_sq[c] < 1e-24 {
                self.mean_sq[c] = 0.0;
            }
        }
    }

    /// Ballistic peak per channel (dBFS).
    pub fn peak_db(&self) -> [f32; 2] {
        self.peak.map(to_db)
    }

    /// RMS per channel (dBFS; a full-scale sine reads −3.01).
    pub fn rms_db(&self) -> [f32; 2] {
        self.mean_sq.map(|m| to_db(m.sqrt() as f32))
    }

    /// Held maximum sample peak of both channels (dBFS).
    pub fn peak_hold_db(&self) -> f32 {
        to_db(self.hold)
    }
}

fn to_db(x: f32) -> f32 {
    crate::util::gain_to_db_floor(x, METER_FLOOR_DB)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn peak_attack_is_instant_and_release_is_40_db_per_s() {
        let fs = 48_000.0;
        let mut m = Meter::new(fs);
        let mut l = vec![0f32; 4800];
        l[0] = 1.0;
        let r = vec![0f32; 4800];
        m.process(&l[..1], &r[..1]);
        assert_eq!(m.peak_db()[0], 0.0);
        m.process(&l[1..], &r[1..]); // 0.1 s later
        let p = m.peak_db()[0];
        assert!((p + 4.0).abs() < 0.01, "after 100 ms: {p}");
        let z = vec![0f32; 24_000];
        m.process(&z, &z); // +0.5 s
        let p = m.peak_db()[0];
        assert!((p + 24.0).abs() < 0.01, "after 600 ms: {p}");
        assert_eq!(m.peak_db()[1], METER_FLOOR_DB);
    }

    #[test]
    fn rms_window_and_sine_level() {
        let fs = 48_000.0;
        let mut m = Meter::new(fs);
        let n = (fs * 3.0) as usize;
        let s: Vec<f32> = (0..n).map(|i| (2.0 * PI * 1000.0 * i as f64 / fs as f64).sin() as f32).collect();
        // After one time constant the mean square reached 63.2 % → −2.0 dB below final.
        let tc = (RMS_TIME_S * fs as f64) as usize;
        m.process(&s[..tc], &s[..tc]);
        let early = m.rms_db()[0];
        assert!((early - (-3.0103 + 10.0 * (1.0 - (-1.0f32).exp()).log10())).abs() < 0.1, "{early}");
        m.process(&s[tc..], &s[tc..]);
        let rms = m.rms_db()[0];
        assert!((rms + 3.0103).abs() < 0.02, "{rms}");
    }

    #[test]
    fn hold_lasts_1_5_s_then_falls() {
        let fs = 1000.0;
        let mut m = Meter::new(fs);
        m.process(&[0.5], &[0.25]);
        let z = vec![0f32; 1499];
        m.process(&z, &z);
        assert!((m.peak_hold_db() + 6.0206).abs() < 1e-3);
        let z = vec![0f32; 501];
        m.process(&z, &z);
        let h = m.peak_hold_db();
        // Released after 1.5 s and follows the decaying ballistic peak (−40 dB/s · 2 s).
        assert!(h < -6.03 && (h - (-6.0206 - 80.0)).abs() < 0.1, "{h}");
        // A new, lower peak during release takes over.
        m.process(&[0.1], &[0.0]);
        assert!((m.peak_hold_db() + 20.0).abs() < 1e-3);
    }

    #[test]
    fn silence_reads_floor_and_nan_is_ignored() {
        let mut m = Meter::new(48_000.0);
        m.process(&[f32::NAN, f32::INFINITY], &[0.0, 0.0]);
        assert_eq!(m.peak_db(), [METER_FLOOR_DB; 2]);
        assert_eq!(m.rms_db(), [METER_FLOOR_DB; 2]);
        assert_eq!(m.peak_hold_db(), METER_FLOOR_DB);
    }
}
