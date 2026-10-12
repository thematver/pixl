//! Musical parameter diagrams with explicit boundaries between preview and live data.
use crate::{
    editor::PLOT,
    paint::{center, path, text},
    EditorApp,
};
use pixl_core::*;
use pixl_gfx::{kurbo::Rect, rgb, rgba, Canvas};

const INK: u32 = 0x161616;
const MUT: u32 = 0x6E6A63;
const A: u32 = 0xFF5A1F;
const GRID: u32 = 0xEFEDE8;

fn note_name(n: i16) -> String {
    let pitch = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"][n.rem_euclid(12) as usize];
    format!("{pitch}{}", n.div_euclid(12) - 1)
}

pub(crate) fn arp(app: &EditorApp, c: &mut Canvas) {
    let get = |id| app.bridge.param_modulated(id);
    let telemetry = app.bridge.telemetry();
    let chord = ArpChord::from_plain(get(ParamId::ArpChord));
    let mode = ArpMode::from_plain(get(ParamId::ArpMode));
    let mut notes: Vec<i16> = if chord == ArpChord::Held {
        (0..128u8).filter(|n| telemetry.is_held(*n)).map(i16::from).collect()
    } else {
        let root = i16::from(telemetry.last_note.unwrap_or(60));
        chord.intervals().iter().map(|n| root + i16::from(*n)).collect()
    };
    let example = notes.is_empty();
    if example {
        notes = vec![60, 64, 67];
    }
    let octaves = get(ParamId::ArpOct) as usize;
    let expanded: Vec<_> = (0..octaves).flat_map(|o| notes.iter().map(move |n| n + o as i16 * 12)).collect();
    let count = expanded.len().max(1);
    let index = |step: usize| match mode {
        ArpMode::Down => count - 1 - step % count,
        ArpMode::UpDown => {
            let p = step % (count.saturating_sub(1) * 2).max(1);
            if p < count {
                p
            } else {
                2 * count - 2 - p
            }
        }
        // Neither random seed nor played-note order is published: label these diagrams as examples.
        ArpMode::Random => {
            let mut h = (step as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
            h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((h ^ (h >> 31)) % count as u64) as usize
        }
        _ => step % count,
    };
    let steps = (count * 2).clamp(8, 16);
    let sequence: Vec<_> = (0..steps).map(|k| expanded[index(k)]).collect();
    let mut lanes = sequence.clone();
    lanes.sort();
    lanes.dedup();
    let lane_h = PLOT.height() / lanes.len().max(1) as f64;
    let cell_w = PLOT.width() / steps as f64;
    let enabled = get(ParamId::ArpOn) > 0.;
    let live = enabled && telemetry.active_voices > 0 && !example;
    let unknown_order = mode == ArpMode::Order && chord == ArpChord::Held;
    let schematic = mode == ArpMode::Random || unknown_order;
    let current_note = if live && !unknown_order && count < 256 {
        telemetry.arp_step.and_then(|k| expanded.get(k as usize).copied())
    } else {
        None
    };
    for (i, note) in lanes.iter().enumerate() {
        let y = PLOT.y1 - (i as f64 + 0.5) * lane_h;
        if note.rem_euclid(12) == 0 {
            c.fill_rect(Rect::new(PLOT.x0, y - lane_h * 0.5, PLOT.x1, y + lane_h * 0.5), rgba(INK, 0.025));
        }
        if current_note == Some(*note) {
            c.fill_rect(Rect::new(PLOT.x0, y - lane_h * 0.5, PLOT.x1, y + lane_h * 0.5), rgba(A, 0.06));
        }
        c.line((PLOT.x0, y + lane_h * 0.5), (PLOT.x1, y + lane_h * 0.5), 1., rgb(GRID));
        if i % lanes.len().div_ceil(8).max(1) == 0 {
            text(c, Rect::new(26., y - 7., 47., y + 7.), &note_name(*note), 9., MUT, true);
        }
    }
    for (k, note) in sequence.iter().enumerate() {
        let x = PLOT.x0 + k as f64 * cell_w;
        let lane = lanes.binary_search(note).unwrap_or(0);
        let y = PLOT.y1 - (lane as f64 + 0.5) * lane_h;
        c.line((x, PLOT.y0), (x, PLOT.y1), 1., rgb(GRID));
        let bar =
            Rect::new(x + 5., y - (lane_h * 0.22).clamp(2., 7.), x + cell_w - 5., y + (lane_h * 0.22).clamp(2., 7.));
        c.fill_rounded_rect(
            bar,
            2.,
            if !enabled {
                rgba(INK, 0.18)
            } else if current_note == Some(*note) {
                rgb(A)
            } else {
                rgba(A, 0.30)
            },
        );
        if current_note == Some(*note) {
            c.stroke_rounded_rect(bar.inflate(2., 2.), 3., 1., rgb(A));
        }
        if k % 2 == 0 {
            center(c, Rect::new(x, 329., x + cell_w, 347.), &format!("{:02}", k + 1), 9., MUT);
        }
    }
    let status = if !enabled {
        app.text("BYPASS · ПРЕДПРОСМОТР", "BYPASSED · PREVIEW")
    } else if example {
        app.text("ПРИМЕР C MAJ · СЫГРАЙТЕ АККОРД", "C MAJ EXAMPLE · PLAY A CHORD")
    } else if schematic {
        app.text("ПРИМЕР ПОСЛЕДОВАТЕЛЬНОСТИ · LIVE-НОТА", "SEQUENCE EXAMPLE · LIVE NOTE")
    } else if live {
        "LIVE NOTE · MIDI"
    } else {
        "PREVIEW"
    };
    text(c, Rect::new(34., 140., 600., 155.), status, 9., MUT, true);
    let timing = if get(ParamId::ArpSync) > 0. {
        format!("{} · {:.1} BPM", ParamId::ArpDiv.format(get(ParamId::ArpDiv)), telemetry.tempo_bpm)
    } else {
        let rate = get(ParamId::ArpRate) as f64;
        let chip = ChipMode::from_plain(get(ParamId::Chip));
        let hz = if chip == ChipMode::Free {
            rate
        } else {
            let frame =
                if chip == ChipMode::Gb { 59.7275 } else { FrameRate::from_plain(get(ParamId::FrameRate)).hz() };
            frame / (frame / rate.max(0.001)).round().max(1.)
        };
        format!("{hz:.2} HZ · ACTUAL")
    };
    text(c, Rect::new(710., 140., 874., 155.), &timing, 9., MUT, true);
}

pub(crate) fn echo(app: &EditorApp, c: &mut Canvas) {
    let get = |id| app.bridge.param_modulated(id) as f64;
    let t = app.bridge.telemetry();
    let sync = get(ParamId::EchoSync) > 0.;
    let time_ms = if sync {
        EchoDiv::from_plain(get(ParamId::EchoDiv) as f32).beats() * 60_000. / t.tempo_bpm.max(1.) as f64
    } else {
        get(ParamId::EchoTime)
    }
    .clamp(10., 2000.);
    let feedback = get(ParamId::EchoFb) / 100.;
    let mix = get(ParamId::EchoMix) / 100.;
    let width = get(ParamId::EchoWidth) / 100.;
    let enabled = get(ParamId::EchoOn) > 0.;
    // A left input impulse: width affects channel routing, never the repeat's time.
    let mut amplitude = [1. - width * 0.5, 0.];
    let mut envelope = [Vec::new(), Vec::new()];
    for (channel, baseline) in [224., 308.].into_iter().enumerate() {
        c.line((PLOT.x0, baseline), (PLOT.x1, baseline), 1., rgb(0xDDD9D1));
        text(
            c,
            Rect::new(27., baseline - 22., 45., baseline - 2.),
            if channel == 0 { "L" } else { "R" },
            11.,
            INK,
            true,
        );
    }
    for k in 1..=8 {
        let x = PLOT.x0 + k as f64 * PLOT.width() / 8.;
        c.line((x, PLOT.y0), (x, PLOT.y1), 1., rgb(GRID));
        for channel in 0..2 {
            let baseline = [224., 308.][channel];
            let level = amplitude[channel] * mix;
            let db = if level > 0. { 20. * level.log10() } else { -60. };
            let y = baseline - ((db + 60.) / 60.).clamp(0., 1.) * 62.;
            c.line(
                (x, baseline),
                (x, y),
                3.,
                if enabled { rgb(if channel == 0 { A } else { INK }) } else { rgba(INK, 0.20) },
            );
            c.fill_circle((x, y), 2.5, if enabled { rgb(if channel == 0 { A } else { INK }) } else { rgb(0xC9C5BC) });
            envelope[channel].push((x, y));
        }
        amplitude = [
            feedback * (amplitude[0] * (1. - width) + amplitude[1] * width),
            feedback * (amplitude[1] * (1. - width) + amplitude[0] * width),
        ];
    }
    for points in envelope {
        path(c, &points, 0xC9C5BC, true);
    }
    for k in [0, 1, 2, 4, 8] {
        let x = PLOT.x0 + k as f64 * PLOT.width() / 8.;
        let value = k as f64 * time_ms;
        let label = if value >= 1000. { format!("{:.2} S", value / 1000.) } else { format!("{value:.0} MS") };
        center(c, Rect::new(x - 34., 329., x + 34., 347.), &label, 9., MUT);
    }
    text(
        c,
        Rect::new(34., 140., 700., 155.),
        if enabled { "L IMPULSE · RELATIVE DB · FIR NOT SHOWN" } else { "BYPASSED · RELATIVE DB · FIR NOT SHOWN" },
        9.,
        MUT,
        true,
    );
    text(
        c,
        Rect::new(742., 140., 874., 155.),
        &if sync { format!("{:.1} BPM", t.tempo_bpm) } else { "FREE TIME".into() },
        9.,
        MUT,
        true,
    );
}
