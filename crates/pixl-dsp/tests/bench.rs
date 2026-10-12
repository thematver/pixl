//! SPEC §4.10 performance: 48 kHz, 128-sample host blocks (split into the plugin's 64-sample
//! sub-blocks with `set_params` before each), release build. Prints CPU % of one core and
//! asserts generous bounds (the targets are < 2 % for the default patch with 8 notes and
//! < 25 % for the worst case on an Apple M1).
//!
//! Timing on a shared machine is noisy (other builds, indexing, efficiency cores), so every
//! figure is the best of several short runs, and the bounds are checked relative to a
//! calibration loop measured alongside: a fixed, engine-independent workload of the same kind
//! (band-limited step adds into a small buffer, f64 integration, scalar control) whose time
//! tracks how fast this machine runs right now. A loaded machine slows both down alike, while a
//! regression in the engine only slows down the engine.

mod common;
use common::*;
use pixl_core::{ChipMode, ParamId, ParamValues};
use std::hint::black_box;
use std::time::Instant;

const FS: f32 = 48_000.0;
const BLOCK: usize = 128;
/// Runs per figure (the best one counts).
const RUNS: usize = 7;
/// Rendered seconds per run.
const SECONDS: f64 = 0.5;

/// Calibration workload: 2·10⁶ 32-tap kernel adds (seconds per run).
fn calibration_run() -> f64 {
    let mut kernel = [0f32; 64];
    for (j, k) in kernel.iter_mut().enumerate() {
        *k = (j as f32 * 0.37).sin() * 0.05 / (1.0 + j as f32 * 0.1);
    }
    let mut buf = [0f32; 192];
    let (mut acc, mut d, mut t) = (0f64, 0.25f32, 0.0f64);
    let iters = black_box(2_000_000usize);
    let t0 = Instant::now();
    for i in 0..iters {
        t += 0.731;
        if t >= 128.0 {
            t -= 128.0;
        }
        let idx = t as usize;
        let f = (t - idx as f64) as f32;
        let (r0, r1) = kernel.split_at(32);
        for (j, dst) in buf[idx..idx + 32].iter_mut().enumerate() {
            *dst += d * (r0[j] + f * (r1[j] - r0[j]));
        }
        d = if i & 3 == 0 { -d } else { d * 0.9999 + 1e-4 };
        acc += buf[(i * 13) & 127] as f64;
        if i & 127 == 127 {
            buf[..64].fill(0.0);
        }
    }
    black_box(acc);
    t0.elapsed().as_secs_f64()
}

/// One run: renders [`SECONDS`] of audio in host blocks and returns the CPU load in % of one
/// core.
fn cpu_run(p: &ParamValues, notes: &[u8]) -> f64 {
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
    let blocks = (SECONDS * FS as f64 / BLOCK as f64) as usize;
    let t0 = Instant::now();
    let mut guard = 0.0f32;
    for _ in 0..blocks {
        rig.render_into(&mut l, &mut r);
        guard += l[0] + r[BLOCK - 1];
    }
    let dt = t0.elapsed().as_secs_f64();
    assert!(guard.is_finite());
    assert_eq!(rig.e.active_voices(), notes.len().min(16), "all voices still sounding");
    dt / (blocks as f64 * BLOCK as f64 / FS as f64) * 100.0
}

/// Best-of-[`RUNS`] CPU % and the best calibration time (ms) measured between the runs.
fn measure(p: &ParamValues, notes: &[u8]) -> (f64, f64) {
    let (mut cpu, mut cal) = (f64::MAX, f64::MAX);
    for _ in 0..RUNS {
        cal = cal.min(calibration_run() * 1e3);
        cpu = cpu.min(cpu_run(p, notes));
    }
    (cpu, cal)
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

/// Generous bounds in % of one core per millisecond of calibration time. Measured on an
/// Apple M4 Max under heavy load (load average 7–13, calibration 29–33 ms): default patch
/// 0.016–0.020, NES/FREE default patch 0.034–0.084, worst case NES 0.46–0.53, GB 0.99–1.15,
/// FREE 1.31–1.38 %/ms. The bounds leave 60–100 % headroom over those ratios: a busy machine
/// does not trip them, a large regression does (the printed figures show smaller ones).
const DEFAULT_BOUND: f64 = 0.04;
const CHIP_DEFAULT_BOUND: f64 = 0.15;
const WORST_BOUND: f64 = 2.2;

#[test]
fn cpu_benchmark() {
    let chord = [48, 52, 55, 60, 64, 67, 71, 72];
    let (default, cal) = measure(&ParamValues::defaults(), &chord);
    println!("calibration: {cal:.2} ms");
    println!("default patch (GB), 8 notes: {default:.3} % of one core ({:.4} %/ms)", default / cal);
    assert!(default / cal < DEFAULT_BOUND, "default patch too slow: {default:.2} % at calibration {cal:.2} ms");
    for chip in [ChipMode::Nes, ChipMode::Free] {
        let mut p = ParamValues::defaults();
        p.set(ParamId::Chip, chip.index() as f32);
        let (c, cal) = measure(&p, &chord);
        println!("default patch in {chip:?}, 8 notes: {c:.3} % of one core ({:.4} %/ms)", c / cal);
        assert!(
            c / cal < CHIP_DEFAULT_BOUND,
            "default patch in {chip:?} too slow: {c:.2} % at calibration {cal:.2} ms"
        );
    }
    let notes: Vec<u8> = (0..16).map(|i| 36 + i * 3).collect();
    for chip in [ChipMode::Nes, ChipMode::Gb, ChipMode::Free] {
        let (w, cal) = measure(&worst_case(chip), &notes);
        println!(
            "worst case {chip:?} (16 voices × 4 unison × 4 channels, brightest noise, arp): {w:.2} % of one core \
             ({:.3} %/ms)",
            w / cal
        );
        assert!(w / cal < WORST_BOUND, "worst case {chip:?} too slow: {w:.2} % at calibration {cal:.2} ms");
    }
}
