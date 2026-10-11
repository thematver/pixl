//! SPEC §4.2 acceptance: band-limited rendering. A C8 pulse at 44.1 kHz must have every
//! non-harmonic (aliased) component ≤ −90 dB relative to the fundamental (Blackman-Harris
//! FFT, harmonics masked). Same for the wave channel and the NES pulse.

mod common;
use common::*;
use pixl_core::{ChipMode, ParamId};
use pixl_dsp::analysis::{alias_report, alias_report_band};
use pixl_dsp::chip::{nes_pulse_freq, nes_pulse_timer, NES_CPU_NTSC};

const FS: f32 = 44_100.0;
const N: usize = 1 << 16;

fn measure(
    chip: ChipMode,
    channel: usize,
    note: u8,
    f0: f64,
    tweak: impl Fn(&mut Rig),
) -> pixl_dsp::analysis::AliasReport {
    let mut rig = Rig::new(FS, clean(chip, channel));
    tweak(&mut rig);
    rig.on(note, 1.0);
    rig.left(secs(FS, 0.3)); // settle
    let x = rig.left(N);
    let r = alias_report(&x, FS as f64, f0);
    println!("{chip:?} ch{channel} note {note} f0 {f0:.2} Hz: worst non-harmonic {:.1} dB at {:.0} Hz (fundamental {:.1} dBFS)", r.worst_db, r.worst_hz, r.fundamental_db);
    r
}

#[test]
fn free_pulse_c8_alias_below_minus_90_db() {
    let r = measure(ChipMode::Free, 0, 108, hz(108.0), |_| {});
    assert!(r.worst_db <= -90.0, "{r:?}");
}

#[test]
fn free_wave_c8_alias_below_minus_90_db() {
    let r = measure(ChipMode::Free, 2, 108, hz(108.0), |_| {});
    assert!(r.worst_db <= -90.0, "{r:?}");
}

#[test]
fn nes_pulse_c8_alias_below_minus_90_db() {
    let f0 = nes_pulse_freq(nes_pulse_timer(hz(108.0), NES_CPU_NTSC), NES_CPU_NTSC);
    let r = measure(ChipMode::Nes, 0, 108, f0, |rig| rig.set(ParamId::Analog, 0.0));
    assert!(r.worst_db <= -90.0, "{r:?}");
    // With the unipolar DAC (analog on) the result is the same up to DC.
    let r = measure(ChipMode::Nes, 0, 108, f0, |_| {});
    assert!(r.worst_db <= -90.0, "{r:?}");
}

#[test]
fn pulse_and_wave_alias_across_the_keyboard() {
    // Beyond the acceptance criterion: across the keyboard, aliases folding into the audible
    // band (< 20 kHz at 44.1 kHz) come only from harmonics above 0.547·fs, where the kernel's
    // stop band applies; they must stay far below audibility in every chip mode.
    for (chip, ch) in [
        (ChipMode::Free, 0),
        (ChipMode::Free, 2),
        (ChipMode::Gb, 0),
        (ChipMode::Gb, 2),
        (ChipMode::Nes, 0),
        (ChipMode::Nes, 2),
    ] {
        for note in [36u8, 48, 60, 72, 84, 96, 100, 104] {
            let mut rig = Rig::new(FS, clean(chip, ch));
            rig.set(ParamId::Analog, 0.0);
            rig.on(note, 1.0);
            rig.left(secs(FS, 0.3));
            let x = rig.left(N);
            let f0 = pixl_dsp::analysis::estimate_frequency(&x, FS as f64).unwrap();
            let r = alias_report_band(&x, FS as f64, f0, 20_000.0);
            println!(
                "{chip:?} ch{ch} note {note}: f0 {f0:.2} Hz, worst audible non-harmonic {:.1} dB at {:.0} Hz",
                r.worst_db, r.worst_hz
            );
            assert!(r.worst_db <= -90.0, "{chip:?} ch{ch} note {note}: {r:?}");
        }
    }
}
