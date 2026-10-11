//! Renders test material through the complete PIXL effect chain into WAV files for listening
//! tests, plus the echo FIR magnitude responses as CSV.
//!
//! ```text
//! cargo run -p pixl-fx --release --example render_fx [-- <out_dir>]
//! ```
//!
//! Default output directory: `<workspace>/target/pixl-renders/fx`. Material (48 kHz stereo,
//! 24-bit PCM):
//!
//! - `impulse`: a single click followed by silence (shows the echo's repeat spacing, FIR
//!   colouring per repeat and the decay),
//! - `sweep`: exponential sine sweep 20 Hz → 20 kHz at −12 dBFS (shows filter responses and
//!   the crusher's images/aliasing),
//! - `melody`: a short 25 %-duty square-wave phrase at 140 BPM, rendered alias-free with the
//!   crate's minBLEP (shows the effects on chip-like material).
//!
//! Every file goes through [`FxChain`] exactly like the plugin drives it (64-sample
//! sub-blocks, `set_params` before each), with the DC blocker, output stage and meters.

use pixl_core::{ParamId, ParamValues};
use pixl_fx::blep::{MinBlep, RING_LEN, RING_MASK};
use pixl_fx::echo::{fir_coeffs, fir_magnitude};
use pixl_fx::FxChain;
use std::f64::consts::PI;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const FS: u32 = 48_000;
const SUB_BLOCK: usize = 64;
const TEMPO: f64 = 140.0;

fn main() -> io::Result<()> {
    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/pixl-renders/fx"));
    std::fs::create_dir_all(&out_dir)?;
    println!("PIXL fx renders → {}", out_dir.display());

    let impulse = impulse();
    let sweep = sweep();
    let melody = melody();
    let materials: [(&str, &[f32]); 3] = [("impulse", &impulse), ("sweep", &sweep), ("melody", &melody)];

    // Dry references (everything off: only the DC blocker and the output stage).
    for (name, x) in &materials {
        render(&out_dir, &format!("dry_{name}"), &neutral(), x, 0.5)?;
    }

    // Echo: every FIR preset on the impulse (free 250 ms, fb 70 %, wet only) and on the
    // melody (synced 1/8D, fb 45 %, mix 40 %, width 50 %).
    for (i, label) in pixl_core::EchoFir::LABELS.iter().enumerate() {
        let lower = label.to_lowercase();
        let mut p = neutral();
        p.set(ParamId::EchoOn, 1.0);
        p.set(ParamId::EchoFir, i as f32);
        p.set(ParamId::EchoSync, 0.0);
        p.set(ParamId::EchoTime, 250.0);
        p.set(ParamId::EchoFb, 70.0);
        p.set(ParamId::EchoMix, 100.0);
        p.set(ParamId::EchoWidth, 0.0);
        render(&out_dir, &format!("echo_{lower}_impulse"), &p, &impulse, 4.0)?;

        let mut p = neutral();
        p.set(ParamId::EchoOn, 1.0);
        p.set(ParamId::EchoFir, i as f32);
        p.set(ParamId::EchoSync, 1.0);
        p.set(ParamId::EchoDiv, 2.0); // 1/8D
        p.set(ParamId::EchoFb, 45.0);
        p.set(ParamId::EchoMix, 40.0);
        p.set(ParamId::EchoWidth, 50.0);
        render(&out_dir, &format!("echo_{lower}_melody"), &p, &melody, 4.0)?;
    }
    // Echo extremes: full ping-pong, and maximum feedback (long, stable, self-limiting tail).
    {
        let mut p = neutral();
        p.set(ParamId::EchoOn, 1.0);
        p.set(ParamId::EchoFir, 1.0); // WARM
        p.set(ParamId::EchoSync, 1.0);
        p.set(ParamId::EchoDiv, 2.0);
        p.set(ParamId::EchoFb, 60.0);
        p.set(ParamId::EchoMix, 60.0);
        p.set(ParamId::EchoWidth, 100.0);
        render(&out_dir, "echo_pingpong_melody", &p, &melody, 4.0)?;
        p.set(ParamId::EchoWidth, 0.0);
        p.set(ParamId::EchoFb, 95.0);
        p.set(ParamId::EchoDiv, 4.0); // 1/16
        render(&out_dir, "echo_fb95_warm_melody", &p, &melody, 12.0)?;
        p.set(ParamId::EchoFir, 2.0); // DARK
        render(&out_dir, "echo_fb95_dark_melody", &p, &melody, 12.0)?;
    }

    // Crusher settings on the melody and the sweep.
    let crush: [(&str, f32, f32); 7] = [
        ("8bit_22k", 8.0, 22_050.0),
        ("6bit_11k", 6.0, 11_025.0), // parameter defaults
        ("4bit_8k", 4.0, 8_000.0),
        ("3bit_5k5", 3.0, 5_512.5),
        ("12bit_4k41", 12.0, 4_410.0), // 10.88-sample hold: fractional, jitter-free
        ("2bit_48k", 2.0, 48_000.0),   // quantize only
        ("5p5bit_16k", 5.5, 16_000.0), // fractional bit depth
    ];
    for (name, bits, rate) in crush {
        let mut p = neutral();
        p.set(ParamId::CrushOn, 1.0);
        p.set(ParamId::CrushBits, bits);
        p.set(ParamId::CrushRate, rate);
        render(&out_dir, &format!("crush_{name}_melody"), &p, &melody, 0.5)?;
        render(&out_dir, &format!("crush_{name}_sweep"), &p, &sweep, 0.5)?;
    }

    // Chip output filters (analog on): NES 90 Hz + 440 Hz HPF + 14 kHz LPF, GB 28 Hz HPF.
    for (name, chip) in [("nes", 0.0), ("gb", 1.0)] {
        let mut p = neutral();
        p.set(ParamId::Analog, 1.0);
        p.set(ParamId::Chip, chip);
        render(&out_dir, &format!("filter_{name}_melody"), &p, &melody, 0.5)?;
        render(&out_dir, &format!("filter_{name}_sweep"), &p, &sweep, 0.5)?;
    }

    // Everything at once: the parameter-table defaults with crush and echo switched on.
    {
        let mut p = ParamValues::defaults();
        p.set(ParamId::CrushOn, 1.0);
        p.set(ParamId::EchoOn, 1.0);
        render(&out_dir, "chain_defaults_crush_echo_melody", &p, &melody, 4.0)?;
    }

    write_fir_csv(&out_dir.join("fir_responses.csv"))?;
    println!("done");
    Ok(())
}

/// Parameters with every effect off, analog off and 0 dB output.
fn neutral() -> ParamValues {
    let mut p = ParamValues::defaults();
    p.set(ParamId::CrushOn, 0.0);
    p.set(ParamId::EchoOn, 0.0);
    p.set(ParamId::Analog, 0.0);
    p.set(ParamId::OutGain, 0.0);
    p
}

/// Runs mono material (duplicated to both channels) plus `tail_s` of silence through a fresh
/// chain and writes the result.
fn render(dir: &Path, name: &str, p: &ParamValues, x: &[f32], tail_s: f64) -> io::Result<()> {
    let n = x.len() + (tail_s * FS as f64) as usize;
    let mut l = vec![0f32; n];
    l[..x.len()].copy_from_slice(x);
    let mut r = l.clone();
    let mut fx = FxChain::new();
    fx.prepare(FS as f32, SUB_BLOCK);
    fx.set_tempo(TEMPO);
    let mut i = 0;
    while i < n {
        let len = SUB_BLOCK.min(n - i);
        fx.set_params(p);
        fx.process(&mut l[i..i + len], &mut r[i..i + len]);
        i += len;
    }
    let finite = l.iter().chain(&r).all(|v| v.is_finite());
    let peak = l.iter().chain(&r).fold(0f32, |m, v| m.max(v.abs()));
    let rms = (l.iter().chain(&r).map(|v| (*v as f64).powi(2)).sum::<f64>() / (2 * n) as f64).sqrt();
    let path = dir.join(format!("{name}.wav"));
    write_wav(&path, &l, &r)?;
    println!(
        "  {name:<34} {:>5.2} s  peak {:>6.1} dBFS  rms {:>6.1} dBFS{}",
        n as f64 / FS as f64,
        20.0 * (peak.max(1e-9) as f64).log10(),
        20.0 * rms.max(1e-9).log10(),
        if finite { "" } else { "  NON-FINITE!" }
    );
    Ok(())
}

/// A single 0.5 click at 50 ms, then silence (total 0.5 s; the render adds the tail).
fn impulse() -> Vec<f32> {
    let mut x = vec![0f32; FS as usize / 2];
    x[FS as usize / 20] = 0.5;
    x
}

/// Exponential sine sweep 20 Hz → 20 kHz over 6 s at −12 dBFS with 10 ms fades.
fn sweep() -> Vec<f32> {
    let dur = 6.0;
    let n = (dur * FS as f64) as usize;
    let (f1, f2) = (20.0f64, 20_000.0f64);
    let k = (f2 / f1).ln();
    let fade = (0.01 * FS as f64) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / FS as f64;
            let phase = 2.0 * PI * f1 * dur / k * ((t / dur * k).exp() - 1.0);
            let g = (i.min(n - 1 - i) as f64 / fade as f64).min(1.0);
            (0.25 * g * phase.sin()) as f32
        })
        .collect()
}

/// A 25 %-duty square-wave phrase (eighth notes at 140 BPM, 60 % gate, short release),
/// rendered band-limited with minBLEP steps.
fn melody() -> Vec<f32> {
    // MIDI notes; 0 = rest.
    const NOTES: [u8; 24] =
        [72, 76, 79, 84, 83, 79, 76, 74, 77, 81, 84, 89, 88, 84, 81, 79, 76, 79, 83, 86, 84, 0, 84, 0];
    let step = 60.0 / TEMPO / 2.0;
    let n = ((NOTES.len() as f64 + 2.0) * step * FS as f64) as usize;
    let blep = MinBlep::get();
    let mut ring = [0f32; RING_LEN];
    let mut out = vec![0f32; n];
    let amp = 0.35f32;
    let duty = 0.25f64;
    let mut level = 0f32; // current naive output level (amplitude × waveform state)
    let mut phase = 0f64; // waveform phase in cycles
    for (i, y) in out.iter_mut().enumerate() {
        let t = i as f64 / FS as f64;
        let k = (t / step) as usize;
        let note = NOTES.get(k).copied().unwrap_or(0);
        let in_note = t - k as f64 * step;
        let gate = step * 0.6;
        // Linear 3 ms attack, 40 ms release after the gate (stepped per sample: tiny steps).
        let env = if note == 0 {
            0.0
        } else if in_note < gate {
            (in_note / 0.003).min(1.0)
        } else {
            (1.0 - (in_note - gate) / 0.04).max(0.0)
        } as f32;
        let f = if note == 0 { 0.0 } else { 440.0 * 2f64.powf((note as f64 - 69.0) / 12.0) };
        let inc = f / FS as f64;
        // Band-limited waveform edges within (i − 1, i].
        let mut high = phase.fract() < duty;
        let start = phase;
        phase += inc;
        let edges = [start.floor() + duty, start.floor() + 1.0, start.floor() + 1.0 + duty];
        for &e in &edges {
            if e > start && e <= phase {
                let x0 = ((phase - e) / inc.max(1e-12)) as f32; // samples since the edge
                let new_high = !high;
                let target = if new_high { amp } else { -amp } * env;
                blep.add_step(&mut ring, i, x0.clamp(0.0, 0.999_999), target - level);
                level = target;
                high = new_high;
            }
        }
        // Envelope changes between edges (level follows the envelope at the current state).
        let target = if high { amp } else { -amp } * env;
        if target != level {
            blep.add_step(&mut ring, i, 0.0, target - level);
            level = target;
        }
        let slot = i & RING_MASK;
        *y = level + ring[slot];
        ring[slot] = 0.0;
    }
    out
}

/// Writes 24-bit PCM stereo.
fn write_wav(path: &Path, l: &[f32], r: &[f32]) -> io::Result<()> {
    let n = l.len().min(r.len());
    let data_len = (n * 2 * 3) as u32;
    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVEfmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&2u16.to_le_bytes()); // channels
    buf.extend_from_slice(&FS.to_le_bytes());
    buf.extend_from_slice(&(FS * 6).to_le_bytes()); // byte rate
    buf.extend_from_slice(&6u16.to_le_bytes()); // block align
    buf.extend_from_slice(&24u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        for x in [l[i], r[i]] {
            let v = (x.clamp(-1.0, 1.0) as f64 * 8_388_607.0).round() as i32;
            buf.extend_from_slice(&v.to_le_bytes()[..3]);
        }
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(&buf)
}

/// Magnitude responses of the echo FIR presets (dB at 32 kHz, 50 Hz grid).
fn write_fir_csv(path: &Path) -> io::Result<()> {
    let mut s = String::from("freq_hz");
    for label in pixl_core::EchoFir::LABELS {
        s += &format!(",{label}");
    }
    s.push('\n');
    for k in 0..=320 {
        let f = k as f64 * 50.0;
        s += &format!("{f}");
        for &fir in pixl_core::EchoFir::ALL {
            let m = fir_magnitude(fir, f);
            s += &format!(",{:.3}", 20.0 * m.max(1e-9).log10());
        }
        s.push('\n');
    }
    std::fs::write(path, s)?;
    println!("  fir_responses.csv (coefficients, Q7):");
    for &fir in pixl_core::EchoFir::ALL {
        println!("    {:<6} {:?}", fir.label(), fir_coeffs(fir));
    }
    Ok(())
}
