//! Linear ADSR envelope (SPEC §4.5).
//!
//! Segments are linear in level. A fresh attack from 0 takes exactly `attack`; a retrigger
//! attacks from the current level at the same slope (no click). Decay runs from 1 to the
//! sustain level in exactly `decay`; release runs from wherever it starts to 0 in exactly
//! `release`. Sustain-level changes while sustaining are slewed (full scale in
//! [`SUSTAIN_SLEW_MS`]) so macro/automation moves never step the level.
//!
//! The envelope is evaluated at control/frame ticks only ([`Adsr::advance`] by the elapsed
//! time, [`Adsr::peek`] for the value a little ahead); it is a pure state machine with no
//! per-sample work, so it costs nothing between ticks.

/// Time for the sustain level to slew across the full 0..1 range when the parameter moves.
pub const SUSTAIN_SLEW_MS: f64 = 10.0;

/// Envelope parameters with times in samples.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvParams {
    /// Attack time (0 → 1), samples. ≤ 0 means instant.
    pub attack: f64,
    /// Decay time (1 → sustain), samples. ≤ 0 means instant.
    pub decay: f64,
    /// Sustain level 0..1.
    pub sustain: f64,
    /// Release time (current level → 0), samples. ≤ 0 means instant.
    pub release: f64,
    /// Sustain slew rate, level per sample.
    pub slew: f64,
}

impl EnvParams {
    /// Parameters from milliseconds / sustain 0..15 at a sample rate (inputs are sanitized).
    pub fn from_ms(attack_ms: f32, decay_ms: f32, sustain_level: f32, release_ms: f32, sample_rate: f64) -> Self {
        let ms = |v: f32| if v.is_finite() { (v.max(0.0) as f64) * sample_rate / 1000.0 } else { 0.0 };
        let sustain = if sustain_level.is_finite() { (sustain_level as f64 / 15.0).clamp(0.0, 1.0) } else { 1.0 };
        Self {
            attack: ms(attack_ms),
            decay: ms(decay_ms),
            sustain,
            release: ms(release_ms),
            slew: 1000.0 / (SUSTAIN_SLEW_MS * sample_rate.max(1.0)),
        }
    }
}

impl Default for EnvParams {
    fn default() -> Self {
        Self::from_ms(2.0, 400.0, 12.0, 150.0, 48_000.0)
    }
}

/// Envelope stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Finished (level 0). A voice ends when all its enabled channels are idle.
    Idle,
    /// Rising towards 1.
    Attack,
    /// Falling from 1 to the sustain level.
    Decay,
    /// Holding the sustain level (slewing to parameter changes).
    Sustain,
    /// Falling to 0 after the key was released.
    Release,
}

/// ADSR state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Adsr {
    stage: Stage,
    level: f64,
    /// Release slope (level per sample), fixed when the release starts.
    release_rate: f64,
}

impl Default for Adsr {
    fn default() -> Self {
        Self::new()
    }
}

impl Adsr {
    /// An idle envelope at level 0.
    pub const fn new() -> Self {
        Self { stage: Stage::Idle, level: 0.0, release_rate: 0.0 }
    }

    /// Current stage.
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// Current level 0..1.
    pub fn level(&self) -> f32 {
        self.level as f32
    }

    /// Whether the envelope has finished.
    pub fn is_idle(&self) -> bool {
        self.stage == Stage::Idle
    }

    /// Whether the envelope is in its release stage.
    pub fn is_releasing(&self) -> bool {
        self.stage == Stage::Release
    }

    /// Hard reset to idle / 0.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// (Re)start the attack from the current level.
    pub fn trigger(&mut self) {
        self.stage = Stage::Attack;
    }

    /// Start the release from the current level; it reaches 0 after `p.release` samples.
    pub fn release(&mut self, p: &EnvParams) {
        if self.stage == Stage::Idle || self.stage == Stage::Release {
            return;
        }
        if p.release <= 0.0 || self.level <= 0.0 {
            self.level = 0.0;
            self.stage = Stage::Idle;
        } else {
            self.release_rate = self.level / p.release;
            self.stage = Stage::Release;
        }
    }

    /// Advance by `dt` samples.
    pub fn advance(&mut self, dt: f64, p: &EnvParams) {
        let mut rem = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        // Each iteration either consumes all remaining time or moves to a later stage, so the
        // loop runs at most a handful of times.
        for _ in 0..6 {
            match self.stage {
                Stage::Idle => {
                    self.level = 0.0;
                    return;
                }
                Stage::Attack => {
                    if p.attack <= 0.0 || self.level >= 1.0 {
                        self.level = 1.0;
                        self.stage = Stage::Decay;
                        continue;
                    }
                    let rate = 1.0 / p.attack;
                    let need = (1.0 - self.level) / rate;
                    if rem < need {
                        self.level += rate * rem;
                        return;
                    }
                    rem -= need;
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
                Stage::Decay => {
                    let s = p.sustain.clamp(0.0, 1.0);
                    if p.decay <= 0.0 {
                        self.level = s;
                        self.stage = Stage::Sustain;
                        continue;
                    }
                    if self.level <= s {
                        // The sustain level was raised above the current level: slew up.
                        self.stage = Stage::Sustain;
                        continue;
                    }
                    let rate = ((1.0 - s) / p.decay).max(1e-12);
                    let need = (self.level - s) / rate;
                    if rem < need {
                        self.level -= rate * rem;
                        return;
                    }
                    rem -= need;
                    self.level = s;
                    self.stage = Stage::Sustain;
                }
                Stage::Sustain => {
                    let s = p.sustain.clamp(0.0, 1.0);
                    if s <= 0.0 && self.level <= 0.0 {
                        self.level = 0.0;
                        self.stage = Stage::Idle;
                        return;
                    }
                    let step = p.slew.max(1e-9) * rem;
                    if (self.level - s).abs() <= step {
                        self.level = s;
                        if s <= 0.0 {
                            self.stage = Stage::Idle;
                        }
                    } else if self.level > s {
                        self.level -= step;
                    } else {
                        self.level += step;
                    }
                    return;
                }
                Stage::Release => {
                    self.level -= self.release_rate * rem;
                    if self.level <= 0.0 || !self.level.is_finite() {
                        self.level = 0.0;
                        self.stage = Stage::Idle;
                    }
                    return;
                }
            }
        }
    }

    /// Current slope magnitude (level change per sample) of the active segment; 0 when idle
    /// or settled at the sustain level.
    pub fn rate(&self, p: &EnvParams) -> f64 {
        match self.stage {
            Stage::Idle => 0.0,
            Stage::Attack => {
                if p.attack > 0.0 {
                    1.0 / p.attack
                } else {
                    1.0
                }
            }
            Stage::Decay => {
                if p.decay > 0.0 {
                    (1.0 - p.sustain.clamp(0.0, 1.0)) / p.decay
                } else {
                    1.0
                }
            }
            Stage::Sustain => {
                if (self.level - p.sustain.clamp(0.0, 1.0)).abs() > 1e-9 {
                    p.slew
                } else {
                    0.0
                }
            }
            Stage::Release => self.release_rate,
        }
    }

    /// The level `dt` samples from now, without changing the state.
    pub fn peek(&self, dt: f64, p: &EnvParams) -> f32 {
        let mut e = *self;
        e.advance(dt, p);
        e.level as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> EnvParams {
        // 48 kHz: A 10 ms, D 100 ms to sustain 6/15, R 200 ms.
        EnvParams::from_ms(10.0, 100.0, 6.0, 200.0, 48_000.0)
    }

    #[test]
    fn adsr_timing_is_exact() {
        let p = params();
        let mut e = Adsr::new();
        e.trigger();
        e.advance(240.0, &p); // 5 ms: half way up
        assert!((e.level() - 0.5).abs() < 1e-6);
        e.advance(240.0, &p); // 10 ms: peak
        assert!((e.level() - 1.0).abs() < 1e-6);
        assert_eq!(e.stage(), Stage::Decay);
        e.advance(2400.0, &p); // 50 ms into decay: 1 − 0.6·0.5
        assert!((e.level() - 0.7).abs() < 1e-6, "{}", e.level());
        e.advance(2400.0, &p);
        assert!((e.level() - 0.4).abs() < 1e-6);
        assert_eq!(e.stage(), Stage::Sustain);
        e.advance(48_000.0, &p);
        assert!((e.level() - 0.4).abs() < 1e-6);
        e.release(&p);
        e.advance(4800.0, &p); // 100 ms of a 200 ms release
        assert!((e.level() - 0.2).abs() < 1e-6);
        e.advance(4800.0, &p);
        assert!(e.is_idle());
        assert_eq!(e.level(), 0.0);
    }

    #[test]
    fn release_takes_release_time_from_any_level() {
        let p = params();
        let mut e = Adsr::new();
        e.trigger();
        e.advance(120.0, &p); // 0.25
        e.release(&p);
        e.advance(9599.0, &p);
        assert!(!e.is_idle());
        e.advance(2.0, &p);
        assert!(e.is_idle());
    }

    #[test]
    fn retrigger_attacks_from_current_level() {
        let p = params();
        let mut e = Adsr::new();
        e.trigger();
        e.advance(48_000.0, &p);
        e.release(&p);
        e.advance(4800.0, &p); // 0.2
        e.trigger();
        let l0 = e.level();
        assert!((l0 - 0.2).abs() < 1e-6);
        e.advance(48.0, &p); // 1 ms at 1/480 per sample
        assert!((e.level() - (0.2 + 0.1)).abs() < 1e-6);
    }

    #[test]
    fn zero_sustain_finishes_after_decay() {
        let p = EnvParams::from_ms(0.0, 50.0, 0.0, 100.0, 48_000.0);
        let mut e = Adsr::new();
        e.trigger();
        e.advance(1.0, &p);
        assert!(!e.is_idle());
        e.advance(2400.0, &p);
        assert!(e.is_idle(), "one-shot envelopes end without a note-off");
    }

    #[test]
    fn instant_segments_and_garbage() {
        let p = EnvParams::from_ms(0.0, 0.0, 15.0, 0.0, 48_000.0);
        let mut e = Adsr::new();
        e.trigger();
        e.advance(0.0, &p);
        assert_eq!(e.level(), 1.0);
        e.release(&p);
        assert!(e.is_idle());
        let bad = EnvParams::from_ms(f32::NAN, f32::INFINITY, f32::NAN, -5.0, 48_000.0);
        let mut e = Adsr::new();
        e.trigger();
        e.advance(f64::NAN, &bad);
        e.advance(1e12, &bad);
        assert!(e.level().is_finite());
    }

    #[test]
    fn sustain_changes_are_slewed() {
        let mut p = params();
        let mut e = Adsr::new();
        e.trigger();
        e.advance(48_000.0, &p);
        p.sustain = 1.0;
        e.advance(48.0, &p); // 1 ms: at most 0.1 of the 10 ms full-scale slew
        assert!((e.level() - 0.5).abs() < 1e-6, "{}", e.level());
        e.advance(4800.0, &p);
        assert!((e.level() - 1.0).abs() < 1e-6);
        assert_eq!(e.peek(10.0, &p), 1.0);
    }
}
