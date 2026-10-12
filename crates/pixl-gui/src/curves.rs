//! Analytical previews. These are parameter diagrams, not captured output audio.
use pixl_core::{Bridge, Channel, ChannelField, Focus, ParamId};

pub(crate) struct Curve {
    pub duration_ms: f64,
    pub handles: Vec<(f64, f64)>,
    pub guides: Vec<Vec<(f64, f64)>>,
    pub frame_clocked: bool,
    kind: Kind,
}

enum Kind {
    Env { attack: f64, decay: f64, sustain: f64, hold: f64, release: f64 },
    Sweep { amount: f64, time: f64, direction: f64, exponential: bool },
    Vibrato { delay: f64, fade: f64, depth: f64, rate: f64, shape: u8 },
    Crush { bits: f64, rate: f64 },
    Glide { start: f64, time: f64, exponential: bool },
}

impl Curve {
    pub fn new(bridge: &dyn Bridge, focus: Focus, channel: Channel) -> Option<Self> {
        let get = |id| bridge.param_modulated(id) as f64;
        let (kind, duration_ms) = match focus {
            Focus::Env => {
                let attack = get(ParamId::channel(channel, ChannelField::Attack));
                let decay = get(ParamId::channel(channel, ChannelField::Decay));
                let sustain = get(ParamId::channel(channel, ChannelField::Sustain)) / 15.;
                let release = get(ParamId::channel(channel, ChannelField::Release));
                let hold = (attack + decay + release).max(300.) * 0.35;
                (Kind::Env { attack, decay, sustain, hold, release }, (attack + decay + hold + release).max(1.))
            }
            Focus::Sweep => {
                let time = get(ParamId::SweepTime);
                (
                    Kind::Sweep {
                        amount: get(ParamId::SweepAmt),
                        time,
                        direction: if get(ParamId::SweepDir) > 0. { -1. } else { 1. },
                        exponential: get(ParamId::SweepCurve) < 0.5,
                    },
                    (time * 1.3).max(1.),
                )
            }
            Focus::Vibrato => {
                let delay = get(ParamId::VibDelay);
                let fade = get(ParamId::VibFade);
                let rate = get(ParamId::VibRate).max(0.01);
                (
                    Kind::Vibrato {
                        delay,
                        fade,
                        rate,
                        depth: get(ParamId::VibDepth),
                        shape: get(ParamId::VibShape) as u8,
                    },
                    delay + fade + (3000. / rate).clamp(250., 2000.),
                )
            }
            Focus::Crush => {
                (Kind::Crush { bits: get(ParamId::CrushBits), rate: get(ParamId::CrushRate) }, 3000. / 440.)
            }
            Focus::Glide => {
                let time = get(ParamId::GlideTime).max(1.);
                let start = time * 0.2;
                (Kind::Glide { start, time, exponential: get(ParamId::GlideCurve) < 0.5 }, start + time * 1.5)
            }
            _ => return None,
        };
        let mut curve =
            Self { kind, duration_ms, handles: Vec::new(), guides: Vec::new(), frame_clocked: focus != Focus::Crush };
        match curve.kind {
            Kind::Env { attack, decay, sustain, .. } => {
                curve.handles.extend([(attack, 1.), (attack + decay, sustain), (duration_ms, 0.)])
            }
            Kind::Sweep { time, .. } => curve.handles.extend([(0., curve.sample(0.)), (time, 0.5)]),
            Kind::Vibrato { delay, fade, depth, .. } => {
                let amplitude = 0.43 * depth / depth.max(25.);
                curve.handles.extend([(delay, 0.5), (delay + fade, 0.5 + amplitude)]);
                for direction in [-1., 1.] {
                    curve.guides.push(vec![
                        (0., 0.5),
                        (delay, 0.5),
                        (delay + fade, 0.5 + direction * amplitude),
                        (duration_ms, 0.5 + direction * amplitude),
                    ]);
                }
            }
            Kind::Glide { start, time, .. } => {
                curve.handles.extend([(start, 0.1), (start + time, curve.sample(start + time))])
            }
            Kind::Crush { .. } => {}
        }
        Some(curve)
    }

    pub fn sample(&self, ms: f64) -> f64 {
        match self.kind {
            Kind::Env { attack, decay, sustain, hold, release } => {
                if ms < attack {
                    ms / attack.max(0.001)
                } else if ms < attack + decay {
                    1. - (1. - sustain) * (ms - attack) / decay.max(0.001)
                } else if ms < attack + decay + hold {
                    sustain
                } else {
                    sustain * (1. - (ms - attack - decay - hold) / release.max(0.001)).max(0.)
                }
            }
            Kind::Sweep { amount, time, direction, exponential } => {
                let residual = (1. - ms / time.max(0.001)).clamp(0., 1.);
                0.5 + direction * 0.45 * amount / amount.max(12.)
                    * if exponential { residual * residual } else { residual }
            }
            Kind::Vibrato { delay, fade, depth, rate, shape } => {
                let p = (ms / 1000. * rate).fract();
                let lfo = match shape {
                    0 => (p * std::f64::consts::TAU).sin(),
                    1 if p < 0.25 => 4. * p,
                    1 if p < 0.75 => 2. - 4. * p,
                    1 => 4. * p - 4.,
                    _ if p < 0.5 => 1.,
                    _ => -1.,
                };
                let envelope = if ms < delay {
                    0.
                } else if fade <= 0. {
                    1.
                } else {
                    ((ms - delay) / fade).clamp(0., 1.)
                };
                0.5 + lfo * 0.43 * depth / depth.max(25.) * envelope
            }
            Kind::Crush { bits, rate } => {
                // Ideal settled sample-and-hold diagram; the DSP also band-limits its edges.
                let held_ms = (ms * rate / 1000.).floor() * 1000. / rate.max(1.);
                0.5 + 0.45 * quantize_reference((held_ms * 0.440 * std::f64::consts::TAU).sin() * 0.8, bits)
            }
            Kind::Glide { start, time, exponential } => {
                let x = ((ms - start) / time).max(0.);
                let residual = if exponential { (-4.605_170_185_988_091 * x).exp() } else { (1. - x).max(0.) };
                0.1 + 0.6 * (1. - residual)
            }
        }
        .clamp(0., 1.)
    }

    pub fn reference(&self, ms: f64) -> f64 {
        if matches!(self.kind, Kind::Crush { .. }) {
            0.5 + 0.45 * (ms * 0.440 * std::f64::consts::TAU).sin() * 0.8
        } else {
            self.sample(ms)
        }
    }
}

fn quantize_reference(x: f64, bits: f64) -> f64 {
    let s = (bits.clamp(1., 16.) - 1.).exp2();
    let tread = (x * s).round().clamp(-s.floor(), s.ceil() - 1.) / s;
    let sign_mix = (2. - bits).clamp(0., 1.);
    let rise = (x / (0.8 / 16.)).clamp(-1., 1.) * (0.5 / s).min(0.8);
    tread + sign_mix * (rise - tread)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockBridge;

    #[test]
    fn handles_are_attached_to_sweep_and_glide_curves_at_extremes() {
        let b = MockBridge::new();
        for amount in [0., 1., 12., 24.] {
            b.set_param_once(ParamId::SweepAmt, amount);
            let c = Curve::new(&b, Focus::Sweep, Channel::Wave).unwrap();
            for &(t, y) in &c.handles {
                assert!((c.sample(t) - y).abs() < 1e-9);
            }
        }
        for mode in [0., 1.] {
            b.set_param_once(ParamId::GlideCurve, mode);
            let c = Curve::new(&b, Focus::Glide, Channel::P1).unwrap();
            for &(t, y) in &c.handles {
                assert!((c.sample(t) - y).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn one_bit_preview_preserves_both_polarities_and_zero() {
        assert_eq!(quantize_reference(0., 1.), 0.);
        assert_eq!(quantize_reference(-0.8, 1.), -0.5);
        assert_eq!(quantize_reference(0.8, 1.), 0.5);
        assert_eq!(quantize_reference(0.8, 2.), 0.5);
        assert_eq!(quantize_reference(-0.8, 2.), -1.);
    }

    #[test]
    fn exponential_glide_reaches_99_percent_at_specified_time() {
        let b = MockBridge::new();
        b.set_param_once(ParamId::GlideCurve, 0.);
        let c = Curve::new(&b, Focus::Glide, Channel::P1).unwrap();
        let Kind::Glide { start, time, .. } = c.kind else { unreachable!() };
        assert!((c.sample(start + time) - 0.694).abs() < 1e-9);
    }
}
