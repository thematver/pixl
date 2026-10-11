//! SPEC §4.10 performance: 48 kHz, 128-sample host blocks (split into the plugin's 64-sample
//! sub-blocks with `set_params` before each), release build. Prints CPU % of one core and
//! asserts generous bounds (the targets are < 2 % for the default patch with 8 notes and
//! < 25 % for the worst case on an Apple M1; CI machines vary, so the asserts leave headroom).

mod common;
use common::*;
use pixl_core::{ChipMode, ParamId, ParamValues};
use std::time::Instant;

const FS: f32 = 48_000.0;
const BLOCK: usize = 128;

/// Renders `seconds` of audio in host blocks and returns the CPU load in % of one core
/// (best of three runs to reduce scheduling noise).
fn cpu_percent(p: &ParamValues, notes: &[u8], seconds: f64) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..3 {
        let mut rig = Rig::new(FS, *p);
        for &n in notes {
            rig.on(n, 0.8);
        }
        let mut l = vec![0.0f32; BLOCK];
        let mut r = vec![0.0f32; BLOCK];
        // Warm-up (attacks, caches).
        for _ in 0..200 {
            rig.render_into(&mut l, &mut r);
        }
        let blocks = (seconds * FS as f64 / BLOCK as f64) as usize;
        let t0 = Instant::now();
        let mut guard = 0.0f32;
        for _ in 0..blocks {
            rig.render_into(&mut l, &mut r);
            guard += l[0] + r[BLOCK - 1];
        }
        let dt = t0.elapsed().as_secs_f64();
        assert!(guard.is_finite());
        assert_eq!(rig.e.active_voices(), notes.len().min(16), "all voices still sounding");
        best = best.min(dt / (blocks as f64 * BLOCK as f64 / FS as f64) * 100.0);
    }
    best
}

fn worst_case(chip: ChipMode) -> ParamValues {
    let mut p = ParamValues::defaults();
    p.set(ParamId::Chip, chip.index() as f32);
    p.set(ParamId::Polyphony, 16.0);
    p.set(ParamId::UniOn, 1.0);
    p.set(ParamId::UniVoices, 4.0);
    for c in 0..4 {
        p.set(CHANNEL_ON[c], 1.0);
        p.set(CHANNEL_SUSTAIN[c], 12.0);
    }
    p.set(ParamId::NsPeriod, 0.0);
    p.set(ParamId::ArpOn, 1.0);
    p.set(ParamId::ArpChord, 1.0); // MAJ: keeps pitches moving
    p
}

#[test]
fn cpu_benchmark() {
    let chord = [48, 52, 55, 60, 64, 67, 71, 72];
    let default = cpu_percent(&ParamValues::defaults(), &chord, 4.0);
    println!("default patch (GB), 8 notes: {default:.3} % of one core");
    for chip in [ChipMode::Nes, ChipMode::Free] {
        let mut p = ParamValues::defaults();
        p.set(ParamId::Chip, chip.index() as f32);
        let c = cpu_percent(&p, &chord, 2.0);
        println!("default patch in {chip:?}, 8 notes: {c:.3} % of one core");
        assert!(c < 6.0);
    }
    let notes: Vec<u8> = (0..16).map(|i| 36 + i * 3).collect();
    let mut worst_all = 0.0f64;
    for chip in [ChipMode::Nes, ChipMode::Gb, ChipMode::Free] {
        let w = cpu_percent(&worst_case(chip), &notes, 2.0);
        println!("worst case {chip:?} (16 voices × 4 unison × 4 channels, brightest noise, arp): {w:.2} % of one core");
        worst_all = worst_all.max(w);
    }
    assert!(default < 6.0, "default patch too slow: {default:.2} %");
    assert!(worst_all < 60.0, "worst case too slow: {worst_all:.2} %");
}
