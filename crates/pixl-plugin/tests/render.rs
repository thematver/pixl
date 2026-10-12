//! End-to-end rendering through the real engine (`pixl-dsp`) and effect chain (`pixl-fx`),
//! driven exactly like the plugin drives them: host blocks of arbitrary size, events at
//! sample offsets, [`pixl::processor::Processor`] splitting at events and every 64 samples,
//! macros applied, MIDI going through the plugin's mapping.
//!
//! Checks: sample-accurate note starts, host-block-size invariance, correct pitch (FREE
//! exact; NES/GB at their register-quantized frequencies), clean note ends, no clicks at
//! note boundaries, the output ceiling, MIDI controllers, macros reaching the effects,
//! telemetry, tail reporting, tempo sync, sample rates, the factory library and a
//! randomized stress test.

mod common;

use common::*;
use nih_plug::prelude::NoteEvent;
use pixl::presets::{factory_library, init_state};
use pixl::processor::BlockStatus;
use pixl_core::preset::PresetState;
use pixl_core::{wave, Channel, ChannelField, MacroAssignments, ParamId, ParamValues, PARAMS};
use pixl_dsp::Transport;

const SR: f32 = 48_000.0;

fn cc(cc: u8, value: u8) -> NoteEvent<()> {
    NoteEvent::MidiCC { timing: 0, channel: 0, cc, value: value as f32 / 127.0 }
}

fn bend(raw: u16) -> NoteEvent<()> {
    NoteEvent::MidiPitchBend { timing: 0, channel: 0, value: raw as f32 / 16383.0 }
}

/// Pitch of `x[from..to]` (seconds) near `guess`.
fn pitch_in(x: &[f32], sr: f32, from: f32, to: f32, guess: f32) -> f32 {
    pitch(&x[secs(sr, from)..secs(sr, to)], sr, guess)
}

/// Patches whose first output sample is non-zero by construction (zero attack, a channel
/// whose first step is high), so the onset time is exactly measurable in every chip mode.
fn onset_patches() -> Vec<(&'static str, PresetState)> {
    let mut out = Vec::new();
    // GB 50 % is `10000111` and NES 75 % is `10011111`: both start on a high step.
    for (name, chip, duty) in [("FREE pulse", 2.0, 50.0), ("GB pulse", 1.0, 50.0), ("NES pulse", 0.0, 75.0)] {
        let mut st = clean_patch(chip, Channel::P1);
        st.values.set(ParamId::P1Attack, 0.0);
        st.values.set(ParamId::P1Duty, duty);
        out.push((name, st));
    }
    out
}

#[test]
fn note_on_at_offset_n_starts_sound_at_n() {
    for (name, st) in onset_patches() {
        for block in [64usize, 100, 512] {
            for n in [0usize, 1, 17, 63, 64, 65, 100, 511, 512, 777, 1500] {
                let r = Session::new(&st, SR).render(&[on(n, 69, 0.8)], n + 2048, block);
                assert!(r.all_finite());
                assert!(
                    r.left[..n].iter().chain(&r.right[..n]).all(|x| *x == 0.0),
                    "{name}: output before the note-on at {n} (host block {block})"
                );
                let first = r.left.iter().zip(&r.right).position(|(l, rr)| *l != 0.0 || *rr != 0.0).unwrap();
                assert!((n..=n + 2).contains(&first), "{name}: note-on at {n} sounds from {first} (block {block})");
                assert!(peak(&r.left[n..n + 8]) > 0.05, "{name}: not at full level within 8 samples of {n}");
            }
        }
    }
}

/// A phrase that exercises voices, unison, pitch bend, mod wheel, sustain, echo and crush.
fn busy_phrase() -> Vec<At> {
    let s = |t: f32| secs(SR, t);
    let mut ev = vec![on(s(0.0), 60, 0.9), on(s(0.01), 64, 0.7), on(s(0.02), 67, 0.5), off(s(0.3), 60)];
    ev.extend(midi(s(0.25), bend(12000)));
    ev.extend(midi(s(0.35), cc(1, 100)));
    ev.extend(midi(s(0.4), cc(64, 127)));
    ev.extend([off(s(0.45), 64), off(s(0.46), 67), on(s(0.5), 72, 1.0), off(s(0.6), 72)]);
    ev.extend(midi(s(0.7), cc(64, 0)));
    ev.extend(midi(s(0.72), bend(8192)));
    ev.extend([on(s(0.8), 55, 0.6), on(s(0.8), 59, 0.6), off(s(1.1), 55), off(s(1.1), 59)]);
    ev
}

fn busy_state() -> PresetState {
    let mut st = init_state();
    st.values.set(ParamId::EchoOn, 1.0);
    st.values.set(ParamId::CrushOn, 1.0);
    st.values.set(ParamId::CrushBits, 9.5);
    st.values.set(ParamId::P1On, 1.0);
    st.values.set(ParamId::NsOn, 1.0);
    st.values.set(ParamId::NsLevel, 4.0);
    st
}

fn max_diff(a: &Render, b: &Render) -> f32 {
    a.left.iter().zip(&b.left).chain(a.right.iter().zip(&b.right)).fold(0.0f32, |m, (x, y)| m.max((x - y).abs()))
}

#[test]
fn host_block_size_does_not_change_the_sound() {
    let total = secs(SR, 1.6);
    // Engine, chip filters, output stage: identical up to float rounding for any segmentation
    // (nih-plug splits host blocks at every automation event).
    let mut no_echo = busy_state();
    no_echo.values.set(ParamId::EchoOn, 0.0);
    let mut clean = no_echo.clone();
    clean.values.set(ParamId::CrushOn, 0.0);
    let reference = Session::new(&clean, SR).render(&busy_phrase(), total, 64);
    assert!(reference.peak() > 0.05);
    for block in [1usize, 7, 63, 100, 333, 512, 4096] {
        let r = Session::new(&clean, SR).render(&busy_phrase(), total, block);
        let diff = max_diff(&reference, &r);
        assert!(diff <= 1e-5, "host block {block}: output differs by {diff:e} from 64-sample blocks");
    }
    // With the crusher on, the same: a quantizer is discontinuous, so a rounding difference
    // that happens to straddle a decision threshold flips that one code (one step Δ for one
    // hold period, ≈ 1.2 Δ with the band-limited step's overshoot) — but never more, and only
    // in isolated places. Anything systematic (hold-clock phase, smoothing, event timing)
    // would show up as many or larger differences.
    let mut s = Session::new(&no_echo, SR);
    let reference = s.render(&busy_phrase(), total, 64);
    let step = 2f32.powf(1.0 - s.processor.effective_values().get(ParamId::CrushBits));
    let energy = |x: &[f32]| x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
    let signal = energy(&reference.left) + energy(&reference.right);
    for block in [1usize, 7, 63, 100, 333, 512, 4096] {
        let r = Session::new(&no_echo, SR).render(&busy_phrase(), total, block);
        let diff = max_diff(&reference, &r);
        assert!(diff <= 1.5 * step, "host block {block}: crushed output differs by {diff:e} (step {step:e})");
        let error: f64 = reference
            .left
            .iter()
            .zip(&r.left)
            .chain(reference.right.iter().zip(&r.right))
            .map(|(a, b)| ((a - b) as f64).powi(2))
            .sum();
        let ratio = 10.0 * (error / signal).max(1e-30).log10();
        assert!(
            ratio < -60.0,
            "host block {block}: crushed output differs by {ratio:.1} dB (more than isolated flips)"
        );
    }
    // With the echo on, its internal 32 kHz grid may restart at a different phase when it
    // wakes, so repeats can differ in fine detail (mostly near the 14-16 kHz band edge), but
    // never in timing or level.
    let reference = Session::new(&busy_state(), SR).render(&busy_phrase(), total, 64);
    let energy = |r: &Render| r.left.iter().chain(&r.right).map(|x| (*x as f64).powi(2)).sum::<f64>();
    for block in [1usize, 7, 100, 512, 4096] {
        let r = Session::new(&busy_state(), SR).render(&busy_phrase(), total, block);
        let diff = max_diff(&reference, &r);
        assert!(diff <= 0.03, "host block {block}: echo output differs by {diff:e}");
        let ratio = energy(&r) / energy(&reference);
        assert!((ratio - 1.0).abs() < 1e-3, "host block {block}: energy ratio {ratio}");
    }
}

#[test]
fn rendering_is_deterministic() {
    let total = secs(SR, 1.2);
    let a = Session::new(&busy_state(), SR).render(&busy_phrase(), total, 256);
    let b = Session::new(&busy_state(), SR).render(&busy_phrase(), total, 256);
    assert_eq!(a.left, b.left);
    assert_eq!(a.right, b.right);
}

/// The phrase used for pitch checks: C4 E4 G4 C5 A4, 0.25 s each with 0.05 s gaps.
const MELODY: [u8; 5] = [60, 64, 67, 72, 69];

fn melody_events(sr: f32) -> Vec<At> {
    MELODY
        .iter()
        .enumerate()
        .flat_map(|(i, n)| {
            let t = i as f32 * 0.3;
            [on(secs(sr, t), *n, 0.8), off(secs(sr, t + 0.25), *n)]
        })
        .collect()
}

/// Frequency the chip actually plays for `note` (SPEC §4.3 register quantization).
fn chip_hz(chip: f32, channel: Channel, note: u8) -> f32 {
    let f = note_hz(note as f32) as f64;
    match (chip as u32, channel) {
        // NES pulse: t = round(CPU / (16 f) − 1), f = CPU / (16 (t + 1)).
        (0, Channel::P1 | Channel::P2) => {
            let cpu = 1_789_773.0;
            let t = (cpu / (16.0 * f) - 1.0).round().clamp(0.0, 2047.0);
            (cpu / (16.0 * (t + 1.0))) as f32
        }
        // NES triangle: t = round(CPU / (32 f) − 1).
        (0, Channel::Wave) => {
            let cpu = 1_789_773.0;
            let t = (cpu / (32.0 * f) - 1.0).round().clamp(0.0, 2047.0);
            (cpu / (32.0 * (t + 1.0))) as f32
        }
        // GB pulse: x = round(2048 − 131072 / f), f = 131072 / (2048 − x).
        (1, Channel::P1 | Channel::P2) => {
            let x = (2048.0 - 131_072.0 / f).round().clamp(0.0, 2047.0);
            (131_072.0 / (2048.0 - x)) as f32
        }
        // GB wave: x = round(2048 − 65536 / f), f = 65536 / (2048 − x).
        (1, Channel::Wave) => {
            let x = (2048.0 - 65_536.0 / f).round().clamp(0.0, 2047.0);
            (65_536.0 / (2048.0 - x)) as f32
        }
        _ => f as f32,
    }
}

#[test]
fn melody_plays_the_right_pitches_in_every_chip_mode() {
    for (chip, channel) in [
        (2.0, Channel::P1),
        (2.0, Channel::Wave),
        (1.0, Channel::P1),
        (1.0, Channel::Wave),
        (0.0, Channel::P1),
        (0.0, Channel::Wave),
    ] {
        let mut st = clean_patch(chip, channel);
        // Short release so a note does not overlap the next one (the NES triangle has no
        // volume control: it plays at full level until its envelope ends).
        st.values.set(ParamId::channel(channel, ChannelField::Release), 5.0);
        if channel == Channel::Wave {
            st.wave = wave::triangle();
        }
        let r = Session::new(&st, SR).render(&melody_events(SR), secs(SR, 1.6), 512);
        assert!(r.all_finite());
        let x = r.mono();
        for (i, note) in MELODY.iter().enumerate() {
            let t = i as f32 * 0.3;
            let expected = chip_hz(chip, channel, *note);
            let f = pitch_in(&x, SR, t + 0.04, t + 0.24, expected);
            let err = cents(f, expected);
            assert!(
                err.abs() < 1.0,
                "chip {chip} {channel:?} note {note}: {f:.3} Hz, expected {expected:.3} Hz ({err:+.2} cents)"
            );
        }
    }
}

#[test]
fn the_init_patch_plays_in_tune() {
    // Hyrule Fanfare: GB wave, sweep from above over 180 ms, delayed vibrato, 3-voice unison
    // (all shaped by the factory macros). Notes of 0.6 s, measured once the vibrato is at full
    // depth over more than a vibrato cycle (unison and vibrato average out).
    let events: Vec<At> = MELODY
        .iter()
        .enumerate()
        .flat_map(|(i, n)| [on(secs(SR, i as f32 * 0.7), *n, 0.8), off(secs(SR, i as f32 * 0.7 + 0.6), *n)])
        .collect();
    let r = Session::new(&init_state(), SR).render(&events, secs(SR, 3.6), 512);
    assert!(r.all_finite());
    assert!(r.peak() > 0.1 && r.peak() <= CEILING);
    let x = r.mono();
    for (i, note) in MELODY.iter().enumerate() {
        let t = i as f32 * 0.7;
        let expected = note_hz(*note as f32);
        let f = acf_pitch(&x[secs(SR, t + 0.3)..secs(SR, t + 0.58)], SR, expected);
        let err = cents(f, expected);
        assert!(err.abs() < 25.0, "note {note}: {f:.2} Hz vs {expected:.2} Hz ({err:+.1} cents)");
    }
    // The sweep is audible: right after the note-on the pitch is far above the note.
    let f0 = acf_pitch(&x[secs(SR, 0.005)..secs(SR, 0.035)], SR, note_hz(60.0) * 1.6);
    assert!(cents(f0, note_hz(60.0)) > 300.0, "sweep start {f0:.1} Hz");
}

#[test]
fn notes_end_cleanly_and_the_instrument_falls_silent() {
    let r = Session::new(&init_state(), SR).render(&[on(0, 64, 0.8), off(secs(SR, 0.5), 64)], secs(SR, 2.5), 512);
    // Release is 150 ms: sound continues for a while after the note-off, then decays fully.
    // What remains afterwards is the subsonic settling of the DMG output capacitor model
    // (28 Hz) and the 5 Hz DC blocker: below -60 dB after 150 ms, below -80 dB after 0.5 s.
    assert!(rms(&r.left[secs(SR, 0.52)..secs(SR, 0.56)]) > 1e-3, "release tail missing");
    let after = |t: f32| peak(&r.left[secs(SR, t)..]).max(peak(&r.right[secs(SR, t)..]));
    assert!(after(0.8) < 1e-3, "{}", after(0.8));
    assert!(after(1.15) < 1e-4, "{}", after(1.15));
    let t = r.telemetry_at(secs(SR, 2.4));
    assert_eq!(t.active_voices, 0);
    assert_eq!(t.last_note, None);
    assert!(!t.is_held(64));
    assert_eq!(r.status_at(secs(SR, 2.4)), BlockStatus::Silent);
}

/// Largest sample-to-sample step in `x[a..b]`.
fn max_step(x: &[f32], a: usize, b: usize) -> f32 {
    x[a.max(1)..b.min(x.len())].iter().zip(&x[a.max(1) - 1..]).fold(0.0f32, |m, (v, u)| m.max((v - u).abs()))
}

#[test]
fn no_clicks_at_note_boundaries() {
    // A smooth waveform (FREE wave channel playing a sine) makes any discontinuity stand out.
    let mut st = clean_patch(2.0, Channel::Wave);
    st.wave = wave::sine();
    st.values.set(ParamId::WvRelease, 30.0);
    st.values.set(ParamId::Polyphony, 1.0); // the second note steals the first
    let s = |t: f32| secs(SR, t);
    let events = [
        on(s(0.0), 60, 1.0),
        off(s(0.3), 60),
        on(s(0.6), 64, 1.0),
        on(s(0.8), 67, 1.0),
        off(s(1.0), 67),
        on(s(1.2), 67, 1.0),
        off(s(1.21), 67), // released during its attack
    ];
    let r = Session::new(&st, SR).render(&events, s(1.5), 512);
    let x = r.mono();
    // The waveform's own largest step, measured on the highest note while it is held.
    let steady = max_step(&x, s(0.85), s(0.95)).max(max_step(&x, s(0.1), s(0.25)));
    assert!(steady > 1e-3);
    for boundary in [0.0f32, 0.3, 0.33, 0.6, 0.8, 1.0, 1.03, 1.2, 1.21, 1.24] {
        let (a, b) = (s((boundary - 0.004).max(0.0)), s(boundary + 0.006));
        let step = max_step(&x, a, b);
        // 1.5×: an envelope or fade step may coincide with one of the waveform's own steps.
        // A hard cut at full level would be ≈ 3× (half the waveform's peak-to-peak jump).
        assert!(step <= 1.5 * steady, "click at {boundary} s: step {step:.5} vs waveform {steady:.5}");
    }
    // Nothing audible after the last release (only the 5 Hz DC blocker settling).
    assert!(peak(&x[s(1.35)..]) < 1e-3);
}

#[test]
fn a_loud_dense_chord_never_exceeds_the_ceiling() {
    let mut st = init_state();
    for ch in Channel::ALL {
        st.values.set(ParamId::channel(*ch, ChannelField::On), 1.0);
        st.values.set(ParamId::channel(*ch, ChannelField::Level), 15.0);
        st.values.set(ParamId::channel(*ch, ChannelField::Sustain), 15.0);
    }
    st.values.set(ParamId::Polyphony, 16.0);
    st.values.set(ParamId::UniVoices, 4.0);
    st.values.set(ParamId::OutGain, 12.0);
    st.values.set(ParamId::EchoOn, 1.0);
    st.values.set(ParamId::EchoFb, 95.0);
    st.values.set(ParamId::EchoMix, 100.0);
    for chip in [0.0, 1.0, 2.0] {
        st.values.set(ParamId::Chip, chip);
        let events: Vec<At> = (0..16).map(|i| on(i * 3, 36 + 3 * i as u8, 1.0)).collect();
        let r = Session::new(&st, SR).render(&events, secs(SR, 1.0), 512);
        assert!(r.all_finite());
        assert!(r.peak() <= CEILING, "chip {chip}: peak {}", r.peak());
        assert!(r.peak() > 0.9, "chip {chip}: the chord should drive the clipper");
    }
}

#[test]
fn sustain_pedal_holds_notes_through_the_midi_mapping() {
    let mut st = clean_patch(2.0, Channel::P1);
    st.values.set(ParamId::P1Release, 50.0);
    let s = |t: f32| secs(SR, t);
    let mut ev = vec![on(0, 69, 0.8)];
    ev.extend(midi(s(0.1), cc(64, 127)));
    ev.push(off(s(0.2), 69));
    ev.extend(midi(s(0.6), cc(64, 0)));
    let r = Session::new(&st, SR).render(&ev, s(1.0), 512);
    let held = rms(&r.left[s(0.05)..s(0.15)]);
    assert!(rms(&r.left[s(0.45)..s(0.55)]) > 0.7 * held, "the pedal did not hold the note");
    assert!(r.telemetry_at(s(0.5)).is_held(69), "sustained notes count as held");
    assert!(peak(&r.left[s(0.75)..]) < 1e-4, "pedal up did not release the note");
}

#[test]
fn pitch_bend_and_controller_reset() {
    let st = clean_patch(2.0, Channel::P1); // pb_range defaults to ±2 semitones
    let s = |t: f32| secs(SR, t);
    let mut ev = vec![on(0, 69, 0.8)];
    ev.extend(midi(s(0.2), bend(16383)));
    ev.extend(midi(s(0.5), bend(0)));
    ev.extend(midi(s(0.8), bend(8192)));
    ev.extend(midi(s(1.1), bend(16383)));
    ev.extend(midi(s(1.4), cc(121, 0))); // reset all controllers
    let r = Session::new(&st, SR).render(&ev, s(1.7), 256);
    let x = r.mono();
    let check = |from: f32, semis: f32| {
        let expected = note_hz(69.0 + semis);
        let f = pitch_in(&x, SR, from, from + 0.12, expected);
        assert!(cents(f, expected).abs() < 1.0, "at {from} s: {f:.3} Hz vs {expected:.3} Hz");
    };
    check(0.05, 0.0);
    check(0.27, 2.0);
    check(0.57, -2.0);
    check(0.87, 0.0);
    check(1.17, 2.0);
    check(1.47, 0.0);
}

/// Pitch spread (max − min, cents) over consecutive short windows of `x[from..to]`.
fn pitch_spread(x: &[f32], from: f32, to: f32, guess: f32) -> f32 {
    let mut fs = Vec::new();
    let mut t = from;
    while t + 0.03 <= to {
        fs.push(cents(pitch_in(x, SR, t, t + 0.03, guess), guess));
        t += 0.02;
    }
    fs.iter().cloned().fold(f32::MIN, f32::max) - fs.iter().cloned().fold(f32::MAX, f32::min)
}

#[test]
fn mod_wheel_and_aftertouch_add_vibrato_even_with_the_vibrato_tile_off() {
    let st = clean_patch(2.0, Channel::P1); // vib_on off
    let a4 = note_hz(69.0);
    let plain = Session::new(&st, SR).render(&[on(0, 69, 0.8)], secs(SR, 1.0), 512).mono();
    assert!(pitch_spread(&plain, 0.5, 0.9, a4) < 2.0);
    for event in [cc(1, 127), NoteEvent::MidiChannelPressure { timing: 0, channel: 0, pressure: 1.0 }] {
        let mut ev = vec![on(0, 69, 0.8)];
        ev.extend(midi(0, event));
        let x = Session::new(&st, SR).render(&ev, secs(SR, 1.0), 512).mono();
        let spread = pitch_spread(&x, 0.5, 0.9, a4);
        assert!(spread > 30.0, "{event:?}: vibrato spread only {spread:.1} cents");
    }
}

#[test]
fn all_sound_off_and_all_notes_off() {
    let mut st = clean_patch(2.0, Channel::P1);
    st.values.set(ParamId::P1Release, 400.0);
    let s = |t: f32| secs(SR, t);
    let chord = [on(0, 60, 0.8), on(0, 64, 0.8), on(0, 67, 0.8)];

    // CC 120: silence now (a short anti-click fade at most).
    let mut ev = chord.to_vec();
    ev.extend(midi(s(0.2), cc(120, 0)));
    let r = Session::new(&st, SR).render(&ev, s(0.8), 512);
    // A 2 ms anti-click fade, then only the 5 Hz DC blocker settling (below -40 dB at once,
    // below -80 dB within 0.3 s).
    let after = |t: f32| peak(&r.left[s(t)..]).max(peak(&r.right[s(t)..]));
    assert!(after(0.203) < 1e-2, "CC120 left sound: {}", after(0.203));
    assert!(after(0.5) < 1e-4, "{}", after(0.5));
    assert_eq!(r.telemetry_at(s(0.25)).active_voices, 0);

    // CC 123: release every note (the 400 ms release still sounds).
    let mut ev = chord.to_vec();
    ev.extend(midi(s(0.2), cc(123, 0)));
    let r = Session::new(&st, SR).render(&ev, s(1.2), 512);
    assert!(rms(&r.left[s(0.3)..s(0.35)]) > 1e-2, "CC123 must release, not cut");
    assert!(peak(&r.left[s(0.9)..]) < 1e-3);
    for n in [60, 64, 67] {
        assert!(!r.telemetry_at(s(0.3)).is_held(n));
    }
}

#[test]
fn all_sound_off_does_not_thump_in_the_analog_chip_modes() {
    // SPEC §4.7/§4.8: cutting voices (all-sound-off, stealing) is click-free in every mode. With
    // `analog` on, NES/GB voices carry the unipolar DAC's DC; dropping it within the 2 ms kill
    // fade made the chip high-pass ring at almost full scale (0.97 in GB). The voice's DC is
    // held through the fade and discharged smoothly instead.
    let s = |t: f32| secs(SR, t);
    for (chip, ch, limit) in [(1.0, Channel::Wave, 0.15), (1.0, Channel::P1, 0.15), (0.0, Channel::P1, 0.03)] {
        let mut st = clean_patch(chip, ch);
        st.values.set(ParamId::Analog, 1.0);
        let mut ev = vec![on(0, 48, 1.0), on(0, 55, 1.0), on(0, 60, 1.0)];
        ev.extend(midi(s(0.3), cc(120, 0)));
        let r = Session::new(&st, SR).render(&ev, s(1.5), 512);
        let window = |a: f32, b: f32| peak(&r.left[s(a)..s(b)]).max(peak(&r.right[s(a)..s(b)]));
        assert!(window(0.1, 0.3) > 0.5, "chip {chip} {ch:?}: the chord plays");
        let thump = window(0.303, 0.35);
        assert!(thump < limit, "chip {chip} {ch:?}: {thump} left after all-sound-off");
        // The discharge itself is subsonic (100 ms time constant) and fades out completely.
        assert!(window(1.0, 1.5) < 1e-4, "chip {chip} {ch:?}: the discharge ends");
    }
}

#[test]
fn macros_reach_the_effect_chain() {
    // Factory routing: macro 4 (SPACE) → echo_mix +0.5, uni_width +0.4.
    let mut st = init_state();
    st.values.set(ParamId::EchoOn, 1.0);
    st.values.set(ParamId::EchoSync, 0.0);
    st.values.set(ParamId::EchoTime, 120.0);
    let tail_energy = |macro4: f32| {
        let mut st = st.clone();
        st.values.set(ParamId::Macro4, macro4);
        let mut s = Session::new(&st, SR);
        let r = s.render(&[on(0, 64, 0.8), off(secs(SR, 0.2), 64)], secs(SR, 1.2), 512);
        let eff = *s.processor.effective_values();
        (rms(&r.mono()[secs(SR, 0.45)..secs(SR, 1.1)]), eff)
    };
    let (dry, eff0) = tail_energy(0.0);
    let (wet, eff1) = tail_energy(100.0);
    assert_eq!(eff0.get(ParamId::EchoMix), 25.0);
    assert!((eff1.get(ParamId::EchoMix) - 75.0).abs() < 1e-3);
    assert!((eff1.get(ParamId::UniWidth) - 100.0).abs() < 1e-3);
    assert!(dry > 1e-4, "echo tail missing");
    assert!(db(wet / dry) > 6.0, "SPACE at 100 % should make the echo much louder: {:.1} dB", db(wet / dry));

    // Routings edited at runtime (the GUI's path) are picked up on the next block.
    let mut s = Session::new(&st, SR);
    s.render(&[], 256, 256);
    let mut a = MacroAssignments::empty();
    a.set(0, ParamId::OutGain, -0.5);
    s.macros.store(&a);
    s.values.set(ParamId::Macro1, 100.0);
    s.render(&[], 256, 256);
    assert!((s.processor.effective_values().get(ParamId::OutGain) + 24.0).abs() < 1e-3);
}

#[test]
fn crush_at_16_bits_is_transparent_on_the_init_patch() {
    // SPEC §5.2 (FX-01): the init patch (GB, `analog` on) feeds unipolar DAC levels into the
    // crusher; a chord reaches well above +1 there. At 16 bits / 48 kHz the crusher must be
    // transparent (its wet path is one sample late), not clip the chord.
    let chord = [on(0, 60, 0.9), on(0, 64, 0.9), on(0, 67, 0.9)];
    let render = |crush: bool| {
        let mut st = init_state();
        st.values.set(ParamId::CrushOn, if crush { 1.0 } else { 0.0 });
        st.values.set(ParamId::CrushBits, 16.0);
        st.values.set(ParamId::CrushRate, 48_000.0);
        Session::new(&st, SR).render(&chord, secs(SR, 1.0), 256).mono()
    };
    let (dry, wet) = (render(false), render(true));
    let (a, b) = (secs(SR, 0.1), secs(SR, 1.0));
    let level = db(rms(&wet[a..b]) / rms(&dry[a..b]));
    assert!(level.abs() < 0.1, "crush at 16 bits changes the chord's level by {level:.2} dB");
    let err = wet[a..b].iter().zip(&dry[a - 1..b - 1]).fold(0.0f32, |m, (w, d)| m.max((w - d).abs()));
    assert!(err < 2e-4, "crush at 16 bits distorts the chord (max deviation {err:e})");
}

#[test]
fn dirt_macro_never_silences_the_crusher() {
    // SPEC §5.2 (FX-02): the factory DIRT macro lowers crush_bits to 1 bit at full travel.
    // 1 bit is the harshest setting (a square), not silence: the level must not collapse
    // anywhere along the macro (before: −10.9 dB at 80 %, −71.6 dB at 100 %).
    let level = |dirt: f32| {
        let mut st = init_state();
        st.values.set(ParamId::CrushOn, 1.0);
        st.values.set(ParamId::Macro2, dirt);
        let r = Session::new(&st, SR).render(&[on(0, 60, 0.9)], secs(SR, 0.8), 256);
        db(rms(&r.mono()[secs(SR, 0.2)..secs(SR, 0.8)]))
    };
    let levels: Vec<f32> = [0.0, 30.0, 60.0, 80.0, 90.0, 100.0].iter().map(|d| level(*d)).collect();
    println!("DIRT 0/30/60/80/90/100 %: {levels:.1?} dB");
    assert!(levels.iter().all(|l| *l > -30.0), "{levels:?}");
    assert!((levels[5] - levels[3]).abs() < 6.0, "1 bit vs 80 %: {levels:?}");
}

/// Second-order (RBJ) filter section for the low-frequency transient measurements.
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(sr: f32, f0: f64, high_pass: bool) -> Self {
        let w = std::f64::consts::TAU * f0 / sr as f64;
        let alpha = w.sin() / std::f64::consts::SQRT_2;
        let c = w.cos();
        let a0 = 1.0 + alpha;
        let b = if high_pass {
            [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0]
        } else {
            [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0]
        };
        Self { b: b.map(|v| v / a0), a: [-2.0 * c / a0, (1.0 - alpha) / a0], z: [0.0; 2] }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The part of `x` an ear hears as a thump: 15–150 Hz (2nd-order high-pass, 4th-order
/// low-pass), i.e. without the steady tone above it and the inaudible drift below.
fn thump_band(x: &[f32], sr: f32) -> Vec<f32> {
    let mut f = [Biquad::new(sr, 15.0, true), Biquad::new(sr, 150.0, false), Biquad::new(sr, 150.0, false)];
    x.iter().map(|v| f.iter_mut().fold(*v as f64, |s, b| b.run(s)) as f32).collect()
}

#[test]
fn chip_mode_and_analog_switches_do_not_thump() {
    // SPEC §4.8/§5.3 (FX-03): switching between the bipolar (FREE, analog off) and unipolar
    // (NES/GB with analog on) DAC changes the signal's DC level. Inside the engine's 2 + 2 ms
    // switch crossfade that DC step turned into a low thump behind the HPFs (≈ 12 % of the
    // signal's peak). The engine now bridges each voice's DC level across the crossfade and lets
    // it settle slowly; the chip filter releases its high-pass gradually when switching to none.
    let s = |t: f32| secs(SR, t);
    let cases: [(&str, f32, f32, f32, f32); 6] = [
        ("FREE → GB", 2.0, 1.0, 1.0, 1.0),
        ("GB → FREE", 1.0, 1.0, 2.0, 1.0),
        ("GB analog off → on", 1.0, 0.0, 1.0, 1.0),
        ("FREE → NES", 2.0, 1.0, 0.0, 1.0),
        ("NES → GB", 0.0, 1.0, 1.0, 1.0),
        ("NES analog on → off", 0.0, 1.0, 0.0, 0.0),
    ];
    for (name, chip0, analog0, chip1, analog1) in cases {
        let mut st = clean_patch(chip0, Channel::P1);
        st.values.set(ParamId::P1Level, 10.0);
        st.values.set(ParamId::Analog, analog0);
        let mut session = Session::new(&st, SR);
        let a = session.render(&[on(0, 72, 0.9)], s(0.5), 64);
        session.values.set(ParamId::Chip, chip1);
        session.values.set(ParamId::Analog, analog1);
        let b = session.render(&[], s(0.5), 64);
        let x: Vec<f32> = a.mono().into_iter().chain(b.mono()).collect();
        let band = thump_band(&x, SR);
        let before = peak(&band[s(0.3)..s(0.5)]);
        let after = peak(&band[s(0.5)..s(0.8)]);
        let level = peak(&x[s(0.3)..s(0.5)]).max(peak(&x[s(0.8)..]));
        println!("{name}: 15–150 Hz band {before:.4} before, {after:.4} after the switch (signal peak {level:.2})");
        // `before` is the tone's own leakage into the band; the switch may add at most 3 % of
        // the signal's peak (before: 25–37 %).
        assert!(after - before < 0.03 * level, "{name}: thump of {after:.4} against a signal peak of {level:.2}");
        assert!(rms(&b.mono()[s(0.1)..]) > 0.05, "{name}: sounds after the switch");
    }
}

#[test]
fn telemetry_reflects_the_playing_note() {
    let s = |t: f32| secs(SR, t);
    let r = Session::new(&init_state(), SR).render(&[on(0, 64, 0.9), off(s(0.5), 64)], s(3.0), 512);
    let t = r.telemetry_at(s(0.3));
    assert!(t.active_voices >= 1);
    assert!(t.is_held(64) && !t.is_held(65));
    assert_eq!(t.last_note, Some(64));
    assert!(t.peak_db[0] > -30.0 && t.rms_db[0] > -40.0, "{t:?}");
    assert!(t.peak_hold_db > -30.0);
    assert!(t.frame > 0);
    assert!(t.wave_step.is_some());
    assert!(t.env_level[2] > 0.1, "wave envelope {:?}", t.env_level);
    let end = r.telemetry_at(s(2.9));
    assert_eq!(end.active_voices, 0);
    assert!(!end.is_held(64));
    assert!(end.peak_db[0] < -60.0, "meter release: {:?}", end.peak_db);
    assert!(end.frame > t.frame);
}

#[test]
fn echo_tail_is_reported_then_the_plugin_goes_quiet() {
    let mut st = init_state();
    st.values.set(ParamId::EchoOn, 1.0);
    st.values.set(ParamId::EchoSync, 0.0);
    st.values.set(ParamId::EchoTime, 150.0);
    st.values.set(ParamId::EchoFb, 60.0);
    let s = |t: f32| secs(SR, t);
    let r = Session::new(&st, SR).render(&[on(0, 64, 0.8), off(s(0.2), 64)], s(12.0), 512);
    assert_eq!(r.status_at(s(0.1)), BlockStatus::Active);
    let tail_seen = r.blocks.iter().any(|b| matches!(b.1, BlockStatus::Tail(n) if n > 0));
    assert!(tail_seen, "no tail reported while the echo decays");
    assert_eq!(r.status_at(s(11.9)), BlockStatus::Silent);
    assert!(peak(&r.left[s(11.0)..]) == 0.0 && peak(&r.right[s(11.0)..]) == 0.0);
}

#[test]
fn synced_echo_follows_the_host_tempo() {
    // A short blip, 100 % wet single repeat at 1/4 note.
    let mut st = clean_patch(2.0, Channel::P1);
    st.values.set(ParamId::P1Attack, 0.0);
    st.values.set(ParamId::P1Decay, 20.0);
    st.values.set(ParamId::P1Sustain, 0.0);
    st.values.set(ParamId::P1Release, 0.0);
    st.values.set(ParamId::EchoOn, 1.0);
    st.values.set(ParamId::EchoSync, 1.0);
    st.values.set(ParamId::EchoDiv, 0.0); // 1/4
    st.values.set(ParamId::EchoFb, 0.0);
    st.values.set(ParamId::EchoMix, 100.0);
    st.values.set(ParamId::EchoFir, 0.0); // FLAT
    st.values.set(ParamId::EchoWidth, 0.0);
    for bpm in [120.0f64, 90.0, 150.0] {
        let mut s = Session::new(&st, SR);
        s.transport = Transport { tempo_bpm: bpm, playing: true, beat_pos: Some(0.0) };
        let r = s.render(&[on(0, 81, 1.0), off(secs(SR, 0.03), 81)], secs(SR, 1.5), 512);
        let x = r.mono();
        let from = secs(SR, 0.2);
        let loudest = from
            + x[from..]
                .iter()
                .enumerate()
                .fold((0, 0.0f32), |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m })
                .0;
        let expected = 60.0 / bpm as f32;
        let at = loudest as f32 / SR;
        assert!((at - expected).abs() < 0.02, "{bpm} BPM: repeat at {at:.3} s, expected ≈ {expected:.3} s");
    }
}

#[test]
fn every_supported_sample_rate_plays_in_tune() {
    let st = clean_patch(2.0, Channel::P1);
    for sr in [22_050.0f32, 32_000.0, 44_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0, 192_000.0] {
        let r = Session::new(&st, sr).render(&[on(0, 69, 0.8)], secs(sr, 0.5), 512);
        assert!(r.all_finite() && r.peak() <= CEILING);
        let f = pitch_in(&r.mono(), sr, 0.1, 0.45, 440.0);
        assert!(cents(f, 440.0).abs() < 1.0, "{sr} Hz: A4 plays at {f:.3} Hz");
    }
    // Outside what the DSP can render: silence, never garbage.
    for sr in [1_234.5f32, 4_000.0, 1_000_000.0] {
        let r = Session::new(&st, sr).render(&[on(0, 69, 0.8)], 4096, 512);
        assert!(r.left.iter().chain(&r.right).all(|x| *x == 0.0), "{sr} Hz");
    }
}

#[test]
fn every_factory_preset_renders_audible_non_clipping_c4() {
    let lib = factory_library();
    assert!(lib.warnings.is_empty(), "{:?}", lib.warnings);
    assert!(!lib.presets.is_empty());
    for f in &lib.presets {
        let r =
            Session::new(&f.state, SR).render(&[on(0, 60, 100.0 / 127.0), off(secs(SR, 1.0), 60)], secs(SR, 1.5), 512);
        let name = format!("{:03} {}", f.preset.number, f.preset.name);
        assert!(r.all_finite(), "{name}: non-finite samples");
        let pk = r.peak();
        assert!(pk > 10f32.powf(-30.0 / 20.0), "{name}: inaudible ({:.1} dBFS)", db(pk));
        assert!(pk <= CEILING, "{name}: peak {pk}");
    }
}

#[test]
fn random_parameters_and_events_never_produce_garbage() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    for round in 0..40 {
        let mut values = ParamValues::defaults();
        for spec in PARAMS.iter() {
            if rng.unit() < 0.7 {
                values.set(spec.id, spec.denormalize(rng.unit()));
            }
        }
        let mut macros = MacroAssignments::empty();
        for m in 0..4 {
            for _ in 0..rng.below(6) {
                let target = PARAMS[rng.below(PARAMS.len())].id;
                macros.set(m, target, rng.unit() * 2.0 - 1.0);
            }
        }
        let mut w = [0u8; 32];
        w.iter_mut().for_each(|v| *v = rng.below(16) as u8);
        let state = PresetState { values, wave: w, macros };
        let total = secs(SR, 0.4);
        let mut events = Vec::new();
        for _ in 0..rng.below(40) {
            let at = rng.below(total);
            let note = rng.below(128) as u8;
            match rng.below(10) {
                0..=3 => events.push(on(at, note, rng.unit())),
                4..=6 => events.push(off(at, note)),
                7 => events.extend(midi(at, bend(rng.below(16384) as u16))),
                8 => events.extend(midi(at, cc([1, 64, 120, 121, 123][rng.below(5)], rng.below(128) as u8))),
                _ => events
                    .extend(midi(at, NoteEvent::MidiChannelPressure { timing: 0, channel: 0, pressure: rng.unit() })),
            }
        }
        let block = [1, 31, 64, 200, 512, 2048][rng.below(6)];
        let mut s = Session::new(&state, SR);
        s.transport = Transport { tempo_bpm: 20.0 + rng.unit() as f64 * 400.0, playing: true, beat_pos: Some(0.0) };
        let r = s.render(&events, total, block);
        assert!(r.all_finite(), "round {round}: non-finite output");
        assert!(r.peak() <= CEILING, "round {round}: peak {}", r.peak());
        assert_eq!(s.processor.recoveries(), 0, "round {round}: the output guard had to step in");
    }
}
