//! Engine behaviour (SPEC §4.3–§4.11): measured on rendered audio and telemetry.

mod common;
use common::*;
use pixl_core::{ArpChord, ArpMode, ChipMode, Curve, GlideMode, ParamId, ParamValues, TelemetrySnapshot, VoiceMode};
use pixl_dsp::analysis::{db, estimate_frequency, peak, remove_dc, rms};
use pixl_dsp::{Engine, EngineEvent};

const FS: f32 = 48_000.0;

/// Frequency from rising zero crossings (linear interpolation) after DC removal.
fn zc_freq(x: &[f32], fs: f32) -> f64 {
    let x = remove_dc(x);
    let mut first = None;
    let mut last = 0.0;
    let mut count = 0;
    for i in 1..x.len() {
        if x[i - 1] < 0.0 && x[i] >= 0.0 {
            let t = (i - 1) as f64 + (-x[i - 1] / (x[i] - x[i - 1])) as f64;
            if first.is_none() {
                first = Some(t);
            } else {
                count += 1;
            }
            last = t;
        }
    }
    match first {
        Some(f) if count > 0 => count as f64 * fs as f64 / (last - f),
        _ => 0.0,
    }
}

fn telemetry(e: &Engine) -> TelemetrySnapshot {
    let mut t = TelemetrySnapshot::default();
    e.fill_telemetry(&mut t);
    t
}

/// Amplitude envelope: RMS of `win`-sample windows, normalised to a 50 % pulse's RMS (0.5).
fn envelope(x: &[f32], win: usize) -> Vec<f64> {
    x.chunks(win).map(|c| rms(&remove_dc(c)) / 0.5).collect()
}

// ---------------------------------------------------------------------------------------
// Pitch and register quantization
// ---------------------------------------------------------------------------------------

#[test]
fn register_quantization_in_rendered_audio() {
    // A4 on each chip: NES pulse t = 253 → 440.40 Hz, GB pulse x = 1750 → 439.83 Hz,
    // NES triangle t = 126 → 440.40 Hz, GB wave x = 1899 → 439.84 Hz, FREE exact.
    let cases = [
        (ChipMode::Nes, 0, 1_789_773.0 / (16.0 * 254.0)),
        (ChipMode::Gb, 0, 131_072.0 / (2048.0 - 1750.0)),
        (ChipMode::Free, 0, 440.0),
        (ChipMode::Nes, 2, 1_789_773.0 / (32.0 * 127.0)),
        (ChipMode::Gb, 2, 65_536.0 / (2048.0 - 1899.0)),
        (ChipMode::Free, 2, 440.0),
    ];
    for (chip, ch, expect) in cases {
        let mut rig = Rig::new(FS, clean(chip, ch));
        rig.on(69, 1.0);
        rig.left(secs(FS, 0.1));
        let x = rig.left(1 << 16);
        let f = estimate_frequency(&x, FS as f64).unwrap();
        println!("{chip:?} ch{ch}: {f:.3} Hz (expected {expect:.3})");
        assert!((f - expect).abs() < 0.02, "{chip:?} ch{ch}: {f} vs {expect}");
    }
}

#[test]
fn pitch_is_exact_from_the_first_period() {
    // No settling: the very first periods after note-on already have the final length (C4,
    // modulation off). Averaged over 8 periods to cancel the sub-sample bias of linear
    // zero-crossing interpolation on band-limited edges.
    let cases = [
        (ChipMode::Free, 0, hz(60.0)),
        (ChipMode::Nes, 0, 1_789_773.0 / (16.0 * 428.0)),
        (ChipMode::Gb, 0, 131_072.0 / (2048.0 - 1547.0)),
        (ChipMode::Free, 2, hz(60.0)),
    ];
    for (chip, ch, expect) in cases {
        let mut p = clean(chip, ch);
        p.set(ParamId::Analog, 0.0);
        let mut rig = Rig::new(FS, p);
        if ch == 2 {
            rig.e.set_wave(&pixl_core::wave::square());
        }
        rig.on(60, 1.0);
        let x = rig.left(secs(FS, 0.05));
        let crossings: Vec<f64> = (1..x.len())
            .filter(|&i| x[i - 1] < 0.0 && x[i] >= 0.0)
            .map(|i| (i - 1) as f64 + (-x[i - 1] / (x[i] - x[i - 1])) as f64)
            .collect();
        assert!(crossings.len() > 9, "{chip:?} ch{ch}: too few periods");
        let period = (crossings[8] - crossings[0]) / 8.0;
        let expected = FS as f64 / expect;
        println!("{chip:?} ch{ch}: first 8 periods {period:.3} samples (expected {expected:.3})");
        assert!((period - expected).abs() < 0.05, "{chip:?} ch{ch}: {period} vs {expected}");
    }
}

#[test]
fn nes_low_notes_clamp_and_high_notes_mute() {
    // Below the 11-bit range the NES pulse plays its lowest note (t = 2047 → 54.6 Hz).
    let mut rig = Rig::new(FS, clean(ChipMode::Nes, 0));
    rig.on(24, 1.0); // C1 = 32.7 Hz
    rig.left(secs(FS, 0.1));
    let f = zc_freq(&rig.left(secs(FS, 1.0)), FS);
    assert!((f - 54.64).abs() < 0.1, "{f}");
    // Above ≈ 13.2 kHz (t < 8) the sweep unit mutes the channel.
    let mut rig = Rig::new(FS, clean(ChipMode::Nes, 0));
    rig.on(127, 1.0); // 12.5 kHz → t = 8: still plays
    rig.left(secs(FS, 0.05));
    assert!(rms(&rig.left(4800)) > 0.1);
    let mut rig = Rig::new(FS, clean(ChipMode::Nes, 0));
    rig.set(ParamId::P1Coarse, 2.0); // 14 kHz → muted
    rig.on(127, 1.0);
    rig.left(secs(FS, 0.05));
    assert!(rms(&remove_dc(&rig.left(4800))) < 1e-3);
}

#[test]
fn pitch_bend_tune_and_coarse() {
    let mut rig = Rig::new(FS, clean(ChipMode::Free, 0));
    rig.set(ParamId::PbRange, 7.0);
    rig.set(ParamId::Tune, 50.0);
    rig.set(ParamId::P1Coarse, -12.0);
    rig.set(ParamId::P1Fine, -25.0);
    rig.ev(EngineEvent::PitchBend { value: 1.0 });
    rig.on(69, 1.0);
    rig.left(secs(FS, 0.1));
    let f = estimate_frequency(&rig.left(1 << 15), FS as f64).unwrap();
    let expect = hz(69.0 + 7.0 + 0.5 - 12.0 - 0.25);
    assert!((f - expect).abs() < 0.05, "{f} vs {expect}");
}

// ---------------------------------------------------------------------------------------
// Levels, calibration, mixing
// ---------------------------------------------------------------------------------------

/// RMS (dB) of one channel at level 15 in a chip mode (DC removed).
fn channel_rms_db(chip: ChipMode, ch: usize, analog: bool, tweak: impl Fn(&mut Rig)) -> (f64, f64) {
    let mut rig = Rig::new(FS, clean(chip, ch));
    rig.set(ParamId::Analog, if analog { 1.0 } else { 0.0 });
    tweak(&mut rig);
    rig.on(57, 1.0);
    rig.left(secs(FS, 0.2));
    let x = remove_dc(&rig.left(secs(FS, 1.0)));
    (db(rms(&x)), db(peak(&x)))
}

#[test]
fn calibration_one_channel_at_level_15() {
    // SPEC §4.3: a level-15 channel is −6 dBFS nominal peak (1.0 peak-to-peak) in every chip
    // mode. Measured by RMS against the ideal waveform's RMS so the band-limited overshoot of
    // sharp edges does not count as level.
    let wave_rms_db = db(0.5 * (21.25f64).sqrt() / 7.5); // 32-step triangle wave RAM, pp 1.0
    let tri = |rig: &mut Rig| {
        let w: Vec<u8> = (0..32).map(|i| if i < 16 { 15 - i } else { i - 16 } as u8).collect();
        rig.e.set_wave(&w.try_into().unwrap());
    };
    for analog in [true, false] {
        for chip in [ChipMode::Nes, ChipMode::Gb, ChipMode::Free] {
            let (p, pk) = channel_rms_db(chip, 0, analog, |_| {});
            println!("{chip:?} analog {analog}: pulse 50 % rms {p:.2} dB (sample peak {pk:.2} dBFS)");
            assert!((p + 6.02).abs() < 0.3, "{chip:?} pulse {p}");
            let (w, pk) = channel_rms_db(chip, 2, analog, tri);
            println!("{chip:?} analog {analog}: wave/triangle rms {w:.2} dB (sample peak {pk:.2} dBFS)");
            if chip == ChipMode::Nes {
                // The volume-less NES triangle keeps its authentic 2A03 weight (1.65 × a
                // pulse's swing: equal RMS to a full pulse).
                let expect = wave_rms_db + db(pixl_dsp::chip::nes_triangle_weight() as f64);
                assert!((w - expect).abs() < 0.3, "{chip:?} triangle {w} vs {expect}");
            } else {
                assert!((w - wave_rms_db).abs() < 0.3, "{chip:?} wave {w} vs {wave_rms_db}");
            }
            // Noise at a slow clock (band-limiting removes little): ±0.5 random → −6 dB RMS.
            let (n, pk) = channel_rms_db(chip, 3, analog, |rig| rig.set(ParamId::NsPeriod, 13.0));
            println!("{chip:?} analog {analog}: noise rms {n:.2} dB (sample peak {pk:.2} dBFS)");
            let expect =
                if chip == ChipMode::Nes { -6.02 + db(pixl_dsp::chip::nes_noise_weight() as f64) } else { -6.02 };
            assert!((n - expect).abs() < 1.5, "{chip:?} noise {n} vs {expect}");
        }
    }
}

#[test]
fn nes_mixer_is_non_linear() {
    // Two pulses at 15 in NES (analog) compress: < 2× one pulse; GB sums linearly.
    let one = |chip: ChipMode, both: bool| {
        let mut p = clean(chip, 0);
        if both {
            p.set(ParamId::P2On, 1.0);
            p.set(ParamId::P2Coarse, 0.0);
        }
        let mut rig = Rig::new(FS, p);
        rig.on(57, 1.0);
        rig.left(secs(FS, 0.2));
        rms(&remove_dc(&rig.left(secs(FS, 0.5))))
    };
    let nes = one(ChipMode::Nes, true) / one(ChipMode::Nes, false);
    let gb = one(ChipMode::Gb, true) / one(ChipMode::Gb, false);
    println!("two pulses / one pulse: NES {nes:.3}, GB {gb:.3}");
    assert!((gb - 2.0).abs() < 0.02, "{gb}");
    // 2A03: pulse_mix(30)/pulse_mix(15) = 1.730.
    assert!((nes - 1.730).abs() < 0.03, "{nes}");
}

#[test]
fn velocity_sensitivity() {
    let level = |vel: f32, sens: f32| {
        let mut rig = Rig::new(FS, clean(ChipMode::Free, 0));
        rig.set(ParamId::VelSens, sens);
        rig.on(60, vel);
        rig.left(secs(FS, 0.05));
        rms(&remove_dc(&rig.left(secs(FS, 0.2))))
    };
    let full = level(1.0, 100.0);
    assert!((level(0.5, 100.0) / full - 0.5).abs() < 0.01);
    assert!((level(0.5, 50.0) / full - 0.75).abs() < 0.01);
    assert!((level(0.2, 0.0) / full - 1.0).abs() < 0.01);
    // Velocity 0 is a note-off.
    let mut rig = Rig::new(FS, clean(ChipMode::Free, 0));
    rig.on(60, 1.0);
    rig.left(1000);
    rig.on(60, 0.0);
    assert!(rig.e.voices().all(|v| !v.gate));
}

#[test]
fn gb_wave_volume_shift_steps() {
    let lvl = |level: f32| {
        let mut rig = Rig::new(FS, clean(ChipMode::Gb, 2));
        rig.set(ParamId::WvLevel, level);
        rig.set(ParamId::Analog, 0.0);
        rig.on(57, 1.0);
        rig.left(secs(FS, 0.1));
        rms(&remove_dc(&rig.left(secs(FS, 0.3))))
    };
    let full = lvl(15.0);
    let half = lvl(7.0) / full; // 0.467 → 50 % (shift 1)
    let quarter = lvl(3.0) / full; // 0.2 → 25 % (shift 2)
    let mute = lvl(1.0) / full; // 0.067 → muted
    println!("GB wave shift levels: {half:.3} {quarter:.3} {mute:.3}");
    assert!((0.4..0.55).contains(&half));
    assert!((0.18..0.3).contains(&quarter));
    assert!(mute < 1e-3);
}

#[test]
fn nes_triangle_ignores_level_and_velocity_and_halts() {
    let render = |level: f32, vel: f32| {
        let mut rig = Rig::new(FS, clean(ChipMode::Nes, 2));
        rig.set(ParamId::WvLevel, level);
        rig.set(ParamId::VelSens, 100.0);
        rig.set(ParamId::WvRelease, 0.0);
        rig.on(45, vel);
        rig.left(secs(FS, 0.1));
        let x = rig.left(secs(FS, 0.3));
        rig.off(45);
        let tail = rig.left(secs(FS, 0.2));
        (rms(&remove_dc(&x)), tail)
    };
    let (a, tail) = render(15.0, 1.0);
    let (b, _) = render(2.0, 0.1);
    assert!((a / b - 1.0).abs() < 1e-3, "{a} vs {b}");
    // After the gate closes the sequencer halts on its step: the output becomes a constant
    // (slowly leaking DC), no oscillation.
    let late = &tail[secs(FS, 0.05)..];
    let max_step = late.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(max_step < 1e-3, "{max_step}");
}

#[test]
fn panning_and_gb_hard_pan() {
    let lr = |chip: ChipMode, pan: f32| {
        let mut rig = Rig::new(FS, clean(chip, 0));
        rig.set(ParamId::P1Pan, pan);
        rig.on(57, 1.0);
        rig.left(secs(FS, 0.1));
        let (l, r) = rig.render(secs(FS, 0.2));
        (rms(&remove_dc(&l)), rms(&remove_dc(&r)))
    };
    let (l, r) = lr(ChipMode::Free, 0.0);
    assert!((l - r).abs() < 1e-6 && (db(l) + 6.02).abs() < 0.3);
    let (l, r) = lr(ChipMode::Free, -100.0);
    assert!(r < 1e-4 && (db(l) + 3.01).abs() < 0.3, "hard left: equal power +3 dB on its side");
    let (l, r) = lr(ChipMode::Free, 50.0);
    assert!((l * l + r * r - 0.5).abs() < 0.02, "constant power");
    let (l, r) = lr(ChipMode::Gb, 30.0);
    assert!((l - r).abs() < 1e-6, "GB: |pan| ≤ 33 is centre");
    let (l, r) = lr(ChipMode::Gb, 40.0);
    assert!(l < 1e-4 && r > 0.6, "GB: hard right");
}

// ---------------------------------------------------------------------------------------
// Envelopes and frame stepping
// ---------------------------------------------------------------------------------------

#[test]
fn envelope_timing_free_mode() {
    let mut p = clean(ChipMode::Free, 0);
    p.set(ParamId::P1Attack, 100.0);
    p.set(ParamId::P1Decay, 200.0);
    p.set(ParamId::P1Sustain, 6.0);
    p.set(ParamId::P1Release, 300.0);
    let mut rig = Rig::new(FS, p);
    rig.on(81, 1.0); // 880 Hz: many cycles per window
    let x = rig.left(secs(FS, 1.0));
    rig.off(81);
    let y = rig.left(secs(FS, 0.4));
    let win = secs(FS, 0.005);
    let ex = envelope(&x, win);
    let ey = envelope(&y, win);
    let at = |e: &[f64], ms: f64| e[(ms / 5.0) as usize];
    println!(
        "env @50 {:.3} @100 {:.3} @200 {:.3} @400 {:.3} rel@150 {:.3} rel@310 {:.4}",
        at(&ex, 50.0),
        at(&ex, 100.0),
        at(&ex, 200.0),
        at(&ex, 400.0),
        at(&ey, 150.0),
        at(&ey, 310.0)
    );
    assert!((at(&ex, 50.0) - 0.5).abs() < 0.04);
    assert!((at(&ex, 97.5) - 0.975).abs() < 0.04);
    assert!((at(&ex, 200.0) - 0.7).abs() < 0.04); // half-way down the decay: 1 − 0.6 · 0.5
    assert!((at(&ex, 400.0) - 0.4).abs() < 0.03);
    assert!((at(&ey, 150.0) - 0.2).abs() < 0.03);
    assert!(at(&ey, 310.0) < 0.01);
    assert!(rig.e.is_silent() || rig.e.active_voices() == 0);
}

#[test]
fn chip_mode_envelope_is_stepped_at_frame_rate() {
    // GB: a 500 ms attack becomes a staircase of 4-bit levels changing only at 59.7275 Hz
    // frame ticks.
    let mut p = clean(ChipMode::Gb, 0);
    p.set(ParamId::P1Attack, 500.0);
    p.set(ParamId::Analog, 0.0);
    let mut rig = Rig::new(FS, p);
    rig.on(93, 1.0); // 1760 Hz
    let x = rig.left(secs(FS, 0.5));
    // Plateau level (median of |x|, immune to edge ringing) per 1 ms window.
    let levels: Vec<f64> = x
        .chunks(48)
        .map(|c| {
            let mut a: Vec<f32> = c.iter().map(|v| v.abs()).collect();
            a.sort_by(f32::total_cmp);
            a[a.len() / 2] as f64
        })
        .collect();
    let mut changes = 0;
    for w in levels.windows(2) {
        if (w[1] - w[0]).abs() > 0.01 {
            changes += 1;
        }
    }
    let frames = 0.5 * 59.7275;
    println!("level changes in 500 ms: {changes} (frames {frames:.1})");
    assert!(changes as f64 <= frames + 2.0, "changes only at frame ticks: {changes}");
    assert!(changes >= 12, "a 15-step staircase: {changes}");
    let t = telemetry(&rig.e);
    assert!((t.frame as f64 - frames).abs() <= 1.5, "frame counter {}", t.frame);
}

#[test]
fn chip_mode_pitch_modulation_is_stepped_at_frame_rate() {
    // NES sweep down 12 st over 500 ms (LIN): the pitch changes once per frame (≈ 30 steps).
    let mut p = clean(ChipMode::Nes, 0);
    p.set(ParamId::SweepOn, 1.0);
    p.set(ParamId::SweepAmt, 12.0);
    p.set(ParamId::SweepTime, 500.0);
    p.set(ParamId::SweepCurve, Curve::Lin.index() as f32);
    p.set(ParamId::Analog, 0.0);
    let mut rig = Rig::new(FS, p);
    rig.on(69, 1.0);
    let x = remove_dc(&rig.left(secs(FS, 0.6)));
    // Rising zero crossings → period sequence.
    let mut ts = Vec::new();
    for i in 1..x.len() {
        if x[i - 1] < 0.0 && x[i] >= 0.0 {
            ts.push((i - 1) as f64 + (-x[i - 1] / (x[i] - x[i - 1])) as f64);
        }
    }
    let periods: Vec<f64> = ts.windows(2).map(|w| w[1] - w[0]).collect();
    // Count pitch plateaus: runs of ≥ 3 cycles whose period stays within 0.25 samples (the
    // interpolated zero-crossing jitter is ±0.07; one NES frame step here is > 0.6 samples).
    let mut plateaus = 0;
    let mut run_start = periods[0];
    let mut run_len = 0;
    for &p in &periods {
        if (p - run_start).abs() > 0.25 {
            if run_len >= 3 {
                plateaus += 1;
            }
            run_start = p;
            run_len = 0;
        }
        run_len += 1;
    }
    let frames = 0.5 * 60.0988;
    println!("NES sweep: {plateaus} pitch plateaus over 500 ms ({frames:.1} frames)");
    assert!((plateaus as f64 - frames).abs() <= 3.0, "{plateaus}");
    // Settles exactly on the NES-quantized A4.
    let tail = &x[secs(FS, 0.52)..];
    assert!((zc_freq(tail, FS) - 440.40).abs() < 0.3);
}

#[test]
fn free_mode_modulation_is_smooth() {
    let mut p = clean(ChipMode::Free, 0);
    p.set(ParamId::SweepOn, 1.0);
    p.set(ParamId::SweepAmt, 12.0);
    p.set(ParamId::SweepTime, 500.0);
    p.set(ParamId::SweepCurve, Curve::Lin.index() as f32);
    let mut rig = Rig::new(FS, p);
    rig.on(69, 1.0);
    rig.left(secs(FS, 0.1));
    let t = telemetry(&rig.e);
    // 100 ms into a 500 ms linear 12-semitone sweep: offset 9.6 st (± one 16-sample tick).
    assert!((t.pitch_offset_st - 9.6).abs() < 0.02, "{}", t.pitch_offset_st);
}

// ---------------------------------------------------------------------------------------
// Voices and allocation
// ---------------------------------------------------------------------------------------

fn notes(e: &Engine) -> Vec<u8> {
    let mut v: Vec<u8> = e.voices().filter(|v| !v.dying).map(|v| v.note).collect();
    v.sort();
    v
}

#[test]
fn poly_allocation_and_stealing() {
    let mut p = clean(ChipMode::Gb, 0);
    p.set(ParamId::Polyphony, 4.0);
    p.set(ParamId::P1Release, 2000.0);
    let mut rig = Rig::new(FS, p);
    for n in [60, 62, 64, 65] {
        rig.on(n, 1.0);
        rig.left(64);
    }
    assert_eq!(notes(&rig.e), [60, 62, 64, 65]);
    // Fifth note steals the oldest (no voice is releasing).
    rig.on(67, 1.0);
    assert_eq!(notes(&rig.e), [62, 64, 65, 67]);
    assert_eq!(rig.e.active_voices(), 5, "the stolen voice fades out");
    rig.left(secs(FS, 0.003));
    assert_eq!(rig.e.active_voices(), 4, "fade takes 2 ms");
    // A releasing voice is preferred over the oldest held one.
    rig.off(64);
    rig.left(64);
    rig.on(69, 1.0);
    assert_eq!(notes(&rig.e), [62, 65, 67, 69]);
    // The quietest of several releasing voices goes first.
    rig.off(65);
    rig.left(secs(FS, 0.5));
    rig.off(62);
    rig.left(64);
    rig.on(71, 1.0);
    assert_eq!(notes(&rig.e), [62, 67, 69, 71], "65 released earlier → quieter → stolen");
    // Same note again retriggers its voice instead of stacking.
    rig.on(71, 1.0);
    assert_eq!(rig.e.voices().filter(|v| v.note == 71).count(), 1);
}

#[test]
fn stealing_is_click_free() {
    let mut p = clean(ChipMode::Free, 0);
    p.set(ParamId::Polyphony, 1.0);
    let mut rig = Rig::new(FS, p);
    rig.on(48, 1.0);
    let a = rig.left(secs(FS, 0.1));
    rig.on(55, 1.0);
    let b = rig.left(secs(FS, 0.1));
    // The largest sample-to-sample jump during the steal is no bigger than the waveform's
    // own edges (a band-limited pulse step of 1.0 moves < 0.6 per sample).
    let jump = |x: &[f32]| x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(jump(&b[..secs(FS, 0.005)]) <= jump(&a) * 1.05 + 1e-3);
}

#[test]
fn polyphony_limit_change_applies_at_next_note() {
    let mut p = clean(ChipMode::Gb, 0);
    p.set(ParamId::Polyphony, 8.0);
    let mut rig = Rig::new(FS, p);
    for n in 60..66 {
        rig.on(n, 1.0);
    }
    rig.left(256);
    assert_eq!(rig.e.active_voices(), 6);
    rig.set(ParamId::Polyphony, 2.0);
    rig.left(256);
    assert_eq!(rig.e.active_voices(), 6, "no glitch on the parameter change");
    rig.on(70, 1.0);
    rig.left(secs(FS, 0.003));
    assert_eq!(rig.e.active_voices(), 2);
}

#[test]
fn mono_and_legato_note_priority() {
    for mode in [VoiceMode::Mono, VoiceMode::Legato] {
        let mut p = clean(ChipMode::Free, 0);
        p.set(ParamId::VoiceMode, mode.index() as f32);
        p.set(ParamId::P1Attack, 0.0);
        p.set(ParamId::P1Decay, 1000.0);
        p.set(ParamId::P1Sustain, 0.0);
        let mut rig = Rig::new(FS, p);
        rig.on(60, 1.0);
        rig.left(secs(FS, 0.3));
        let before = telemetry(&rig.e).env_level[0];
        rig.on(64, 1.0);
        rig.left(64);
        let after = telemetry(&rig.e).env_level[0];
        assert_eq!(rig.e.active_voices(), 1);
        assert_eq!(notes(&rig.e), [64]);
        match mode {
            VoiceMode::Mono => assert!(after > before + 0.2, "MONO retriggers: {before} → {after}"),
            _ => assert!(after <= before + 1e-3, "LEGATO continues: {before} → {after}"),
        }
        // Releasing the top note returns to the most recent still-held one.
        rig.on(67, 1.0);
        rig.left(64);
        rig.off(67);
        rig.left(64);
        assert_eq!(notes(&rig.e), [64]);
        rig.off(64);
        rig.left(64);
        assert_eq!(notes(&rig.e), [60]);
        let x = rig.left(secs(FS, 0.1));
        assert!((zc_freq(&x, FS) - hz(60.0)).abs() < 0.5);
        rig.off(60);
        rig.left(64);
        assert!(rig.e.voices().all(|v| !v.gate));
    }
}

#[test]
fn unison_sub_voices_detune_and_level() {
    let render = |voices: f32| {
        let mut p = clean(ChipMode::Free, 0);
        p.set(ParamId::UniOn, 1.0);
        p.set(ParamId::UniVoices, voices);
        p.set(ParamId::UniDetune, 20.0);
        p.set(ParamId::UniWidth, 0.0);
        let mut rig = Rig::new(FS, p);
        rig.on(57, 1.0);
        rig.left(secs(FS, 0.05));
        let x = rig.left(secs(FS, 2.0));
        let subs = rig.e.voices().next().map_or(0, |v| v.sub_voices);
        (rms(&remove_dc(&x)), subs)
    };
    let (one, n1) = render(1.0);
    let (four, n4) = render(4.0);
    assert_eq!((n1, n4), (1, 4));
    // Sub-voices are scaled by 1/√n: the power of 4 uncorrelated (detuned) layers ≈ one voice.
    println!("unison 4 vs 1: {:.2} dB", db(four / one));
    assert!(db(four / one).abs() < 1.5);
}

#[test]
fn unison_count_changes_apply_at_the_next_note() {
    // SPEC §4.8 (DSP-06): structural changes, the unison count included, apply at the next
    // note — also when that note retriggers a sounding voice (MONO, POLY same key) or continues
    // it (LEGATO) — through a short crossfade that must not click.
    let subs = |rig: &Rig| rig.e.voices().map(|v| v.sub_voices).collect::<Vec<_>>();
    let jump = |x: &[f32]| x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    for (mode, second) in [(VoiceMode::Mono, 64u8), (VoiceMode::Legato, 64), (VoiceMode::Poly, 60)] {
        for chip in [ChipMode::Free, ChipMode::Nes] {
            let mut p = clean(chip, 0);
            p.set(CHANNEL_ON[2], 1.0);
            p.set(ParamId::VoiceMode, mode.index() as f32);
            p.set(ParamId::UniDetune, 20.0);
            let mut rig = Rig::new(FS, p);
            rig.on(60, 1.0);
            let a = rig.left(secs(FS, 0.1));
            assert_eq!(subs(&rig), [1]);
            // Unison on with 4 voices: nothing changes until the next note.
            rig.set(ParamId::UniOn, 1.0);
            rig.set(ParamId::UniVoices, 4.0);
            rig.left(secs(FS, 0.05));
            assert_eq!(subs(&rig), [1], "{mode:?} {chip:?}: changed before the next note");
            if mode == VoiceMode::Poly {
                rig.off(60);
                rig.left(64);
            }
            rig.on(second, 1.0);
            let b = rig.left(secs(FS, 0.1));
            assert_eq!(subs(&rig), [4], "{mode:?} {chip:?}: the next note did not adopt unison 4");
            assert!(jump(&b) < jump(&a) * 1.5 + 0.05, "{mode:?} {chip:?}: click at the change");
            assert!(rms(&remove_dc(&b[secs(FS, 0.02)..])) > 0.1, "{mode:?} {chip:?}: sounds after the change");
            // And back to one voice at the following note.
            rig.set(ParamId::UniOn, 0.0);
            rig.on(if mode == VoiceMode::Poly { second } else { 67 }, 1.0);
            let c = rig.left(secs(FS, 0.1));
            assert_eq!(subs(&rig).last(), Some(&1), "{mode:?} {chip:?}");
            assert!(jump(&c) < jump(&a) * 1.5 + 0.05, "{mode:?} {chip:?}: click at the change back");
        }
    }
}

#[test]
fn toggling_analog_in_free_mode_does_not_interrupt_notes() {
    // DSP-08: `analog` has no effect in FREE, so toggling it (menu, automation) must not run
    // the 2 + 2 ms mode-switch crossfade that cuts a hole into every sounding note.
    let mut rig = Rig::new(FS, clean(ChipMode::Free, 0));
    rig.on(60, 1.0);
    rig.left(secs(FS, 0.1));
    let window = secs(FS, 0.0005);
    for analog in [0.0, 1.0, 0.0] {
        rig.set(ParamId::Analog, analog);
        let x = rig.left(secs(FS, 0.02));
        let quietest = x.chunks(window).map(rms).fold(f64::MAX, f64::min);
        assert!(quietest > 0.3, "analog {analog}: the note drops to {quietest:.3} RMS");
    }
    // In the chip modes `analog` changes the DAC and mixer: that still crossfades.
    let mut p = clean(ChipMode::Gb, 0);
    p.set(ParamId::Analog, 1.0);
    let mut rig = Rig::new(FS, p);
    rig.on(60, 1.0);
    rig.left(secs(FS, 0.1));
    rig.set(ParamId::Analog, 0.0);
    let x = rig.left(secs(FS, 0.02));
    let quietest = x.chunks(window).map(|c| rms(&remove_dc(c))).fold(f64::MAX, f64::min);
    assert!(quietest < 0.1, "GB: the switch crossfade runs ({quietest:.3})");
}

#[test]
fn sustain_pedal_defers_note_offs() {
    let mut rig = Rig::new(FS, clean(ChipMode::Gb, 0));
    rig.on(60, 1.0);
    rig.ev(EngineEvent::Sustain { on: true });
    rig.off(60);
    rig.left(secs(FS, 0.5));
    assert!(rig.e.voices().any(|v| v.note == 60 && v.gate), "sustained");
    assert!(rig.e.is_note_held(60));
    rig.ev(EngineEvent::Sustain { on: false });
    assert!(rig.e.voices().all(|v| !v.gate), "released on pedal up");
    rig.left(secs(FS, 0.5));
    assert_eq!(rig.e.active_voices(), 0);
    // A key still physically held survives the pedal release.
    rig.ev(EngineEvent::Sustain { on: true });
    rig.on(62, 1.0);
    rig.ev(EngineEvent::Sustain { on: false });
    assert!(rig.e.voices().any(|v| v.note == 62 && v.gate));
}

#[test]
fn pedal_up_in_mono_releases_the_last_played_note() {
    // SPEC §4.7: mono note priority is "last note"; the sustain pedal only defers note-offs. On
    // pedal up, every sustained note is released at once: the voice must not "return" to an
    // older sustained note (wrong pitch, spurious retrigger) on its way to the release.
    for mode in [VoiceMode::Mono, VoiceMode::Legato] {
        for (first, second) in [(72u8, 60u8), (60, 72)] {
            let mut p = clean(ChipMode::Free, 0);
            p.set(ParamId::VoiceMode, mode.index() as f32);
            p.set(ParamId::P1Release, 400.0);
            let mut rig = Rig::new(FS, p);
            rig.ev(EngineEvent::Sustain { on: true });
            rig.on(first, 1.0);
            rig.left(2000);
            rig.off(first);
            rig.on(second, 1.0);
            rig.left(2000);
            rig.off(second);
            rig.left(2000);
            let before = rig.e.voices().next().map(|v| v.order);
            rig.ev(EngineEvent::Sustain { on: false });
            let v: Vec<_> = rig.e.voices().collect();
            assert_eq!(v.len(), 1, "{mode:?} {first}->{second}: {v:?}");
            assert_eq!(v[0].note, second, "{mode:?} {first}->{second}: the release plays the last note");
            assert!(!v[0].gate);
            assert_eq!(Some(v[0].order), before, "{mode:?} {first}->{second}: no retrigger on pedal up");
            let tail = rig.left(secs(FS, 0.1));
            let f = zc_freq(&tail, FS);
            assert!(
                (1200.0 * (f / hz(second as f64)).log2()).abs() < 5.0,
                "{mode:?} {first}->{second}: tail at {f:.1} Hz"
            );
        }
        // A key still held when the pedal comes up is returned to (last-note priority).
        let mut p = clean(ChipMode::Free, 0);
        p.set(ParamId::VoiceMode, mode.index() as f32);
        let mut rig = Rig::new(FS, p);
        rig.on(64, 1.0);
        rig.ev(EngineEvent::Sustain { on: true });
        rig.on(72, 1.0);
        rig.off(72);
        rig.on(67, 1.0);
        rig.off(67);
        rig.ev(EngineEvent::Sustain { on: false });
        let v: Vec<_> = rig.e.voices().collect();
        assert_eq!((v.len(), v[0].note, v[0].gate), (1, 64, true), "{mode:?}: back to the held key {v:?}");
    }
}

#[test]
fn all_notes_off_and_all_sound_off() {
    let mut rig = Rig::new(FS, clean(ChipMode::Free, 0));
    rig.set(ParamId::P1Release, 1000.0);
    for n in [60, 64, 67] {
        rig.on(n, 1.0);
    }
    rig.left(1024);
    rig.ev(EngineEvent::AllNotesOff);
    assert!(rig.e.voices().all(|v| !v.gate));
    assert_eq!(rig.e.active_voices(), 3, "releasing");
    rig.ev(EngineEvent::AllSoundOff);
    let x = rig.left(secs(FS, 0.003));
    assert_eq!(rig.e.active_voices(), 0);
    assert!(x.iter().all(|v| v.is_finite()));
    // Only a sub-audible DC residue of the integrator leak remains (−40 dB or less, decaying
    // at 1 Hz): no waveform.
    let tail = rig.left(secs(FS, 0.5));
    assert!(peak(&tail) < 0.01, "{}", peak(&tail));
    let steps = tail.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(steps < 1e-5, "no waveform after all sound off: {steps}");
    rig.left(secs(FS, 2.0));
    assert!(rig.e.is_silent());
}

// ---------------------------------------------------------------------------------------
// Glide
// ---------------------------------------------------------------------------------------

#[test]
fn glide_timing_lin_and_exp() {
    for curve in [Curve::Lin, Curve::Exp] {
        let mut p = clean(ChipMode::Free, 0);
        p.set(ParamId::GlideOn, 1.0);
        p.set(ParamId::GlideTime, 100.0);
        p.set(ParamId::GlideMode, GlideMode::Always.index() as f32);
        p.set(ParamId::GlideCurve, curve.index() as f32);
        p.set(ParamId::VoiceMode, VoiceMode::Mono.index() as f32);
        let mut rig = Rig::new(FS, p);
        rig.on(60, 1.0);
        rig.left(secs(FS, 0.1));
        rig.off(60);
        rig.on(72, 1.0);
        // At note-on the voice is a full octave below its target.
        assert!((telemetry(&rig.e).pitch_offset_st + 12.0).abs() < 0.1);
        rig.left(secs(FS, 0.05));
        let mid = telemetry(&rig.e).pitch_offset_st;
        rig.left(secs(FS, 0.05));
        let end = telemetry(&rig.e).pitch_offset_st;
        println!("{curve:?}: offset at 50 ms {mid:.3}, at 100 ms {end:.3}");
        match curve {
            Curve::Lin => {
                assert!((mid + 6.0).abs() < 0.1, "{mid}");
                assert!(end.abs() < 0.05, "{end}");
            }
            Curve::Exp => {
                // 99 % reached at the glide time: −12 · e^(−ln100 · t/T).
                assert!((mid + 1.2).abs() < 0.1, "{mid}");
                assert!((end + 0.12).abs() < 0.03, "{end}");
            }
        }
        let x = rig.left(secs(FS, 0.4));
        assert!((zc_freq(&x[secs(FS, 0.1)..], FS) - hz(72.0)).abs() < 0.5);
    }
}

#[test]
fn glide_legato_rule() {
    let mut p = clean(ChipMode::Free, 0);
    p.set(ParamId::GlideOn, 1.0);
    p.set(ParamId::GlideTime, 200.0);
    p.set(ParamId::GlideMode, GlideMode::Legato.index() as f32);
    let mut rig = Rig::new(FS, p);
    // Detached notes do not glide…
    rig.on(60, 1.0);
    rig.left(1024);
    rig.off(60);
    rig.on(67, 1.0);
    rig.left(64);
    assert!(telemetry(&rig.e).pitch_offset_st.abs() < 1e-3);
    // …overlapping ones do (POLY: the new voice glides from the previous note).
    rig.on(72, 1.0);
    rig.left(64);
    let off = telemetry(&rig.e).pitch_offset_st;
    assert!((-5.0..-4.0).contains(&off), "{off}");
}

// ---------------------------------------------------------------------------------------
// Arpeggiator
// ---------------------------------------------------------------------------------------

fn arp_rig(chip: ChipMode, mode: ArpMode, chord: ArpChord, rate: f32) -> Rig {
    let mut p = clean(chip, 0);
    p.set(ParamId::ArpOn, 1.0);
    p.set(ParamId::ArpMode, mode.index() as f32);
    p.set(ParamId::ArpChord, chord.index() as f32);
    p.set(ParamId::ArpRate, rate);
    p.set(ParamId::ArpSync, 0.0);
    Rig::new(FS, p)
}

/// The pitch (relative to the played note) at the centre of each arp step.
fn arp_sequence(rig: &mut Rig, note: u8, step_s: f64, steps: usize) -> Vec<i32> {
    let mut out = Vec::new();
    rig.left(secs(FS, step_s * 0.5));
    for _ in 0..steps {
        let t = telemetry(&rig.e);
        out.push((t.pitch_offset_st.round() as i32) + (t.last_note.unwrap_or(note) as i32 - note as i32));
        rig.left(secs(FS, step_s));
    }
    out
}

#[test]
fn arp_orders_on_chords() {
    let cases = [
        (ArpMode::Up, vec![0, 4, 7, 0, 4, 7, 0]),
        (ArpMode::Down, vec![7, 4, 0, 7, 4, 0, 7]),
        (ArpMode::UpDown, vec![0, 4, 7, 4, 0, 4, 7]),
        (ArpMode::Order, vec![0, 4, 7, 0, 4, 7, 0]),
    ];
    for (mode, expect) in cases {
        let mut rig = arp_rig(ChipMode::Free, mode, ArpChord::Maj, 20.0);
        rig.on(60, 1.0);
        let seq = arp_sequence(&mut rig, 60, 0.05, 7);
        assert_eq!(seq, expect, "{mode:?}");
    }
    // Octaves extend the sequence.
    let mut rig = arp_rig(ChipMode::Free, ArpMode::Up, ArpChord::Min, 20.0);
    rig.set(ParamId::ArpOct, 2.0);
    rig.on(60, 1.0);
    assert_eq!(arp_sequence(&mut rig, 60, 0.05, 7), [0, 3, 7, 12, 15, 19, 0]);
    // RANDOM: notes of the chord only, deterministic for the same input.
    let run = || {
        let mut rig = arp_rig(ChipMode::Free, ArpMode::Random, ArpChord::Dom7, 20.0);
        rig.on(60, 1.0);
        arp_sequence(&mut rig, 60, 0.05, 24)
    };
    let a = run();
    assert_eq!(a, run());
    assert!(a.iter().all(|v| [0, 4, 7, 10].contains(v)));
    assert!(a.windows(2).any(|w| w[0] != w[1]));
}

#[test]
fn arp_pitch_only_no_retrigger() {
    let mut rig = arp_rig(ChipMode::Free, ArpMode::Up, ArpChord::Maj, 20.0);
    rig.set(ParamId::P1Decay, 1000.0);
    rig.set(ParamId::P1Sustain, 0.0);
    rig.on(60, 1.0);
    let mut last = 2.0;
    for _ in 0..10 {
        rig.left(secs(FS, 0.025));
        let e = telemetry(&rig.e).env_level[0];
        assert!(e < last, "envelope keeps decaying through arp steps");
        last = e;
    }
}

#[test]
fn arp_held_collapses_to_one_voice() {
    let mut rig = arp_rig(ChipMode::Free, ArpMode::Up, ArpChord::Held, 20.0);
    rig.set(ParamId::VoiceMode, VoiceMode::Poly.index() as f32);
    rig.on(64, 1.0);
    rig.on(60, 1.0);
    rig.on(67, 1.0);
    rig.left(64);
    assert_eq!(rig.e.active_voices(), 1);
    assert!(rig.e.voices().next().unwrap().held_arp);
    let seq = arp_sequence(&mut rig, 64, 0.05, 6);
    // UP over the held notes 60 64 67 (relative to 64).
    assert_eq!(seq, [-4, 0, 3, -4, 0, 3]);
    // ORDER plays them as played: 64 60 67.
    let mut rig = arp_rig(ChipMode::Free, ArpMode::Order, ArpChord::Held, 20.0);
    rig.on(64, 1.0);
    rig.on(60, 1.0);
    rig.on(67, 1.0);
    assert_eq!(arp_sequence(&mut rig, 64, 0.05, 6), [0, -4, 3, 0, -4, 3]);
    // Releasing all notes releases the arp voice.
    for n in [64, 60, 67] {
        rig.off(n);
    }
    assert!(rig.e.voices().all(|v| !v.gate));
}

#[test]
fn arp_rate_quantized_to_frames_in_chip_modes() {
    // NES: 20 Hz → 3 frames per step (49.9 ms); 25 Hz → 2 frames (33.3 ms).
    for (rate, frames) in [(20.0, 3.0), (25.0, 2.0), (55.0, 1.0)] {
        let mut rig = arp_rig(ChipMode::Nes, ArpMode::Up, ArpChord::Oct, rate);
        rig.on(60, 1.0);
        let step = frames / 60.0988;
        let mut changes = 0;
        let mut last = telemetry(&rig.e).arp_step;
        let total = 2.0;
        let n = secs(FS, total) / 64;
        for _ in 0..n {
            rig.left(64);
            let s = telemetry(&rig.e).arp_step;
            if s != last {
                changes += 1;
                last = s;
            }
        }
        let expect = total / step;
        assert!((changes as f64 - expect).abs() <= 2.0, "rate {rate}: {changes} steps vs {expect:.1}");
    }
}

#[test]
fn arp_tempo_sync() {
    let mut rig = arp_rig(ChipMode::Free, ArpMode::Up, ArpChord::Pow, 20.0);
    rig.set(ParamId::ArpSync, 1.0);
    rig.set(ParamId::ArpDiv, 3.0); // 1/16
    rig.transport(150.0); // 1/16 = 100 ms
    rig.on(60, 1.0);
    assert_eq!(arp_sequence(&mut rig, 60, 0.1, 6), [0, 7, 0, 7, 0, 7]);
}

// ---------------------------------------------------------------------------------------
// Noise
// ---------------------------------------------------------------------------------------

#[test]
fn gb_short_noise_is_periodic_with_127_clocks() {
    // GB index 12 = 4096 Hz clock; at 49 152 Hz that is exactly 12 samples per clock, so the
    // 7-bit LFSR repeats every 127 · 12 = 1524 samples.
    let fs = 49_152.0;
    let mut p = clean(ChipMode::Gb, 3);
    p.set(ParamId::NsPeriod, 12.0);
    p.set(ParamId::NsMode, 1.0);
    p.set(ParamId::Analog, 0.0);
    let mut rig = Rig::new(fs, p);
    rig.on(60, 1.0);
    rig.left(4096);
    let x = remove_dc(&rig.left(1524 * 8));
    let corr = |lag: usize| {
        let n = x.len() - lag;
        let a: f64 = (0..n).map(|i| x[i] as f64 * x[i + lag] as f64).sum();
        let b: f64 = (0..n).map(|i| (x[i] as f64).powi(2)).sum();
        a / b
    };
    println!("short-noise autocorrelation: lag 1524 {:.4}, lag 762 {:.4}", corr(1524), corr(762));
    assert!(corr(1524) > 0.98);
    assert!(corr(762).abs() < 0.3);
}

#[test]
fn noise_period_index_and_key_tracking() {
    // Zero-crossing rate follows the clock (NES index 12: 2349 Hz, index 13: 1762 Hz).
    let zc_rate = |period: f32, track: bool, note: u8| {
        let mut p = clean(ChipMode::Nes, 3);
        p.set(ParamId::NsPeriod, period);
        p.set(ParamId::NsTrack, if track { 1.0 } else { 0.0 });
        p.set(ParamId::Analog, 0.0);
        let mut rig = Rig::new(FS, p);
        rig.on(note, 1.0);
        rig.left(4800);
        let x = remove_dc(&rig.left(secs(FS, 2.0)));
        x.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count() as f64 / 2.0
    };
    let a = zc_rate(12.0, false, 60);
    let b = zc_rate(13.0, false, 60);
    println!("NES noise zero crossings/s: idx12 {a:.0}, idx13 {b:.0}");
    assert!((a / b - 2349.0 / 1762.0).abs() < 0.1, "{}", a / b);
    // Tracking: one index per semitone (note 61 with period 13 = index 12).
    let c = zc_rate(13.0, true, 61);
    assert!((c / a - 1.0).abs() < 0.08, "{c} vs {a}");
    // Without tracking the note does not matter.
    let d = zc_rate(13.0, false, 72);
    assert!((d / b - 1.0).abs() < 0.08);
}

#[test]
fn brightest_noise_uses_integrate_and_dump_and_stays_bounded() {
    let mut p = clean(ChipMode::Nes, 3);
    p.set(ParamId::NsPeriod, 0.0);
    let mut rig = Rig::new(FS, p);
    rig.on(60, 1.0);
    let x = rig.left(secs(FS, 1.0));
    assert!(x.iter().all(|v| v.is_finite() && v.abs() < 2.0));
    let r = rms(&remove_dc(&x[4800..]));
    assert!(r > 0.1, "{r}");
}

// ---------------------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------------------

#[test]
fn telemetry_fields() {
    let mut p = clean(ChipMode::Gb, 2);
    p.set(ParamId::NsOn, 1.0);
    let mut rig = Rig::new(FS, p);
    let t = telemetry(&rig.e);
    assert_eq!(t.active_voices, 0);
    assert_eq!(t.last_note, None);
    rig.on(60, 1.0);
    rig.on(64, 0.5);
    rig.left(secs(FS, 0.1));
    let t = telemetry(&rig.e);
    assert_eq!(t.active_voices, 2);
    assert!(t.is_held(60) && t.is_held(64) && !t.is_held(61));
    assert_eq!(t.last_note, Some(64));
    assert!(t.wave_step.is_some_and(|s| s < 32));
    assert!(t.env_level[2] > 0.5 && t.env_level[3] > 0.5);
    assert!(t.lfsr < 0x7fff, "noise register is running");
    assert!(t.frame >= 5);
    let s1 = telemetry(&rig.e).wave_step;
    rig.left(7);
    assert_ne!(telemetry(&rig.e).wave_step, s1, "wave step moves");
}

// ---------------------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------------------

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn one_in(&mut self, n: u64) -> bool {
        self.next().is_multiple_of(n)
    }
    fn f(&mut self) -> f32 {
        (self.next() % 1_000_000) as f32 / 1_000_000.0
    }
}

#[test]
fn fuzz_parameters_and_events_never_produce_nan() {
    let mut rng = Lcg(0xC0FFEE);
    let specials = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30, 0.0, -0.0];
    for round in 0..60 {
        let fs = [22_050.0, 44_100.0, 48_000.0, 96_000.0, 192_000.0, f32::NAN, 1.0][round % 7];
        let mut e = Engine::new(64);
        e.prepare(fs, 64);
        let mut p = ParamValues::defaults();
        let mut l = vec![0.0f32; 300];
        let mut r = vec![0.0f32; 300];
        for _ in 0..80 {
            // Mutate a few parameters, sometimes with garbage.
            for _ in 0..4 {
                let i = rng.next() as usize % p.0.len();
                let spec = &pixl_core::PARAMS[i];
                let (lo, hi) = spec.range();
                p.0[i] = if rng.one_in(9) {
                    specials[rng.next() as usize % specials.len()]
                } else {
                    lo + (hi - lo) * rng.f()
                };
            }
            e.set_params(&p);
            match rng.next() % 10 {
                0..=3 => e.event(EngineEvent::NoteOn {
                    note: (rng.next() % 140) as u8,
                    velocity: if rng.one_in(13) { f32::NAN } else { rng.f() },
                }),
                4..=5 => e.event(EngineEvent::NoteOff { note: (rng.next() % 140) as u8 }),
                6 => e.event(EngineEvent::PitchBend { value: rng.f() * 4.0 - 2.0 }),
                7 => e.event(EngineEvent::ModWheel { value: if rng.one_in(5) { f32::INFINITY } else { rng.f() } }),
                8 => e.event(EngineEvent::Sustain { on: rng.one_in(2) }),
                _ => e.event(if rng.one_in(4) {
                    EngineEvent::AllSoundOff
                } else {
                    EngineEvent::Aftertouch { value: rng.f() }
                }),
            }
            if rng.one_in(17) {
                let mut w = [0u8; 32];
                w.iter_mut().for_each(|v| *v = (rng.next() % 40) as u8);
                e.set_wave(&w);
            }
            let n = 1 + rng.next() as usize % 300;
            e.render(&mut l[..n], &mut r[..n]);
            assert!(
                l[..n].iter().chain(r[..n].iter()).all(|v| v.is_finite() && v.abs() < 64.0),
                "round {round}: non-finite or runaway output"
            );
            let mut t = TelemetrySnapshot::default();
            e.fill_telemetry(&mut t);
            assert!(t.pitch_offset_st.is_finite() && t.env_level.iter().all(|v| v.is_finite()));
        }
    }
}

fn scripted(e: &mut Engine, p: &ParamValues, blocks: &[usize]) -> Vec<f32> {
    let mut out = Vec::new();
    let mut buf_l = vec![0.0f32; 512];
    let mut buf_r = vec![0.0f32; 512];
    let script = [
        (0usize, EngineEvent::NoteOn { note: 60, velocity: 0.9 }),
        (5, EngineEvent::NoteOn { note: 64, velocity: 0.7 }),
        (20, EngineEvent::NoteOff { note: 60 }),
        (30, EngineEvent::NoteOn { note: 67, velocity: 1.0 }),
        (60, EngineEvent::AllNotesOff),
    ];
    let mut bi = 0;
    e.set_params(p);
    for step in 0..120 {
        for (at, ev) in script.iter() {
            if *at == step {
                e.event(*ev);
            }
        }
        // Each script step lasts 256 samples, rendered with the given block pattern.
        let mut left = 256;
        while left > 0 {
            let n = blocks[bi % blocks.len()].min(left);
            bi += 1;
            e.set_params(p);
            e.render(&mut buf_l[..n], &mut buf_r[..n]);
            out.extend_from_slice(&buf_l[..n]);
            left -= n;
        }
    }
    out
}

#[test]
fn deterministic_and_reset_restores_fresh_state() {
    let mut p = ParamValues::defaults();
    p.set(ParamId::NsOn, 1.0);
    p.set(ParamId::P1On, 1.0);
    p.set(ParamId::ArpOn, 1.0);
    p.set(ParamId::ArpChord, ArpChord::Min.index() as f32);
    p.set(ParamId::ArpMode, ArpMode::Random.index() as f32);
    let mut a = Engine::new(64);
    let mut b = Engine::new(64);
    let x = scripted(&mut a, &p, &[64]);
    let y = scripted(&mut b, &p, &[64]);
    assert_eq!(x, y, "bit-identical");
    a.reset();
    let z = scripted(&mut a, &p, &[64]);
    assert_eq!(x, z, "reset == fresh");
    assert!(x.iter().any(|v| v.abs() > 0.05));
}

#[test]
fn block_size_independence() {
    // Different block patterns (including blocks longer than max_block, which are split)
    // give the same audio to within float rounding of the parameter smoothers.
    let p = {
        let mut p = ParamValues::defaults();
        p.set(ParamId::P1On, 1.0);
        p.set(ParamId::Chip, ChipMode::Free.index() as f32);
        p
    };
    let mut a = Engine::new(64);
    let mut b = Engine::new(64);
    let x = scripted(&mut a, &p, &[64]);
    let y = scripted(&mut b, &p, &[1, 7, 64, 33, 200, 5]);
    let diff = x.iter().zip(y.iter()).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(diff < 1e-4, "{diff}");
}

#[test]
fn sample_rates() {
    for fs in [22_050.0f32, 44_100.0, 88_200.0, 96_000.0, 192_000.0] {
        let mut rig = Rig::new(fs, clean(ChipMode::Free, 0));
        rig.on(69, 1.0);
        rig.left(secs(fs, 0.05));
        let x = rig.left(secs(fs, 0.5));
        let f = zc_freq(&x, fs);
        assert!((f - 440.0).abs() < 0.2, "{fs}: {f}");
        assert!((db(rms(&remove_dc(&x))) + 6.02).abs() < 0.3);
    }
}

#[test]
fn chip_switch_while_holding_is_clean() {
    let mut p = ParamValues::defaults();
    p.set(ParamId::P1On, 1.0);
    let mut rig = Rig::new(FS, p);
    for n in [48, 55, 60, 64] {
        rig.on(n, 0.9);
    }
    let a = rig.left(secs(FS, 0.3));
    let jump = |x: &[f32]| x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    for chip in [ChipMode::Nes, ChipMode::Free, ChipMode::Gb] {
        rig.set(ParamId::Chip, chip.index() as f32);
        let b = rig.left(secs(FS, 0.3));
        assert!(b.iter().all(|v| v.is_finite()));
        assert!(jump(&b[..secs(FS, 0.01)]) < jump(&a) * 1.5 + 0.05, "{chip:?}");
        assert_eq!(rig.e.active_voices(), 4, "voices keep sounding in the new mode");
        assert!(rms(&remove_dc(&b[secs(FS, 0.1)..])) > 0.05);
    }
    rig.set(ParamId::Analog, 0.0);
    let b = rig.left(secs(FS, 0.1));
    assert!(b.iter().all(|v| v.is_finite()));
}

#[test]
fn mode_switches_move_the_dc_level_smoothly() {
    // FX-03 (SPEC §4.8): the unipolar DAC (NES/GB with `analog` on) carries a DC level, the
    // bipolar one does not. Across a switch the 2 + 2 ms crossfade must not step that level (the
    // chip filter would turn the step into a thump): it is held through the crossfade and then
    // settles with a 100 ms time constant.
    let mean = |x: &[f32]| x.iter().map(|v| *v as f64).sum::<f64>() / x.len() as f64;
    for (from, to, analog_to) in
        [(ChipMode::Gb, ChipMode::Free, 1.0), (ChipMode::Free, ChipMode::Nes, 1.0), (ChipMode::Gb, ChipMode::Gb, 0.0)]
    {
        let mut p = clean(from, 0);
        p.set(ParamId::P1Level, 10.0);
        p.set(ParamId::Analog, 1.0);
        let mut rig = Rig::new(FS, p);
        rig.on(72, 1.0);
        let a = rig.left(secs(FS, 0.3));
        rig.set(ParamId::Chip, to.index() as f32);
        rig.set(ParamId::Analog, analog_to);
        let b = rig.left(secs(FS, 1.0));
        // DC trajectory in windows of four pulse periods (523 Hz → 4 × 92 samples).
        let w = 367;
        let dc: Vec<f64> =
            a[a.len() - 4 * w..].chunks(w).chain(b.chunks(w)).filter(|c| c.len() == w).map(mean).collect();
        let (start, end) = (dc[0], *dc.last().unwrap());
        let step = dc.windows(2).map(|p| (p[1] - p[0]).abs()).fold(0.0, f64::max);
        println!(
            "{from:?} → {to:?} (analog {analog_to}): DC {start:+.3} → {end:+.3}, largest step per window {step:.4}"
        );
        assert!((start - end).abs() > 0.1, "the switch changes the DC level");
        // A plain 4 ms crossfade moves the whole difference within one window; the bridge moves
        // ≈ 7.6 ms / 100 ms of it per window (plus the crossfade's amplitude modulation).
        assert!(step < 0.15 * (start - end).abs(), "{from:?} → {to:?}: DC step {step} of {}", (start - end).abs());
        // Fully settled to the new mode after the bridge.
        let late = mean(&b[secs(FS, 0.8)..]);
        assert!((late - end).abs() < 0.01, "{late} vs {end}");
    }
}

#[test]
fn mod_wheel_adds_vibrato_without_vib_on() {
    let mut rig = Rig::new(FS, clean(ChipMode::Free, 0));
    rig.ev(EngineEvent::ModWheel { value: 1.0 });
    rig.on(69, 1.0);
    let mut lo = 0.0f32;
    let mut hi = 0.0f32;
    for _ in 0..200 {
        rig.left(64);
        let o = telemetry(&rig.e).pitch_offset_st;
        lo = lo.min(o);
        hi = hi.max(o);
    }
    assert!((hi - 0.5).abs() < 0.03 && (lo + 0.5).abs() < 0.03, "{lo} {hi}");
}

#[test]
fn vibrato_delay_and_fade_in() {
    let mut p = clean(ChipMode::Free, 0);
    p.set(ParamId::VibOn, 1.0);
    p.set(ParamId::VibDepth, 40.0);
    p.set(ParamId::VibDelay, 200.0);
    p.set(ParamId::VibFade, 200.0);
    p.set(ParamId::VibRate, 6.0);
    let mut rig = Rig::new(FS, p);
    rig.on(69, 1.0);
    let max_abs = |rig: &mut Rig, ms: usize| {
        let mut m = 0.0f32;
        for _ in 0..ms * 48 / 64 {
            rig.left(64);
            m = m.max(telemetry(&rig.e).pitch_offset_st.abs());
        }
        m
    };
    assert!(max_abs(&mut rig, 190) < 1e-3, "silent during the delay");
    let ramp = max_abs(&mut rig, 210);
    assert!(ramp > 0.1 && ramp <= 0.41);
    let full = max_abs(&mut rig, 400);
    assert!((full - 0.4).abs() < 0.02, "{full}");
}

#[test]
fn sweep_up_and_down() {
    for (dir, sign) in [(0.0, 1.0f32), (1.0, -1.0)] {
        let mut p = clean(ChipMode::Free, 0);
        p.set(ParamId::SweepOn, 1.0);
        p.set(ParamId::SweepAmt, 7.0);
        p.set(ParamId::SweepDir, dir);
        p.set(ParamId::SweepTime, 300.0);
        let mut rig = Rig::new(FS, p);
        rig.on(60, 1.0);
        let start = telemetry(&rig.e).pitch_offset_st;
        assert!((start - sign * 7.0).abs() < 0.05, "{start}");
        rig.left(secs(FS, 0.15));
        let mid = telemetry(&rig.e).pitch_offset_st; // EXP: 7 · 0.25
        assert!((mid - sign * 1.75).abs() < 0.05, "{mid}");
        rig.left(secs(FS, 0.2));
        assert_eq!(telemetry(&rig.e).pitch_offset_st, 0.0);
    }
}

#[test]
fn wave_ram_changes_apply_to_sounding_notes() {
    let mut rig = Rig::new(FS, clean(ChipMode::Gb, 2));
    rig.e.set_wave(&[7; 32]); // flat → no waveform
    rig.on(57, 1.0);
    rig.left(4800);
    let flat = rig.left(4800);
    let steps = flat.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(steps < 1e-4, "{steps}");
    rig.e.set_wave(&pixl_core::wave::square());
    rig.left(480);
    assert!(rms(&remove_dc(&rig.left(4800))) > 0.3);
}

#[test]
fn silence_detection() {
    let mut rig = Rig::new(FS, clean(ChipMode::Gb, 0));
    assert!(rig.e.is_silent());
    rig.on(60, 1.0);
    rig.left(64);
    assert!(!rig.e.is_silent());
    rig.off(60);
    rig.left(secs(FS, 4.0));
    assert!(rig.e.is_silent());
    let x = rig.left(256);
    assert!(x.iter().all(|v| *v == 0.0), "exact digital silence");
}

#[test]
fn long_modulated_chords_leave_no_dc_and_end_in_silence() {
    // SPEC §4.2 (bounded numeric drift): 30 s of an 8-note chord on the init patch (GB, unison
    // 3) while macros move unison width and pans (what the factory SPACE macro does). After the
    // release the integrator must return to exactly zero and the engine report silence — no
    // DC residue that grows with playing time.
    for chip in [ChipMode::Gb, ChipMode::Nes, ChipMode::Free] {
        let mut p = ParamValues::defaults();
        p.set(ParamId::Chip, chip.index() as f32);
        p.set(ParamId::P1On, 1.0);
        p.set(ParamId::UniVoices, 4.0);
        let mut rig = Rig::new(FS, p);
        let chord = [48u8, 52, 55, 60, 64, 67, 71, 72];
        for n in chord {
            rig.on(n, 0.9);
        }
        let mut l = [0.0f32; SUB];
        let mut r = [0.0f32; SUB];
        let blocks = secs(FS, 30.0) / SUB;
        for b in 0..blocks {
            let ph = b as f32 * 0.013;
            rig.p.set(ParamId::UniWidth, 50.0 + 50.0 * ph.sin());
            rig.p.set(ParamId::WvPan, 80.0 * (ph * 0.7).cos());
            rig.p.set(ParamId::P1Pan, -60.0 * (ph * 1.3).sin());
            rig.p.set(ParamId::WvLevel, 9.0 + 3.0 * (ph * 0.37).sin());
            rig.render_into(&mut l, &mut r);
        }
        for n in chord {
            rig.off(n);
        }
        rig.left(secs(FS, 2.0));
        assert_eq!(rig.e.active_voices(), 0, "{chip:?}");
        let (x, y) = rig.render(1024);
        let dc = x.iter().chain(y.iter()).fold(0.0f32, |m, v| m.max(v.abs()));
        println!("{chip:?}: residue after release {dc:.3e}");
        assert!(dc < 1e-6, "{chip:?}: DC residue {dc:e} after 30 s");
        assert!(rig.e.is_silent(), "{chip:?}: silent after release");
    }
}

#[test]
fn nes_triangle_unison_layers_start_at_random_phases() {
    // SPEC §4.3/§4.7 (DSP-05): unison sub-voices are scaled by 1/√n and must not start
    // coherently. The volume-less NES triangle cannot start on a random step (that would jump
    // its DAC), so its layers wait a random fraction of a period on the rest step. The init
    // patch (wave channel, unison 3) switched to NES must therefore start about as loud as the
    // same patch with one voice — not +4.8 dB (three layers in phase). Measured at the onset
    // (later the layers' detune decorrelates them anyway) and averaged over 24 notes, i.e. 24
    // different seeds.
    let onset_power = |voices: f32| {
        let mut p = ParamValues::defaults();
        p.set(ParamId::Chip, ChipMode::Nes.index() as f32);
        p.set(ParamId::UniVoices, voices);
        let mut rig = Rig::new(FS, p);
        let mut power = 0.0;
        for k in 0..24u8 {
            let note = 45 + (k * 7) % 31;
            rig.on(note, 0.9);
            let x = rig.left(secs(FS, 0.06));
            rig.off(note);
            rig.left(secs(FS, 0.4));
            power += rms(&remove_dc(&x[secs(FS, 0.005)..])).powi(2);
        }
        power
    };
    let (p1, p3) = (onset_power(1.0), onset_power(3.0));
    let diff = 10.0 * (p3 / p1).log10();
    println!("unison 3 vs 1 (NES triangle, init patch): {diff:+.2} dB");
    assert!(diff.abs() < 1.5, "unison 3 is {diff:+.2} dB against unison 1");
    // Still click-free: every layer starts on the rest step, so the output starts from zero
    // and only steps along the triangle (a layer started on a random step would jump its DAC
    // by up to 15 levels at once).
    let mut p = clean(ChipMode::Nes, 2);
    p.set(ParamId::UniOn, 1.0);
    p.set(ParamId::UniVoices, 4.0);
    let mut rig = Rig::new(FS, p);
    rig.on(57, 1.0);
    let x = rig.left(64);
    let start = peak(&x[..4]);
    assert!(start < 0.02, "DAC jump of {start} at the note-on");
}

// ---------------------------------------------------------------------------------------
// Real-time safety of long sessions
// ---------------------------------------------------------------------------------------

#[test]
fn reused_voice_slot_after_long_idle_renders_in_constant_time() {
    // SPEC §0 / §4.2: the integrate-and-dump noise (NES period 0 = 9.3 clocks per sample)
    // must not do work proportional to how long a voice slot sat idle. A slot left idle for
    // 60 s is reused by the next chord; its first blocks must stay far inside the real-time
    // budget of a 64-sample block (1.33 ms at 48 kHz; the defect took 14-17 ms here). Best of
    // three runs, so scheduling noise on a loaded machine cannot fail the test.
    let mut best = f64::MAX;
    for _ in 0..3 {
        let mut p = clean(ChipMode::Nes, 3);
        p.set(ParamId::NsPeriod, 0.0);
        p.set(ParamId::NsRelease, 0.0);
        let mut rig = Rig::new(FS, p);
        let chord = [48u8, 52, 55, 60];
        for n in chord {
            rig.on(n, 1.0);
        }
        rig.left(secs(FS, 0.1));
        for n in chord {
            rig.off(n);
        }
        rig.left(secs(FS, 60.0));
        assert_eq!(rig.e.active_voices(), 0);
        for n in chord {
            rig.on(n, 1.0);
        }
        let mut l = [0.0f32; SUB];
        let mut r = [0.0f32; SUB];
        let mut worst = 0.0f64;
        for _ in 0..8 {
            let t0 = std::time::Instant::now();
            rig.render_into(&mut l, &mut r);
            worst = worst.max(t0.elapsed().as_secs_f64());
        }
        best = best.min(worst);
        assert!(rms(&remove_dc(&rig.left(4800))) > 0.05, "the reused voices sound");
    }
    println!("first blocks after 60 s idle: worst {:.4} ms", best * 1e3);
    assert!(best < 1.0e-3, "{:.3} ms for one 64-sample block", best * 1e3);
}

// ---------------------------------------------------------------------------------------
// Note-on values in chip modes (SPEC §4.4 / §4.6)
// ---------------------------------------------------------------------------------------

/// Frame period (samples) of a chip mode at [`FS`].
fn frame_period(chip: ChipMode) -> f64 {
    let hz = if chip == ChipMode::Gb { pixl_dsp::chip::GB_FRAME_HZ } else { pixl_dsp::chip::NES_FRAME_NTSC };
    FS as f64 / hz
}

#[test]
fn chip_mode_note_on_plays_its_initial_values_for_the_first_frame() {
    // SPEC §4.4: on note-on the initial values apply immediately. A one-shot envelope shorter
    // than a frame (A 0 / D 5 ms / S 0: clicks, hats, blips) must sound at full level for its
    // first frame wherever the note falls relative to the global frame clock — at least half a
    // frame, at most one and a half.
    for chip in [ChipMode::Nes, ChipMode::Gb] {
        let period = frame_period(chip);
        for offset in [0usize, 1, 37, 200, 399, 401, 600, 790] {
            let mut p = clean(chip, 0);
            p.set(ParamId::P1Decay, 5.0);
            p.set(ParamId::P1Sustain, 0.0);
            p.set(ParamId::P1Release, 0.0);
            let mut rig = Rig::new(FS, p);
            rig.left(offset);
            rig.on(69, 1.0);
            // Raw output (unipolar DAC): high half-periods reach the full level, silence is 0.
            let x = rig.left(secs(FS, 0.06));
            let pk = peak(&x);
            assert!(pk > 0.6, "{chip:?} offset {offset}: first frame silent (peak {pk})");
            let last = x.iter().rposition(|v| v.abs() > 0.1).unwrap_or(0) as f64;
            assert!(
                last >= 0.45 * period && last <= 1.55 * period,
                "{chip:?} offset {offset}: sounded for {last} samples (frame {period:.0})"
            );
        }
    }
}

#[test]
fn chip_mode_sweep_and_glide_start_at_their_initial_offset() {
    // SPEC §4.6: the sweep offset starts at +amt; a glide starts at the previous note. In chip
    // modes the first frame plays exactly that, even for sweeps and glides shorter than a frame.
    for chip in [ChipMode::Nes, ChipMode::Gb] {
        let period = frame_period(chip);
        for offset in [0usize, 123, 399, 401, 777] {
            for time in [5.0f32, 8.0, 20.0, 180.0] {
                let mut p = clean(chip, 0);
                p.set(ParamId::SweepOn, 1.0);
                p.set(ParamId::SweepAmt, 24.0);
                p.set(ParamId::SweepTime, time);
                p.set(ParamId::SweepCurve, Curve::Exp.index() as f32);
                let mut rig = Rig::new(FS, p);
                rig.left(offset);
                rig.on(57, 1.0);
                let x = rig.left((0.45 * period) as usize);
                // Skip the onset (a GB duty cycle starts high, so its first crossing is early).
                let f = zc_freq(&x[64..], FS);
                let expect = hz(57.0 + 24.0);
                assert!(
                    (1200.0 * (f / expect).log2()).abs() < 40.0,
                    "{chip:?} offset {offset} sweep {time} ms: first frame at {f:.1} Hz, expected ≈{expect:.1}"
                );
            }
        }
        // Glide: MONO legato line A4 → A5 with a 10 ms glide starts at A4.
        let mut p = clean(chip, 0);
        p.set(ParamId::VoiceMode, VoiceMode::Legato.index() as f32);
        p.set(ParamId::GlideOn, 1.0);
        p.set(ParamId::GlideTime, 10.0);
        p.set(ParamId::GlideMode, GlideMode::Always.index() as f32);
        p.set(ParamId::GlideCurve, Curve::Lin.index() as f32);
        let mut rig = Rig::new(FS, p);
        rig.on(69, 1.0);
        rig.left(secs(FS, 0.2) + 211);
        rig.on(81, 1.0);
        rig.left(1);
        assert!((telemetry(&rig.e).pitch_offset_st + 12.0).abs() < 1e-3, "{chip:?} glide starts at the previous note");
        let x = rig.left((0.45 * period) as usize);
        let f = zc_freq(&x[32..], FS);
        assert!((1200.0 * (f / hz(69.0)).log2()).abs() < 40.0, "{chip:?} glide first frame at {f:.1} Hz");
    }
}

#[test]
fn busy_chip_patch_is_independent_of_host_segmentation() {
    fn render(block: usize) -> Vec<f32> {
        let mut p = ParamValues::defaults();
        p.set(ParamId::P1On, 1.0);
        p.set(ParamId::NsOn, 1.0);
        p.set(ParamId::NsLevel, 4.0);
        let mut effective = p;
        pixl_core::macros::apply_macros(
            &p,
            [p.get(ParamId::Macro1), p.get(ParamId::Macro2), p.get(ParamId::Macro3), p.get(ParamId::Macro4)],
            &pixl_core::MacroAssignments::factory_default(),
            &mut effective,
        );
        let p = effective;
        let mut e = Engine::new(64);
        e.prepare(FS, 64);
        e.set_params(&p);
        let events = [
            (0, EngineEvent::NoteOn { note: 60, velocity: 0.9 }),
            (480, EngineEvent::NoteOn { note: 64, velocity: 0.7 }),
            (960, EngineEvent::NoteOn { note: 67, velocity: 0.5 }),
            (12000, EngineEvent::PitchBend { value: 12000.0 / 16383.0 * 2.0 - 1.0 }),
            (14400, EngineEvent::NoteOff { note: 60 }),
            (16800, EngineEvent::ModWheel { value: 100.0 / 127.0 }),
            (19200, EngineEvent::Sustain { on: true }),
            (21600, EngineEvent::NoteOff { note: 64 }),
            (22080, EngineEvent::NoteOff { note: 67 }),
            (24000, EngineEvent::NoteOn { note: 72, velocity: 1.0 }),
            (28800, EngineEvent::NoteOff { note: 72 }),
            (33600, EngineEvent::Sustain { on: false }),
            (34560, EngineEvent::PitchBend { value: 8192.0 / 16383.0 * 2.0 - 1.0 }),
            (38400, EngineEvent::NoteOn { note: 55, velocity: 0.6 }),
            (38400, EngineEvent::NoteOn { note: 59, velocity: 0.6 }),
            (52800, EngineEvent::NoteOff { note: 55 }),
            (52800, EngineEvent::NoteOff { note: 59 }),
        ];
        let mut next = 0;
        let mut out = Vec::new();
        for start in (0..76800).step_by(block) {
            let end = (start + block).min(76800);
            let mut pos = start;
            while pos < end {
                while next < events.len() && events[next].0 == pos {
                    e.event(events[next].1);
                    next += 1;
                }
                let stop = events.get(next).map_or(end, |e| e.0.min(end));
                let n = (stop - pos).min(64);
                let mut l = vec![0.0; n];
                let mut r = vec![0.0; n];
                e.set_params(&p);
                e.render(&mut l, &mut r);
                out.extend(l);
                pos += n;
            }
        }
        out
    }
    let a = render(64);
    for block in [1, 7, 63, 100, 333, 512] {
        let b = render(block);
        let (i, diff) = a
            .iter()
            .zip(&b)
            .enumerate()
            .map(|(i, (a, b))| (i, (a - b).abs()))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        assert!(diff < 1e-5, "block {block}, sample {i}: {diff}");
    }
}
