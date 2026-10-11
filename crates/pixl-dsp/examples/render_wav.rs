//! Renders demo phrases for every chip mode to `target/pixl-renders/*.wav` (32-bit float
//! stereo, 48 kHz) and verifies them numerically (pitch of every note in cents, levels,
//! drum-hit timing), so the engine can be checked without listening.
//!
//! ```text
//! cargo run -p pixl-dsp --release --example render_wav
//! ```
//!
//! The engine output is the raw voice signal (before `pixl-fx`): unipolar chip DACs carry DC
//! that the effect chain's analog filters / DC blocker remove. For listening, this example
//! applies the same first-order chip output filters as `pixl-fx` (SPEC §5.3: NES HPF 90 Hz →
//! HPF 440 Hz → LPF 14 kHz, GB HPF 28 Hz, when `analog` is on) and the 5 Hz DC blocker, and
//! reports levels after them. No limiter is applied, so chord onsets may exceed 0 dBFS (the
//! plugin's output stage protects; polyphony is not auto-scaled, SPEC §4.3).

use pixl_core::{ArpChord, ArpMode, ChipMode, Curve, GlideMode, NoiseMode, ParamId, ParamValues, VoiceMode};
use pixl_dsp::analysis::{db, estimate_frequency, peak, rms};
use pixl_dsp::chip;
use pixl_dsp::{Engine, EngineEvent, Transport};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const FS: f32 = 48_000.0;
const SUB: usize = 64;

/// A timed event (seconds).
#[derive(Clone, Copy, Debug)]
struct Ev {
    t: f64,
    ev: EngineEvent,
}

/// Note helper: on at `t` for `dur` seconds.
fn note(evs: &mut Vec<Ev>, t: f64, dur: f64, n: u8, vel: f32) {
    evs.push(Ev { t, ev: EngineEvent::NoteOn { note: n, velocity: vel } });
    evs.push(Ev { t: t + dur, ev: EngineEvent::NoteOff { note: n } });
}

/// Renders `evs` with patch `p` for `seconds`, splitting blocks at events and every 64
/// samples exactly like the plugin's processor.
fn render(p: &ParamValues, evs: &[Ev], seconds: f64, tempo: f64) -> (Vec<f32>, Vec<f32>) {
    let mut e = Engine::new(SUB);
    e.prepare(FS, SUB);
    e.set_transport(Transport { tempo_bpm: tempo, playing: true, beat_pos: Some(0.0) });
    e.set_params(p);
    let mut evs = evs.to_vec();
    // Stable sort: note-offs at the same instant as note-ons keep their order.
    evs.sort_by(|a, b| a.t.total_cmp(&b.t));
    let n = (seconds * FS as f64) as usize;
    let mut l = vec![0.0f32; n];
    let mut r = vec![0.0f32; n];
    let mut pos = 0usize;
    let mut next = 0usize;
    while pos < n {
        e.set_params(p);
        while next < evs.len() && (evs[next].t * FS as f64).round() as usize <= pos {
            e.event(evs[next].ev);
            next += 1;
        }
        let limit = if next < evs.len() { ((evs[next].t * FS as f64).round() as usize).min(n) } else { n };
        let end = (pos + SUB).min(limit).max(pos + 1).min(n);
        e.render(&mut l[pos..end], &mut r[pos..end]);
        pos = end;
    }
    (l, r)
}

/// First-order high-pass (bilinear-free one-pole form, matched at low frequencies).
fn hpf(x: &mut [f32], hz: f64) {
    let rc = 1.0 / (2.0 * std::f64::consts::PI * hz);
    let dt = 1.0 / FS as f64;
    let a = rc / (rc + dt);
    let (mut x1, mut y1) = (0.0f64, 0.0f64);
    for v in x.iter_mut() {
        let xi = *v as f64;
        let y = a * (y1 + xi - x1);
        x1 = xi;
        y1 = y;
        *v = y as f32;
    }
}

/// First-order low-pass.
fn lpf(x: &mut [f32], hz: f64) {
    let rc = 1.0 / (2.0 * std::f64::consts::PI * hz);
    let dt = 1.0 / FS as f64;
    let a = dt / (rc + dt);
    let mut y = 0.0f64;
    for v in x.iter_mut() {
        y += a * (*v as f64 - y);
        *v = y as f32;
    }
}

/// The chip output filter of `pixl-fx` for a patch, followed by the 5 Hz DC blocker.
fn output_filter(p: &ParamValues, x: &mut [f32]) {
    if p.bool(ParamId::Analog) {
        match p.chip() {
            ChipMode::Nes => {
                hpf(x, 90.0);
                hpf(x, 440.0);
                lpf(x, 14_000.0);
            }
            ChipMode::Gb => hpf(x, 28.0),
            ChipMode::Free => {}
        }
    }
    hpf(x, 5.0);
}

/// Writes a 32-bit float stereo WAV.
fn write_wav(path: &Path, l: &[f32], r: &[f32]) -> std::io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    let frames = l.len().min(r.len()) as u32;
    let data_len = frames * 8;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&3u16.to_le_bytes())?; // IEEE float
    w.write_all(&2u16.to_le_bytes())?;
    w.write_all(&(FS as u32).to_le_bytes())?;
    w.write_all(&(FS as u32 * 8).to_le_bytes())?;
    w.write_all(&8u16.to_le_bytes())?;
    w.write_all(&32u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    for i in 0..frames as usize {
        w.write_all(&l[i].to_le_bytes())?;
        w.write_all(&r[i].to_le_bytes())?;
    }
    w.flush()
}

fn out_dir() -> PathBuf {
    // crates/pixl-dsp → workspace root.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = root.join("target/pixl-renders");
    std::fs::create_dir_all(&dir).expect("create target/pixl-renders");
    dir
}

fn save(name: &str, p: &ParamValues, mut l: Vec<f32>, mut r: Vec<f32>) -> (Vec<f32>, Vec<f32>) {
    output_filter(p, &mut l);
    output_filter(p, &mut r);
    let path = out_dir().join(name);
    write_wav(&path, &l, &r).expect("write wav");
    let pk = peak(&l).max(peak(&r));
    println!(
        "{name:28} peak {:6.2} dBFS  rms {:6.2} dB  ({:.1} s)",
        db(pk),
        db(rms(&l).max(rms(&r))),
        l.len() as f64 / FS as f64
    );
    assert!(l.iter().chain(r.iter()).all(|v| v.is_finite()), "{name}: non-finite samples");
    (l, r)
}

/// Basic lead patch for a chip: pulse 1 (25 %) + a quiet pulse 2 an octave below, short
/// sweep, delayed vibrato, no unison.
fn lead(chip: ChipMode) -> ParamValues {
    let mut p = ParamValues::defaults();
    p.set(ParamId::Chip, chip.index() as f32);
    p.set(ParamId::P1On, 1.0);
    p.set(ParamId::P1Level, 13.0);
    p.set(ParamId::P1Duty, 25.0);
    p.set(ParamId::P1Attack, 2.0);
    p.set(ParamId::P1Decay, 600.0);
    p.set(ParamId::P1Sustain, 9.0);
    p.set(ParamId::P1Release, 120.0);
    p.set(ParamId::P2On, 0.0);
    p.set(ParamId::WvOn, 0.0);
    p.set(ParamId::NsOn, 0.0);
    p.set(ParamId::SweepOn, 0.0);
    p.set(ParamId::VibOn, 1.0);
    p.set(ParamId::VibDepth, 15.0);
    p.set(ParamId::VibDelay, 250.0);
    p.set(ParamId::GlideOn, 0.0);
    p.set(ParamId::UniOn, 0.0);
    p.set(ParamId::VelSens, 30.0);
    p
}

/// Expected sounding frequency of `note` on pulse 1 of a chip (register quantization).
fn chip_freq(chip: ChipMode, n: f64) -> f64 {
    let f = chip::pitch_to_freq(n);
    match chip {
        ChipMode::Nes => chip::nes_pulse_freq(chip::nes_pulse_timer(f, chip::NES_CPU_NTSC), chip::NES_CPU_NTSC),
        ChipMode::Gb => chip::gb_pulse_freq(chip::gb_pulse_reg(f)),
        ChipMode::Free => f,
    }
}

fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

/// C-major melody (8 notes) followed by four chords. Verifies every melody note's pitch.
fn melody_and_chords(chip: ChipMode) -> f64 {
    let p = lead(chip);
    let beat = 0.25;
    let mut evs = Vec::new();
    let mel = [60u8, 62, 64, 65, 67, 69, 71, 72, 71, 67, 64, 60];
    for (i, n) in mel.iter().enumerate() {
        note(&mut evs, i as f64 * beat * 2.0, beat * 1.8, *n, 0.9);
    }
    let t0 = mel.len() as f64 * beat * 2.0 + 0.25;
    let chords: [[u8; 3]; 4] = [[60, 64, 67], [57, 60, 64], [53, 57, 60], [55, 59, 62]];
    for (i, c) in chords.iter().enumerate() {
        for n in c {
            note(&mut evs, t0 + i as f64 * 0.8, 0.7, *n, 0.8);
        }
    }
    let total = t0 + 4.0 * 0.8 + 0.6;
    let (raw, r) = render(&p, &evs, total, 120.0);
    let name = format!("melody_chords_{}.wav", chip.label().to_lowercase());
    let (l, _) = save(&name, &p, raw.clone(), r);
    // Pitch check on the sustained middle of each melody note (before vibrato starts).
    let mut worst: f64 = 0.0;
    for (i, n) in mel.iter().enumerate() {
        let a = ((i as f64 * beat * 2.0 + 0.04) * FS as f64) as usize;
        // Raw engine output: the NES 440 Hz output high-pass would hide low fundamentals.
        let seg = &raw[a..a + 8192];
        let f = estimate_frequency(seg, FS as f64).unwrap_or(0.0);
        let c = cents(f, chip_freq(chip, *n as f64));
        worst = worst.max(c.abs());
    }
    println!("    melody pitch: worst deviation from the chip-quantized frequency {worst:.2} cents");
    assert!(worst < 2.0, "{name}: pitch error {worst} cents");
    // Engine loudness of the single-note melody part (first 6 s, DC removed only), for the
    // cross-mode calibration check; the chip output filters are an effect-chain property.
    let mut dry = raw[..(6.0 * FS as f64) as usize].to_vec();
    hpf(&mut dry, 5.0);
    println!("    after output filter: melody {:.2} dB RMS", db(rms(&l[..(6.0 * FS as f64) as usize])));
    db(rms(&dry))
}

/// Kick / snare / hat patches for the NES drum demo (one engine instance per drum, like
/// three PIXL instances in a DAW).
fn nes_drum(kind: &str) -> ParamValues {
    let mut p = ParamValues::defaults();
    p.set(ParamId::Chip, ChipMode::Nes.index() as f32);
    for id in
        [ParamId::P1On, ParamId::P2On, ParamId::WvOn, ParamId::NsOn, ParamId::VibOn, ParamId::GlideOn, ParamId::UniOn]
    {
        p.set(id, 0.0);
    }
    p.set(ParamId::VelSens, 40.0);
    match kind {
        "kick" => {
            // Triangle swept down two octaves + a short noise click.
            p.set(ParamId::WvOn, 1.0);
            p.set(ParamId::WvAttack, 0.0);
            p.set(ParamId::WvDecay, 140.0);
            p.set(ParamId::WvSustain, 0.0);
            p.set(ParamId::WvRelease, 0.0);
            p.set(ParamId::SweepOn, 1.0);
            p.set(ParamId::SweepAmt, 24.0);
            p.set(ParamId::SweepTime, 70.0);
            p.set(ParamId::SweepCurve, Curve::Exp.index() as f32);
            p.set(ParamId::NsOn, 1.0);
            p.set(ParamId::NsLevel, 11.0);
            p.set(ParamId::NsAttack, 0.0);
            p.set(ParamId::NsDecay, 30.0);
            p.set(ParamId::NsSustain, 0.0);
            p.set(ParamId::NsRelease, 0.0);
            p.set(ParamId::NsPeriod, 9.0);
        }
        "snare" => {
            // Long-mode noise + a pulse burst swept down.
            p.set(ParamId::NsOn, 1.0);
            p.set(ParamId::NsLevel, 14.0);
            p.set(ParamId::NsAttack, 0.0);
            p.set(ParamId::NsDecay, 190.0);
            p.set(ParamId::NsSustain, 0.0);
            p.set(ParamId::NsRelease, 0.0);
            p.set(ParamId::NsPeriod, 5.0);
            p.set(ParamId::P1On, 1.0);
            p.set(ParamId::P1Level, 10.0);
            p.set(ParamId::P1Duty, 50.0);
            p.set(ParamId::P1Attack, 0.0);
            p.set(ParamId::P1Decay, 60.0);
            p.set(ParamId::P1Sustain, 0.0);
            p.set(ParamId::P1Release, 0.0);
            p.set(ParamId::SweepOn, 1.0);
            p.set(ParamId::SweepAmt, 12.0);
            p.set(ParamId::SweepTime, 50.0);
        }
        _ => {
            // Hats: short-mode (metallic) noise, very short decay.
            p.set(ParamId::NsOn, 1.0);
            p.set(ParamId::NsLevel, 9.0);
            p.set(ParamId::NsMode, NoiseMode::Short.index() as f32);
            p.set(ParamId::NsAttack, 0.0);
            p.set(ParamId::NsDecay, 45.0);
            p.set(ParamId::NsSustain, 0.0);
            p.set(ParamId::NsRelease, 0.0);
            p.set(ParamId::NsPeriod, 2.0);
            p.set(ParamId::SweepOn, 0.0);
        }
    }
    p
}

fn drums() {
    let tempo = 120.0;
    let s16 = 60.0 / tempo / 4.0;
    let bars = 4;
    let mut kick = Vec::new();
    let mut snare = Vec::new();
    let mut hat = Vec::new();
    let mut kick_times = Vec::new();
    for bar in 0..bars {
        let b0 = bar as f64 * 16.0 * s16;
        for step in [0usize, 6, 8, 11] {
            let t = b0 + step as f64 * s16;
            note(&mut kick, t, s16, 45, 1.0);
            kick_times.push(t);
        }
        for step in [4usize, 12] {
            note(&mut snare, b0 + step as f64 * s16, s16, 52, 1.0);
        }
        for step in (0..16).step_by(2) {
            note(&mut hat, b0 + step as f64 * s16, s16 * 0.5, 72, if step % 4 == 0 { 0.9 } else { 0.6 });
        }
    }
    let total = bars as f64 * 16.0 * s16 + 0.5;
    let (kl, kr) = render(&nes_drum("kick"), &kick, total, tempo);
    let (sl, sr) = render(&nes_drum("snare"), &snare, total, tempo);
    let (hl, hr) = render(&nes_drum("hat"), &hat, total, tempo);
    let mix = |a: &[f32], b: &[f32], c: &[f32], ga: f32, gb: f32, gc: f32| -> Vec<f32> {
        (0..a.len()).map(|i| a[i] * ga + b[i] * gb + c[i] * gc).collect()
    };
    let l = mix(&kl, &sl, &hl, 0.6, 0.5, 0.45);
    let r = mix(&kr, &sr, &hr, 0.6, 0.5, 0.45);
    let nes = nes_drum("kick");
    let (kick_only, _) = save("drums_nes_kick.wav", &nes, kl.clone(), kr.clone());
    save("drums_nes_snare.wav", &nes, sl, sr);
    save("drums_nes_hat.wav", &nes, hl, hr);
    save("drums_nes.wav", &nes, l, r);
    // Kick timing: energy onset within 1 ms of every scheduled hit, and the sweep falls.
    let mut worst_ms: f64 = 0.0;
    for &t in &kick_times {
        let a = (t * FS as f64) as usize;
        let thresh = 0.05;
        let onset = (a.saturating_sub(48)..a + 960).find(|&i| kick_only[i].abs() > thresh).unwrap_or(a + 960);
        worst_ms = worst_ms.max((onset as f64 - a as f64) / FS as f64 * 1000.0);
    }
    println!("    kick onset: latest {worst_ms:.2} ms after the scheduled time");
    assert!(worst_ms < 2.0, "kick timing {worst_ms} ms");
    let a = (kick_times[0] * FS as f64) as usize;
    let early = estimate_frequency(&kick_only[a..a + 1024], FS as f64).unwrap_or(0.0);
    let late = estimate_frequency(&kick_only[a + 3600..a + 3600 + 1024], FS as f64).unwrap_or(0.0);
    println!("    kick sweep: {early:.0} Hz at the hit → {late:.0} Hz after 75 ms");
    assert!(early > 2.0 * late, "kick should sweep down");
}

/// Chord arps (every chord note arpeggiates its triad) and a HELD arp.
fn arps(chip: ChipMode) {
    let mut p = lead(chip);
    p.set(ParamId::ArpOn, 1.0);
    p.set(ParamId::ArpMode, ArpMode::Up.index() as f32);
    p.set(ParamId::ArpChord, ArpChord::Maj.index() as f32);
    p.set(ParamId::ArpRate, 20.0);
    p.set(ParamId::ArpOct, 2.0);
    p.set(ParamId::P1Sustain, 12.0);
    p.set(ParamId::VibOn, 0.0);
    let mut evs = Vec::new();
    let prog = [(60u8, ArpChord::Maj), (57, ArpChord::Min), (53, ArpChord::Maj), (55, ArpChord::Maj)];
    for (i, (n, _)) in prog.iter().enumerate() {
        note(&mut evs, i as f64 * 1.0, 0.95, *n, 0.9);
    }
    let (l, r) = render(&p, &evs, 4.3, 120.0);
    save(&format!("arp_chord_{}.wav", chip.label().to_lowercase()), &p, l, r);

    // HELD: a chord held on the keyboard plays as one arpeggiated voice, synced 1/16.
    let mut p = lead(chip);
    p.set(ParamId::ArpOn, 1.0);
    p.set(ParamId::ArpChord, ArpChord::Held.index() as f32);
    p.set(ParamId::ArpMode, ArpMode::UpDown.index() as f32);
    p.set(ParamId::ArpSync, 1.0);
    p.set(ParamId::ArpDiv, 3.0);
    p.set(ParamId::ArpOct, 2.0);
    p.set(ParamId::VibOn, 0.0);
    p.set(ParamId::P1Sustain, 12.0);
    let mut evs = Vec::new();
    for (i, c) in [[48u8, 55, 60, 64], [45, 52, 57, 60]].iter().enumerate() {
        for (k, n) in c.iter().enumerate() {
            note(&mut evs, i as f64 * 2.0 + k as f64 * 0.01, 1.9, *n, 0.9);
        }
    }
    let (l, r) = render(&p, &evs, 4.3, 120.0);
    save(&format!("arp_held_{}.wav", chip.label().to_lowercase()), &p, l, r);
}

/// Unison + glide legato lead (FREE and GB) with overlapping notes.
fn unison_glide(chip: ChipMode) {
    let mut p = lead(chip);
    p.set(ParamId::VoiceMode, VoiceMode::Legato.index() as f32);
    p.set(ParamId::UniOn, 1.0);
    p.set(ParamId::UniVoices, 4.0);
    p.set(ParamId::UniDetune, 16.0);
    p.set(ParamId::UniWidth, 80.0);
    p.set(ParamId::GlideOn, 1.0);
    p.set(ParamId::GlideTime, 110.0);
    p.set(ParamId::GlideMode, GlideMode::Legato.index() as f32);
    p.set(ParamId::P1Duty, if chip == ChipMode::Free { 35.0 } else { 50.0 });
    p.set(ParamId::P2On, 1.0);
    p.set(ParamId::P2Coarse, -12.0);
    p.set(ParamId::P2Level, 8.0);
    p.set(ParamId::P2Duty, 12.5);
    p.set(ParamId::VibDelay, 300.0);
    p.set(ParamId::VibDepth, 25.0);
    let mut evs = Vec::new();
    let line = [
        (64u8, 0.0, 0.6),
        (67, 0.5, 0.6),
        (72, 1.0, 1.0),
        (71, 1.9, 0.35),
        (67, 2.2, 0.35),
        (69, 2.5, 1.2),
        (60, 3.9, 1.0),
    ];
    for (n, t, d) in line {
        note(&mut evs, t, d, n, 0.9);
    }
    let (l, r) = render(&p, &evs, 5.4, 120.0);
    let name = format!("unison_glide_{}.wav", chip.label().to_lowercase());
    let (l, rr) = save(&name, &p, l, r);
    // Stereo width from unison spread: the channels differ.
    let diff: Vec<f32> = l.iter().zip(rr.iter()).map(|(a, b)| a - b).collect();
    println!("    stereo side/mid {:.1} dB", db(rms(&diff) / rms(&l).max(1e-9)));
}

/// The default patch (factory 014 "Hyrule Fanfare"): a short fanfare.
fn default_patch() {
    let p = ParamValues::defaults();
    let mut evs = Vec::new();
    for (i, (n, d)) in [(67u8, 0.2), (67, 0.2), (67, 0.2), (72, 0.9), (70, 0.3), (72, 1.2)].iter().enumerate() {
        let t = [0.0, 0.25, 0.5, 0.75, 1.7, 2.05][i];
        note(&mut evs, t, *d, *n, 0.9);
    }
    let (l, r) = render(&p, &evs, 4.0, 120.0);
    save("default_patch_fanfare.wav", &p, l, r);
}

fn main() {
    println!("Rendering to {}", out_dir().display());
    let levels: Vec<f64> =
        [ChipMode::Nes, ChipMode::Gb, ChipMode::Free].iter().map(|c| melody_and_chords(*c)).collect();
    let spread = levels.iter().cloned().fold(f64::MIN, f64::max) - levels.iter().cloned().fold(f64::MAX, f64::min);
    println!(
        "    engine melody loudness NES {:.2} / GB {:.2} / FREE {:.2} dB RMS → spread {spread:.2} dB",
        levels[0], levels[1], levels[2]
    );
    assert!(spread < 1.5, "chip modes must have consistent loudness");
    drums();
    for chip in [ChipMode::Nes, ChipMode::Gb, ChipMode::Free] {
        arps(chip);
    }
    unison_glide(ChipMode::Free);
    unison_glide(ChipMode::Gb);
    default_patch();
    println!("done");
}
