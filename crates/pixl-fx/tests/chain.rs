//! Integration tests of the complete effect chain (SPEC §5.6).

use pixl_core::{ParamId, ParamValues, TelemetrySnapshot, PARAMS, PARAM_COUNT};
use pixl_fx::meter::METER_FLOOR_DB;
use pixl_fx::output::CLIP_CEILING;
use pixl_fx::util::Rng;
use pixl_fx::FxChain;
use std::f64::consts::PI;
use std::time::Instant;

/// Parameters with every effect off, analog off and 0 dB output.
fn neutral() -> ParamValues {
    let mut p = ParamValues::defaults();
    p.set(ParamId::CrushOn, 0.0);
    p.set(ParamId::EchoOn, 0.0);
    p.set(ParamId::Analog, 0.0);
    p.set(ParamId::OutGain, 0.0);
    p
}

fn chain(fs: f32, block: usize, p: &ParamValues) -> FxChain {
    let mut fx = FxChain::new();
    fx.set_params(p);
    fx.prepare(fs, block);
    fx
}

fn sine(f: f64, fs: f64, n: usize, amp: f64, offset: usize) -> Vec<f32> {
    (0..n).map(|i| (amp * (2.0 * PI * f * (i + offset) as f64 / fs).sin()) as f32).collect()
}

fn run(fx: &mut FxChain, l: &mut [f32], r: &mut [f32], block: usize) {
    let mut i = 0;
    while i < l.len() {
        let n = block.min(l.len() - i);
        fx.process(&mut l[i..i + n], &mut r[i..i + n]);
        i += n;
    }
}

#[test]
fn neutral_chain_is_transparent_apart_from_the_dc_blocker() {
    let fs = 48_000.0;
    let mut fx = chain(fs, 128, &neutral());
    let x = sine(1000.0, fs as f64, 48_000, 0.5, 0);
    let mut l = x.clone();
    let mut r = x.clone();
    run(&mut fx, &mut l, &mut r, 128);
    let mut max_err = 0f32;
    for i in 24_000..48_000 {
        max_err = max_err.max((l[i] - x[i]).abs());
    }
    // A 5 Hz first-order high-pass changes a 1 kHz tone by < 0.0001 dB and ~0.3° of phase.
    assert!(max_err < 3e-3, "max error {max_err}");
    assert!(!fx.echo_active() && !fx.crusher_active());
}

#[test]
fn fuzz_never_produces_nan_inf_or_overs() {
    let mut rng = Rng::new(0xF022);
    let rates = [22_050.0f32, 32_000.0, 44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0];
    let fx_ids = [
        ParamId::Chip,
        ParamId::Analog,
        ParamId::OutGain,
        ParamId::CrushOn,
        ParamId::CrushBits,
        ParamId::CrushRate,
        ParamId::EchoOn,
        ParamId::EchoTime,
        ParamId::EchoSync,
        ParamId::EchoDiv,
        ParamId::EchoFb,
        ParamId::EchoMix,
        ParamId::EchoFir,
        ParamId::EchoWidth,
    ];
    let garbage = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30, 0.0, -1.0];
    let mut total = 0usize;
    for (k, &fs) in rates.iter().enumerate() {
        let max_block = [64usize, 128, 512, 1024][k % 4];
        let mut fx = FxChain::new();
        fx.prepare(fs, max_block);
        for round in 0..200 {
            let mut p = ParamValues::defaults();
            for &id in &fx_ids {
                let spec = &PARAMS[id.index()];
                let (lo, hi) = spec.range();
                // Raw writes bypass `set`'s sanitizing on purpose: the chain must cope.
                p.0[id.index()] = if rng.next_f32() < 0.05 {
                    garbage[(rng.next_u64() % garbage.len() as u64) as usize]
                } else {
                    lo + (hi - lo) * rng.next_f32()
                };
            }
            if round % 3 == 0 {
                p.0[ParamId::EchoOn.index()] = 1.0;
                p.0[ParamId::EchoFb.index()] = 95.0;
            }
            fx.set_params(&p);
            let tempo = match rng.next_u64() % 6 {
                0 => f64::NAN,
                1 => -5.0,
                2 => 0.0,
                _ => 20.0 + rng.next_f32() as f64 * 300.0,
            };
            fx.set_tempo(tempo);
            // Random block length, including over-long blocks and length 0.
            let n = (rng.next_u64() % (max_block as u64 * 2 + 1)) as usize;
            let mut l = vec![0f32; n];
            let mut r = vec![0f32; n];
            for i in 0..n {
                let pick = rng.next_f32();
                l[i] = if pick < 0.01 {
                    garbage[(rng.next_u64() % garbage.len() as u64) as usize]
                } else {
                    rng.bipolar() * 4.0
                };
                r[i] = if pick > 0.99 { f32::NAN } else { rng.bipolar() * 4.0 };
            }
            fx.process(&mut l, &mut r);
            for (&a, &b) in l.iter().zip(&r) {
                assert!(a.is_finite() && b.is_finite(), "fs {fs} round {round}");
                assert!(a.abs() <= CLIP_CEILING && b.abs() <= CLIP_CEILING, "{a} {b}");
            }
            let mut t = TelemetrySnapshot::default();
            fx.fill_telemetry(&mut t);
            for v in t.peak_db.iter().chain(&t.rms_db).chain([&t.peak_hold_db]) {
                assert!(v.is_finite() && *v >= METER_FLOOR_DB && *v <= 0.0, "{v}");
            }
            total += n;
        }
    }
    assert!(total > 100_000);
}

#[test]
fn mismatched_lengths_process_common_prefix_only() {
    let mut fx = chain(48_000.0, 64, &neutral());
    let mut l = vec![0.1f32; 100];
    let mut r = vec![0.1f32; 50];
    fx.process(&mut l, &mut r);
    assert_eq!(l[50..], vec![0.1f32; 50][..]);
}

#[test]
fn deterministic_bit_identical_output() {
    let mut p = ParamValues::defaults();
    p.set(ParamId::CrushOn, 1.0);
    p.set(ParamId::CrushBits, 5.3);
    p.set(ParamId::CrushRate, 9000.0);
    p.set(ParamId::EchoOn, 1.0);
    p.set(ParamId::EchoFb, 80.0);
    p.set(ParamId::Chip, 0.0);
    let render = || {
        let mut fx = chain(44_100.0, 256, &p);
        fx.set_tempo(133.0);
        let mut rng = Rng::new(99);
        let mut out = Vec::new();
        for _ in 0..200 {
            let mut l: Vec<f32> = (0..256).map(|_| rng.bipolar() * 0.6).collect();
            let mut r: Vec<f32> = (0..256).map(|_| rng.bipolar() * 0.6).collect();
            fx.process(&mut l, &mut r);
            out.extend(l.iter().map(|v| v.to_bits()));
            out.extend(r.iter().map(|v| v.to_bits()));
        }
        out
    };
    assert_eq!(render(), render());
}

#[test]
fn loud_input_is_caught_by_the_safety_clipper() {
    let mut p = neutral();
    p.set(ParamId::OutGain, 12.0);
    let mut fx = chain(48_000.0, 128, &p);
    let mut l = sine(200.0, 48_000.0, 48_000, 4.0, 0);
    let mut r = l.clone();
    run(&mut fx, &mut l, &mut r, 128);
    let peak = l.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak <= CLIP_CEILING && peak > 0.99, "{peak}");
    let mut t = TelemetrySnapshot::default();
    fx.fill_telemetry(&mut t);
    assert!(t.peak_hold_db > -0.02 && t.peak_hold_db <= -0.01 + 1e-4, "{}", t.peak_hold_db);
}

#[test]
fn telemetry_reports_levels() {
    let mut fx = chain(48_000.0, 128, &neutral());
    let mut l = sine(1000.0, 48_000.0, 96_000, 0.5, 0);
    let mut r = vec![0f32; 96_000];
    run(&mut fx, &mut l, &mut r, 128);
    let mut t = TelemetrySnapshot::default();
    fx.fill_telemetry(&mut t);
    assert!((t.peak_db[0] + 6.02).abs() < 0.05, "{:?}", t.peak_db);
    assert!((t.rms_db[0] + 9.03).abs() < 0.05, "{:?}", t.rms_db);
    assert_eq!(t.peak_db[1], METER_FLOOR_DB);
    assert!((t.peak_hold_db + 6.02).abs() < 0.05);
}

#[test]
fn tail_reporting_follows_echo_state() {
    let mut p = neutral();
    p.set(ParamId::EchoOn, 1.0);
    p.set(ParamId::EchoSync, 0.0);
    p.set(ParamId::EchoTime, 100.0);
    p.set(ParamId::EchoFb, 95.0);
    let mut fx = chain(48_000.0, 256, &p);
    assert_eq!(fx.tail_samples(), 0, "nothing in the line yet");
    let mut l = sine(440.0, 48_000.0, 4800, 0.5, 0);
    let mut r = l.clone();
    run(&mut fx, &mut l, &mut r, 256);
    let tail = fx.tail_samples();
    // 0.5 at 95 %: ~200 repeats of 100 ms until below one 16-bit LSB.
    println!("tail estimate at fb 95 %, 100 ms: {:.2} s", tail as f64 / 48_000.0);
    assert!(tail > 48_000 * 10 && tail < 48_000 * 30, "{tail}");
    p.set(ParamId::EchoOn, 0.0);
    p.set(ParamId::EchoFb, 30.0);
    fx.set_params(&p);
    let mut silent_after = None;
    for b in 0..2000 {
        let mut l = vec![0f32; 256];
        let mut r = vec![0f32; 256];
        fx.process(&mut l, &mut r);
        if !fx.echo_active() {
            silent_after = Some(b);
            break;
        }
    }
    assert!(silent_after.is_some(), "echo must go idle");
    assert_eq!(fx.tail_samples(), 0);
}

#[test]
fn every_host_rate_runs_the_echo_with_exact_timing() {
    for fs in [22_050.0f32, 32_000.0, 44_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0, 192_000.0] {
        let mut p = neutral();
        p.set(ParamId::EchoOn, 1.0);
        p.set(ParamId::EchoSync, 1.0);
        p.set(ParamId::EchoDiv, 4.0); // 1/16
        p.set(ParamId::EchoFb, 0.0);
        p.set(ParamId::EchoMix, 100.0);
        p.set(ParamId::EchoFir, 0.0); // FLAT
        p.set(ParamId::EchoWidth, 0.0);
        let mut fx = chain(fs, 512, &p);
        fx.set_tempo(150.0); // 1/16 = 100 ms
        let n = (fs * 0.2) as usize;
        let mut l = vec![0f32; n];
        let mut r = vec![0f32; n];
        // Band-limited click: a short Hann burst centred at sample 100.
        for i in 0..41 {
            l[80 + i] = (0.5 - 0.5 * (2.0 * PI * i as f64 / 40.0).cos()) as f32 * 0.5;
        }
        run(&mut fx, &mut l, &mut r, 512);
        let expect = 100.0 + 0.1 * fs as f64;
        let lo = (expect - 60.0) as usize;
        let hi = (expect + 60.0) as usize;
        let (mut best, mut bv) = (lo, 0f32);
        for (i, &v) in l.iter().enumerate().take(hi).skip(lo) {
            if v > bv {
                bv = v;
                best = i;
            }
        }
        println!("fs {fs}: echo peak at {best} (ideal {expect:.1}), level {bv:.4}");
        assert!((best as f64 - expect).abs() <= 1.0, "fs {fs}: {best} vs {expect}");
        assert!((bv - 0.5 * 127.0 / 128.0).abs() < 0.01, "fs {fs}: level {bv}");
    }
}

#[test]
fn all_parameters_in_table_are_read_safely() {
    // Every slot of the value array filled with garbage must not upset the chain.
    let mut p = ParamValues::defaults();
    for i in 0..PARAM_COUNT {
        p.0[i] = f32::NAN;
    }
    let mut fx = chain(48_000.0, 64, &p);
    let mut l = vec![0.3f32; 64];
    let mut r = vec![-0.3f32; 64];
    fx.process(&mut l, &mut r);
    assert!(l.iter().chain(&r).all(|v| v.is_finite()));
}

#[test]
fn performance_full_chain() {
    // Everything on: crusher with fractional hold, NES filter, echo at max feedback.
    let mut p = ParamValues::defaults();
    p.set(ParamId::Chip, 0.0);
    p.set(ParamId::CrushOn, 1.0);
    p.set(ParamId::CrushRate, 7777.0);
    p.set(ParamId::EchoOn, 1.0);
    p.set(ParamId::EchoFb, 95.0);
    // 64-sample blocks with `set_params` before each, exactly like the plugin's sub-blocks.
    for (fs, block) in [(44_100.0f32, 64usize), (48_000.0, 64), (96_000.0, 64), (192_000.0, 64)] {
        let mut fx = chain(fs, block, &p);
        let seconds = 4.0;
        let n = (fs * seconds) as usize;
        let mut rng = Rng::new(5);
        let mut l: Vec<f32> = (0..n).map(|_| rng.bipolar() * 0.5).collect();
        let mut r = l.clone();
        let t0 = Instant::now();
        let mut i = 0;
        while i < n {
            let len = block.min(n - i);
            fx.set_params(&p);
            fx.process(&mut l[i..i + len], &mut r[i..i + len]);
            i += len;
        }
        let el = t0.elapsed().as_secs_f64();
        let pct = el / seconds as f64 * 100.0;
        println!("full chain {fs} Hz / {block}: {:.3} % of one core", pct);
        // Generous bound for an optimized build on any CI machine (unoptimized debug builds
        // only report the number).
        if !cfg!(debug_assertions) {
            assert!(pct < 10.0, "{pct}");
        }
    }
}

/// Runs a 220 Hz sine through the chain while parameters jump every 100 ms and returns the
/// largest second difference `|y[n] − 2y[n−1] + y[n−2]|` of either channel (a click of size
/// J shows up as ≈ J; the clean sine itself has `A·ω²`).
fn max_second_difference(changes: &[(ParamId, f32)], base: &ParamValues, tempo: f64) -> (f32, f32) {
    let fs = 48_000.0;
    let mut fx = chain(fs, 64, base);
    fx.set_tempo(tempo);
    let mut p = *base;
    let block = 64;
    let per_change = 75; // 100 ms
    let blocks = per_change * (changes.len() + 4);
    let amp = 0.25;
    let mut hist = [[0f32; 2]; 2];
    let mut worst = 0f32;
    let mut steady = 0f32;
    for b in 0..blocks {
        if b >= 2 * per_change && (b - 2 * per_change) % per_change == 0 {
            if let Some(&(id, v)) = changes.get((b - 2 * per_change) / per_change) {
                p.set(id, v);
            }
        }
        fx.set_params(&p);
        let mut l = sine(220.0, fs as f64, block, amp, b * block);
        let mut r = sine(220.0, fs as f64, block, amp * 0.5, b * block);
        fx.process(&mut l, &mut r);
        for i in 0..block {
            for (c, y) in [l[i], r[i]].into_iter().enumerate() {
                let d2 = (y - 2.0 * hist[c][0] + hist[c][1]).abs();
                if b >= per_change && b < 2 * per_change {
                    steady = steady.max(d2);
                } else if b >= 2 * per_change {
                    worst = worst.max(d2);
                }
                hist[c] = [y, hist[c][0]];
            }
        }
    }
    (steady, worst)
}

#[test]
fn parameter_changes_are_click_free() {
    let mut base = neutral();
    base.set(ParamId::CrushBits, 16.0);
    base.set(ParamId::CrushRate, 48_000.0);
    base.set(ParamId::EchoSync, 0.0);
    base.set(ParamId::EchoTime, 180.0);
    base.set(ParamId::EchoFb, 50.0);
    base.set(ParamId::EchoMix, 100.0);
    let cases: [(&str, Vec<(ParamId, f32)>); 5] = [
        (
            "crusher on/off (16 bit, 48 kHz)",
            vec![(ParamId::CrushOn, 1.0), (ParamId::CrushOn, 0.0), (ParamId::CrushOn, 1.0)],
        ),
        (
            "chip filter NES/GB/FREE/analog off",
            vec![
                (ParamId::Chip, 0.0),
                (ParamId::Analog, 1.0),
                (ParamId::Chip, 1.0),
                (ParamId::Chip, 2.0),
                (ParamId::Chip, 0.0),
                (ParamId::Analog, 0.0),
            ],
        ),
        (
            "output gain jumps",
            vec![
                (ParamId::OutGain, -12.0),
                (ParamId::OutGain, 0.0),
                (ParamId::OutGain, -36.0),
                (ParamId::OutGain, -3.0),
            ],
        ),
        (
            "echo on/off, time, FIR, width, feedback",
            vec![
                (ParamId::EchoOn, 1.0),
                (ParamId::EchoTime, 333.0),
                (ParamId::EchoFir, 4.0),
                (ParamId::EchoWidth, 100.0),
                (ParamId::EchoFb, 95.0),
                (ParamId::EchoFb, 20.0),
                (ParamId::EchoTime, 20.0),
                (ParamId::EchoFir, 2.0),
                (ParamId::EchoOn, 0.0),
                (ParamId::EchoOn, 1.0),
            ],
        ),
        (
            "echo sync on (tempo division)",
            vec![(ParamId::EchoOn, 1.0), (ParamId::EchoSync, 1.0), (ParamId::EchoDiv, 5.0), (ParamId::EchoDiv, 0.0)],
        ),
    ];
    for (name, changes) in cases {
        let (steady, worst) = max_second_difference(&changes, &base, 128.0);
        println!("{name}: max |Δ²| steady {steady:.2e}, around changes {worst:.2e}");
        // −54 dBFS: far below anything audible as a click, far above the signal's own Δ².
        assert!(worst < 2e-3, "{name}: {worst}");
    }
}

#[test]
fn enabled_echo_sleeps_when_idle_and_wakes_on_signal() {
    let fs = 48_000.0f32;
    let mut p = neutral();
    p.set(ParamId::EchoOn, 1.0);
    p.set(ParamId::EchoSync, 0.0);
    p.set(ParamId::EchoTime, 50.0);
    p.set(ParamId::EchoFb, 60.0);
    p.set(ParamId::EchoMix, 100.0);
    p.set(ParamId::EchoFir, 0.0);
    p.set(ParamId::EchoWidth, 0.0);
    let mut fx = chain(fs, 64, &p);
    assert!(!fx.echo_active(), "nothing to process yet");
    // A short burst, then silence: the echo rings, decays and falls asleep while still on.
    let mut l = sine(1000.0, fs as f64, 2400, 0.5, 0);
    let mut r = l.clone();
    run(&mut fx, &mut l, &mut r, 64);
    assert!(fx.echo_active() && fx.tail_samples() > 0);
    let mut slept_after = None;
    for b in 0..20_000 {
        let mut l = vec![0f32; 64];
        let mut r = vec![0f32; 64];
        fx.process(&mut l, &mut r);
        if !fx.echo_active() {
            slept_after = Some(b * 64);
            break;
        }
    }
    let slept_after = slept_after.expect("enabled echo must go to sleep on silence");
    println!("echo (on) asleep {:.2} s after the input stopped", slept_after as f64 / fs as f64);
    assert_eq!(fx.tail_samples(), 0);
    // Silence keeps it asleep (no CPU); only the DC blocker's last decay remains.
    let mut l = vec![0f32; 64];
    let mut r = vec![0f32; 64];
    fx.process(&mut l, &mut r);
    assert!(!fx.echo_active() && l.iter().all(|&v| v.abs() < 1e-9));
    // A new click wakes it; the repeat comes 50 ms later at full level (no fade-in).
    let n = 4800;
    let mut l = vec![0f32; n];
    let mut r = vec![0f32; n];
    for i in 0..41 {
        l[100 + i] = (0.5 - 0.5 * (2.0 * PI * i as f64 / 40.0).cos()) as f32 * 0.5;
    }
    run(&mut fx, &mut l, &mut r, 64);
    assert!(fx.echo_active());
    let expect = 120 + (0.05 * fs) as usize;
    let (best, peak) =
        l.iter().enumerate().skip(expect - 60).take(120).fold(
            (0, 0f32),
            |(bi, bv), (i, &v)| {
                if v > bv {
                    (i, v)
                } else {
                    (bi, bv)
                }
            },
        );
    println!("wake-up repeat at {best} (ideal {expect}), level {peak:.4}");
    assert!((best as i64 - expect as i64).abs() <= 1, "{best} vs {expect}");
    assert!((peak - 0.5 * 127.0 / 128.0).abs() < 0.01, "{peak}");
}

#[test]
fn chain_can_move_to_the_audio_thread() {
    fn assert_send<T: Send + 'static>() {}
    assert_send::<FxChain>();
}
