//! PIXL factory-library audition.
//!
//! Renders every factory preset through the production [`pixl::processor::Processor`]
//! (the real engine and effect chain, exactly as the plugin runs them) at 48 kHz with
//! category-appropriate material, measures the result and checks it against the library's
//! sound targets:
//!
//! - every preset is audible and finite, a single note at full velocity peaks at or below
//!   −1 dBFS (the safety clipper's threshold), chords stay under the ceiling;
//! - loudness is balanced within each category: EBU R128 integrated loudness of the phrase
//!   for the sustained categories (lead, bass, arp, pad), the maximum momentary loudness
//!   (400 ms) for the one-shot categories (drum, SFX), whose integrated loudness mostly
//!   measures how long the hits ring;
//! - pitched presets play in tune (octave-folded, after modulation settles, measured with
//!   unison collapsed to one layer: unison detune is symmetric by construction, and the check
//!   is about coarse/fine/tune settings and chip pitch quantization);
//! - the tail ends (the processor reports silence);
//! - each of the four macros audibly changes the sound between 0 % and 100 %.
//!
//! Findings on the init preset 014 that follow from its SPEC-fixed state (parameter defaults
//! and factory-default macros: its peaks and its SPACE macro) are printed as `~~ known` and do
//! not fail the run; everything else exits with status 1.
//!
//! ```text
//! audition [--only 1,5-9] [--details] [--calibrate] [--wav DIR] [--library PATH]
//! ```
//!
//! `--calibrate` writes `tools/presets/levels.json` with the output gain that puts each
//! preset on its category's loudness target (bounded by the peak limits); `generate.py`
//! reads it. All level measurements are taken at a fixed low output gain and referred to
//! the stored gain (the chain is linear before the clipper), so the clipper never biases them.

mod host;
mod measure;
mod phrase;

use host::{render, render_traced, secs, values_with, Render};
use measure::*;
use phrase::{material, single, Material, PHRASE_VELOCITY};
use pixl::presets::{FactoryLibrary, FactoryPreset};
use pixl::processor::BlockStatus;
use pixl_core::macros::MACRO_NAMES;
use pixl_core::preset::{Category, PresetIcon};
use pixl_core::{apply_macros, Channel, ChannelField, ParamId, ParamValues, MACRO_COUNT};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Output gain used for every measurement render (dB); results are referred to the stored gain.
const REFERENCE_GAIN_DB: f64 = -24.0;
/// A single note at full velocity must peak at or below this (dBFS).
const SINGLE_PEAK_LIMIT_DB: f64 = -1.0;
/// Calibration headroom kept below the limits (dB).
const CALIBRATION_MARGIN_DB: f64 = 0.5;
/// Phrase (chords, overlaps) peak the calibration aims to stay under (dBFS).
const PHRASE_PEAK_LIMIT_DB: f64 = -1.0;
/// Audibility floor (dBFS).
const AUDIBLE_DB: f64 = -30.0;
/// Pitch tolerance for pitched presets (cents).
const PITCH_TOLERANCE_C: f64 = 15.0;
/// Largest allowed loudness spread inside a category (LU).
const SPREAD_LIMIT: f64 = 6.0;
/// Tail render after the phrase (s).
const TAIL_S: f64 = 20.0;
/// Loudness of each category relative to the init preset 014 (LU), whose output gain is
/// fixed at the parameter default. Sustained categories compare integrated loudness with
/// 014's, one-shot categories (drum, SFX) their maximum momentary loudness with 014's.
const CATEGORY_OFFSETS: [(Category, f64); 6] = [
    (Category::Lead, 0.0),
    (Category::Bass, 0.0),
    (Category::Arp, -1.0),
    (Category::Pad, -2.0),
    // Drums sit 2 LU under the lead's loudest moment: the shortest hits (a closed hat, a rim
    // shot) reach only about −19 LUFS momentary at the single-note peak limit, and the category
    // must stay within the spread limit.
    (Category::Drum, -2.0),
    (Category::Sfx, -1.0),
];

/// What moving one macro from 0 % to 100 % does (others at their stored positions).
#[derive(Clone, Copy, Debug, Default)]
struct MacroEffect {
    d_level_db: f64,
    d_centroid_pct: f64,
    d_width: f64,
    d_active_ms: f64,
    d_pitch_c: f64,
    d_vibrato_c: f64,
    /// 95th percentile of the pitch-modulation difference over the phrase (cents, from the
    /// engine's telemetry: sweep, vibrato, glide and arp movement).
    d_pitch_mod_c: f64,
    distance_db: f64,
    distance_p95_db: f64,
}

impl MacroEffect {
    /// Below every threshold: the macro does not audibly change the sound.
    fn negligible(&self) -> bool {
        self.d_level_db.abs() < 1.0
            && self.d_centroid_pct.abs() < 5.0
            && self.d_width.abs() < 0.05
            && self.d_active_ms.abs() < 30.0
            && self.d_pitch_c.abs() < 5.0
            && self.d_vibrato_c.abs() < 3.0
            && self.d_pitch_mod_c < 10.0
            && self.distance_db < 1.0
            && self.distance_p95_db < 3.0
    }

    /// The most prominent change, for the table ("lvl+3.1", "cen-24%", ...).
    fn headline(&self) -> String {
        let cands = [
            (self.d_level_db.abs() / 1.0, format!("lvl{:+.1}", self.d_level_db)),
            (self.d_centroid_pct.abs() / 5.0, format!("cen{:+.0}%", self.d_centroid_pct)),
            (self.d_width.abs() / 0.05, format!("wid{:+.2}", self.d_width)),
            (self.d_active_ms.abs() / 30.0, format!("dur{:+.0}", self.d_active_ms)),
            (self.d_pitch_c.abs() / 5.0, format!("pit{:+.0}c", self.d_pitch_c)),
            (self.d_vibrato_c.abs() / 3.0, format!("vib{:+.0}c", self.d_vibrato_c)),
            (self.d_pitch_mod_c / 10.0, format!("mod{:.0}c", self.d_pitch_mod_c)),
            (self.distance_db / 1.0, format!("dst{:.1}", self.distance_db)),
            (self.distance_p95_db / 3.0, format!("d95{:.1}", self.distance_p95_db)),
        ];
        let best = cands.iter().max_by(|a, b| a.0.total_cmp(&b.0)).map(|c| c.1.clone()).unwrap_or_default();
        if self.negligible() {
            format!("!{best}")
        } else {
            best
        }
    }
}

/// Everything measured for one preset. Levels are referred to the stored output gain.
#[derive(Clone, Debug, Default)]
struct Audit {
    number: u32,
    name: String,
    category: Category,
    chip: String,
    icon: String,
    stored_gain: f64,
    single_peak_db: f64,
    single_peak_note: u8,
    phrase_peak_db: f64,
    phrase_rms_db: f64,
    /// Integrated loudness of the phrase (LUFS).
    lufs: f64,
    /// Maximum momentary loudness of the phrase (LUFS).
    lufs_m: f64,
    width: f64,
    centroid_hz: f64,
    /// Actual render at the stored gain: C4, velocity 100, 1 s (the plugin's factory test).
    c4_peak_db: f64,
    pitched: bool,
    expected_note: f64,
    pitch: Option<(f64, f64)>,
    timing: Timing,
    tail_ms: Option<f64>,
    tail_ends: bool,
    final_status: Option<BlockStatus>,
    /// After the last note-off: no voice counted by the telemetry / engine reports silence /
    /// processor reports silence (ms).
    voice_count_off_ms: f64,
    voices_off_ms: f64,
    silent_ms: f64,
    pitch_frames: usize,
    finite: bool,
    recoveries: u64,
    macros: [MacroEffect; MACRO_COUNT],
    suggested_gain: f64,
    problems: Vec<String>,
    /// Findings the library cannot fix (see [`INIT_STATE_NOTE`]): printed, never fatal.
    known: Vec<String>,
}

/// Number of the init preset, 014 "Hyrule Fanfare": the parameter defaults, the default wave
/// and `MacroAssignments::factory_default()` (SPEC §2, §9), none of which the library owns.
const INIT_NUMBER: u32 = 14;
/// Why a finding on the init preset is reported as known instead of failing the audition.
const INIT_STATE_NOTE: &str = "init state = parameter defaults + factory_default macros (SPEC §2): \
                               not adjustable in the library, an engine/product decision";

/// Whether a problem of the init preset follows from the SPEC-fixed init state: its output
/// gain is the parameter default (so its peaks are the engine's), and its macros are the
/// factory defaults (SPACE drives `echo_mix` with the echo off and a unison width that GB hard
/// panning already saturates).
fn init_state_finding(problem: &str) -> bool {
    problem.starts_with("single note ") || problem.starts_with("macro 4 SPACE has a negligible effect")
}

impl Audit {
    /// The loudness measure the category is balanced on (LUFS).
    fn level(&self) -> f64 {
        level_of(self.category, self.lufs, self.lufs_m)
    }
}

/// Integrated loudness for sustained categories, maximum momentary loudness for one-shots.
fn level_of(category: Category, integrated: f64, momentary: f64) -> f64 {
    match category {
        Category::Drum | Category::Sfx => momentary,
        _ => integrated,
    }
}

/// Name of [`level_of`]'s measure for a category.
fn level_name(category: Category) -> &'static str {
    match category {
        Category::Drum | Category::Sfx => "momentary max",
        _ => "integrated",
    }
}

fn macro_positions(values: &ParamValues) -> [f32; MACRO_COUNT] {
    std::array::from_fn(|m| values.get(ParamId::macro_value(m)) / 100.0)
}

fn effective(f: &FactoryPreset) -> ParamValues {
    let mut out = ParamValues::defaults();
    apply_macros(&f.state.values, macro_positions(&f.state.values), &f.state.macros, &mut out);
    out
}

/// The note offset (semitones) the preset is heard at: the coarse tuning of the channel its
/// icon shows, plus the level-weighted mean fine tuning of the tonal channels sounding in
/// octaves with it (symmetrically detuned layers are heard at their centre), plus master tune.
fn main_channel_offset(f: &FactoryPreset, v: &ParamValues) -> f64 {
    let on = |c: Channel| v.bool(ParamId::channel(c, ChannelField::On));
    let level = |c: Channel| {
        if !on(c) {
            0.0
        } else if c == Channel::Wave && v.chip() == pixl_core::ChipMode::Nes {
            15.0 // the NES triangle has no volume control
        } else {
            v.channel(c, ChannelField::Level) as f64
        }
    };
    let main = match f.preset.icon {
        PresetIcon::P12 | PresetIcon::P50 => {
            if level(Channel::P2) > level(Channel::P1) {
                Channel::P2
            } else {
                Channel::P1
            }
        }
        PresetIcon::Tri | PresetIcon::Wav => Channel::Wave,
        PresetIcon::Noi => Channel::Noise,
    };
    let coarse = |c: Channel| v.channel(c, ChannelField::Coarse) as i32;
    let fine = |c: Channel| v.channel(c, ChannelField::Fine) as f64;
    let (mut sum, mut weight) = (0.0, 0.0);
    for c in [Channel::P1, Channel::P2, Channel::Wave] {
        if level(c) > 0.0 && (coarse(c) - coarse(main)).rem_euclid(12) == 0 {
            sum += level(c) * fine(c);
            weight += level(c);
        }
    }
    let mean_fine = if weight > 0.0 { sum / weight } else { fine(main) };
    coarse(main) as f64 + mean_fine / 100.0 + v.get(ParamId::Tune) as f64 / 100.0
}

fn render_phrase(
    f: &FactoryPreset,
    mat: &Material,
    gain: f64,
    macro_override: Option<(usize, f32)>,
    tail: f64,
) -> Render {
    let values = values_with(&f.state, gain as f32, macro_override);
    render(&f.state, &values, &mat.phrase, mat.phrase_end + secs(tail))
}

/// Pitch of a probe-note render: `(median cents vs expected, SD of the track)` and the
/// number of frames it is based on. The window is the end of the held note, at least
/// `window` long (one and a half vibrato cycles for slow vibratos), starting no earlier than
/// `settle`; if the note has decayed by then, the whole settled part of the note is used.
fn probe_pitch(r: &Render, hold: usize, expected: f64, settle: usize, window: usize) -> (Option<(f64, f64)>, usize) {
    let x = mono(&r.left, &r.right);
    let (env, _) = envelope(&x[..hold.min(x.len())]);
    let max = env.iter().cloned().fold(0.0f64, f64::max);
    if max <= 1e-7 {
        return (None, 0);
    }
    let hz = note_hz(expected);
    let late_start = hold.saturating_sub(window).max(settle.min(hold / 2));
    let mut track = pitch_track(&x, late_start, hold, hz, 0.03 * max);
    if track.len() < 5 {
        track = pitch_track(&x, settle.min(hold / 2), hold, hz, 0.1 * max);
    }
    if track.len() < 5 {
        return (None, track.len());
    }
    let sd = mean_sd(&track).map_or(0.0, |m| m.1);
    (median(&track).map(|m| (m, sd)), track.len())
}

/// Pitch window for `v`: 0.6 s, or 1.5 vibrato cycles when the vibrato is slower.
fn pitch_window(v: &ParamValues) -> usize {
    let mut w = 0.6f64;
    if v.bool(ParamId::VibOn) && v.get(ParamId::VibDepth) > 0.0 {
        w = w.max(1.5 / (v.get(ParamId::VibRate) as f64).max(0.1));
    }
    secs(w)
}

/// Samples after note-on until pitch modulation (sweep, glide, vibrato fade-in) settles.
fn settle_samples(v: &ParamValues) -> usize {
    let mut t = 0.03f64;
    if v.bool(ParamId::SweepOn) && v.get(ParamId::SweepAmt) > 0.0 {
        t = t.max(v.get(ParamId::SweepTime) as f64 / 1000.0 + 0.03);
    }
    secs(t)
}

fn audit(f: &FactoryPreset) -> Audit {
    let p = &f.preset;
    let mat = material(p.category);
    let eff = effective(f);
    let stored_gain = f.state.values.get(ParamId::OutGain) as f64;
    let refer = stored_gain - REFERENCE_GAIN_DB; // dB to add to reference-gain levels
    let mut a = Audit {
        number: p.number,
        name: p.name.clone(),
        category: p.category,
        chip: eff.chip().label().to_string(),
        icon: format!("{:?}", p.icon).to_lowercase(),
        stored_gain,
        finite: true,
        ..Default::default()
    };

    // --- phrase -----------------------------------------------------------------------
    let full = render_phrase(f, &mat, REFERENCE_GAIN_DB, None, TAIL_S);
    a.finite &= all_finite(&full.left, &full.right);
    a.recoveries += full.recoveries;
    let play_end = (mat.phrase_end + secs(0.5)).min(full.left.len());
    let (pl, pr) = (&full.left[..play_end], &full.right[..play_end]);
    a.phrase_peak_db = db(peak(pl, pr)) + refer;
    a.phrase_rms_db = db(rms(pl, pr)) + refer;
    let tail_view = mat.phrase_end + secs(4.0);
    let (ll, lr) = (&full.left[..tail_view.min(full.left.len())], &full.right[..tail_view.min(full.right.len())]);
    a.lufs = loudness_lufs(ll, lr) + refer;
    a.lufs_m = momentary_max_lufs(ll, lr) + refer;
    a.width = width(pl, pr);
    a.centroid_hz = centroid(&mono(pl, pr));
    a.tail_ms = settle_time(&full.left, &full.right, mat.phrase_end, -60.0 - refer);
    a.tail_ends = full.final_status == BlockStatus::Silent;
    a.final_status = Some(full.final_status);
    let ms = |n: usize| n.saturating_sub(mat.phrase_end) as f64 * 1000.0 / SR as f64;
    a.voice_count_off_ms = ms(full.voice_count_end);
    a.voices_off_ms = ms(full.voices_end);
    a.silent_ms = ms(full.sound_end);

    // --- worst-case single-note peak (full velocity) ---------------------------------------
    a.single_peak_db = f64::NEG_INFINITY;
    for &n in mat.peak_notes {
        let (ev, end) = single(n, 1.0, 1.0);
        let r = render(&f.state, &values_with(&f.state, REFERENCE_GAIN_DB as f32, None), &ev, end + secs(2.0));
        a.finite &= all_finite(&r.left, &r.right);
        a.recoveries += r.recoveries;
        let pk = db(peak(&r.left, &r.right)) + refer;
        if pk > a.single_peak_db {
            a.single_peak_db = pk;
            a.single_peak_note = n;
        }
    }

    // --- the plugin's factory test, at the stored gain ------------------------------------
    {
        let (ev, end) = single(60, 100.0 / 127.0, 1.0);
        let r = render(&f.state, &f.state.values, &ev, end + secs(0.5));
        a.finite &= all_finite(&r.left, &r.right);
        a.c4_peak_db = db(peak(&r.left, &r.right));
    }

    // --- probe note: pitch and envelope timing ---------------------------------------------
    let hold = secs(mat.probe_hold);
    let arp = eff.bool(ParamId::ArpOn);
    a.pitched = p.category != Category::Drum && p.icon != PresetIcon::Noi && !arp;
    a.expected_note = mat.probe_note as f64 + main_channel_offset(f, &eff);
    let probe = |macro_override: Option<(usize, f32)>| {
        let (ev, end) = single(mat.probe_note, PHRASE_VELOCITY, mat.probe_hold);
        render(&f.state, &values_with(&f.state, REFERENCE_GAIN_DB as f32, macro_override), &ev, end + secs(3.0))
    };
    let pr0 = probe(None);
    a.finite &= all_finite(&pr0.left, &pr0.right);
    let timing_hz = if a.pitched { note_hz(a.expected_note) } else { 0.0 };
    a.timing = timing(&mono(&pr0.left, &pr0.right), hold, timing_window(timing_hz));
    let window = pitch_window(&eff).min(hold);
    if a.pitched {
        let mut v = values_with(&f.state, REFERENCE_GAIN_DB as f32, None);
        v.set(ParamId::UniOn, 0.0);
        let (ev, end) = single(mat.probe_note, PHRASE_VELOCITY, mat.probe_hold);
        let tuning = render(&f.state, &v, &ev, end + secs(0.5));
        (a.pitch, a.pitch_frames) = probe_pitch(&tuning, hold, a.expected_note, settle_samples(&eff), window);
    }

    // --- macros ------------------------------------------------------------------------------
    for m in 0..MACRO_COUNT {
        let lo = render_phrase(f, &mat, REFERENCE_GAIN_DB, Some((m, 0.0)), 3.0);
        let hi = render_phrase(f, &mat, REFERENCE_GAIN_DB, Some((m, 100.0)), 3.0);
        a.finite &= all_finite(&lo.left, &lo.right) && all_finite(&hi.left, &hi.right);
        a.recoveries += lo.recoveries + hi.recoveries;
        let (ml, mh) = (mono(&lo.left, &lo.right), mono(&hi.left, &hi.right));
        let e = &mut a.macros[m];
        e.d_level_db = db(rms(&hi.left, &hi.right)) - db(rms(&lo.left, &lo.right));
        let (c0, c1) = (centroid(&ml), centroid(&mh));
        e.d_centroid_pct = if c0 > 0.0 { 100.0 * (c1 - c0) / c0 } else { 0.0 };
        e.d_width = width(&hi.left, &hi.right).min(4.0) - width(&lo.left, &lo.right).min(4.0);
        let thr = db(peak(&lo.left, &lo.right).max(peak(&hi.left, &hi.right))) - 50.0;
        e.d_active_ms = active_ms(&hi.left, &hi.right, thr) - active_ms(&lo.left, &lo.right, thr);
        (e.distance_db, e.distance_p95_db) = band_distance(&ml, &mh);
        e.d_pitch_mod_c = pitch_mod_difference(&lo.pitch_mod, &lo.voices, &hi.pitch_mod, &hi.voices);
        if a.pitched {
            let (p0, p1) = (probe(Some((m, 0.0))), probe(Some((m, 100.0))));
            let settle = settle_samples(&eff);
            if let ((Some(x0), _), (Some(x1), _)) = (
                probe_pitch(&p0, hold, a.expected_note, settle, window),
                probe_pitch(&p1, hold, a.expected_note, settle, window),
            ) {
                e.d_pitch_c = x1.0 - x0.0;
                e.d_vibrato_c = x1.1 - x0.1;
            }
        }
    }
    a
}

/// Category loudness targets (LUFS, in each category's [`level_of`] measure), anchored on
/// 014's loudness in the same measure.
fn targets(audits: &[Audit]) -> Option<Vec<(Category, f64)>> {
    let anchor = audits.iter().find(|a| a.number == INIT_NUMBER)?;
    Some(CATEGORY_OFFSETS.iter().map(|(c, o)| (*c, level_of(*c, anchor.lufs, anchor.lufs_m) + o)).collect())
}

fn suggest_gain(a: &Audit, target: f64) -> f64 {
    let loud = a.stored_gain + (target - a.level());
    let single = a.stored_gain + (SINGLE_PEAK_LIMIT_DB - CALIBRATION_MARGIN_DB - a.single_peak_db);
    let phrase = a.stored_gain + (PHRASE_PEAK_LIMIT_DB - a.phrase_peak_db);
    let g = loud.min(single).min(phrase).clamp(-36.0, 12.0);
    (g * 10.0).round() / 10.0
}

fn check(a: &mut Audit) {
    let mut p = Vec::new();
    if !a.finite {
        p.push("non-finite output".to_string());
    }
    if a.recoveries > 0 {
        p.push(format!("output guard stepped in {} time(s)", a.recoveries));
    }
    if a.c4_peak_db <= AUDIBLE_DB || a.phrase_peak_db <= AUDIBLE_DB {
        p.push(format!("inaudible (C4 {:.1} dBFS, phrase {:.1} dBFS)", a.c4_peak_db, a.phrase_peak_db));
    }
    if a.single_peak_db > SINGLE_PEAK_LIMIT_DB + 0.05 {
        p.push(format!(
            "single note {} peaks at {:+.1} dBFS (> {SINGLE_PEAK_LIMIT_DB} dBFS)",
            a.single_peak_note, a.single_peak_db
        ));
    }
    if a.phrase_peak_db > 0.0 {
        p.push(format!("phrase drives the clipper ({:+.1} dBFS before clipping)", a.phrase_peak_db));
    }
    if a.pitched && matches!(a.category, Category::Lead | Category::Bass | Category::Pad) {
        match a.pitch {
            Some((c, _)) if c.abs() >= PITCH_TOLERANCE_C => p.push(format!("out of tune by {c:+.1} c")),
            None => p.push("pitch not measurable".to_string()),
            _ => {}
        }
    }
    let tail_end = (TAIL_S * 1000.0) - 100.0;
    if a.voice_count_off_ms >= tail_end {
        p.push("voices never end".to_string());
    } else if !a.tail_ends || a.tail_ms.is_none() {
        p.push(format!(
            "tail does not end: voices end +{:.0} ms but the {} (DC or residual output)",
            a.voice_count_off_ms,
            if a.voices_off_ms >= tail_end { "engine never reports silence" } else { "effect tail never ends" }
        ));
    }
    for (m, e) in a.macros.iter().enumerate() {
        if e.negligible() {
            p.push(format!("macro {} {} has a negligible effect ({})", m + 1, MACRO_NAMES[m], e.headline()));
        }
    }
    if a.number == INIT_NUMBER {
        let (known, rest): (Vec<String>, Vec<String>) = p.into_iter().partition(|s| init_state_finding(s));
        a.known = known;
        a.problems = rest;
    } else {
        a.problems = p;
    }
}

fn parse_only(spec: &str) -> Result<BTreeSet<u32>, String> {
    let mut out = BTreeSet::new();
    for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b): (u32, u32) =
                (a.parse().map_err(|_| part.to_string())?, b.parse().map_err(|_| part.to_string())?);
            out.extend(a..=b);
        } else {
            out.insert(part.parse().map_err(|_| part.to_string())?);
        }
    }
    Ok(out)
}

fn fmt_opt_ms(v: Option<f64>) -> String {
    match v {
        None => "sus".to_string(),
        Some(ms) if ms >= 1000.0 => format!("{:.1}s", ms / 1000.0),
        Some(ms) => format!("{ms:.0}"),
    }
}

fn print_tables(audits: &[Audit], details: bool) -> bool {
    let mut ok = true;
    let tgt = targets(audits);
    for (cat, _) in CATEGORY_OFFSETS {
        let rows: Vec<&Audit> = audits.iter().filter(|a| a.category == cat).collect();
        if rows.is_empty() {
            continue;
        }
        println!();
        println!(
            "{:<6} {:<17} {:<4} {:<3} {:>5} {:>5} {:>5} {:>6} {:>6} {:>5} {:>6} {:>11} {:>4} {:>5} {:>5}  {:<9} {:<9} {:<9} {:<9}",
            cat.tag(),
            "name",
            "chip",
            "ico",
            "gain",
            "pk1",
            "pkP",
            "rms",
            if matches!(cat, Category::Drum | Category::Sfx) { "lufsM" } else { "lufs" },
            "wid",
            "cent",
            "pitch c",
            "att",
            "dec",
            "tail",
            "BRIGHT",
            "DIRT",
            "MOTION",
            "SPACE"
        );
        for a in &rows {
            let pitch = match (a.pitched, a.pitch) {
                (false, _) => "-".to_string(),
                (true, None) => "?".to_string(),
                (true, Some((c, sd))) => format!("{c:+.1}±{sd:.0}"),
            };
            let tail = match (a.tail_ends, a.tail_ms) {
                (true, Some(ms)) => fmt_opt_ms(Some(ms)),
                _ => "NO".to_string(),
            };
            println!(
                "{:03}    {:<17} {:<4} {:<3} {:>+5.1} {:>5.1} {:>5.1} {:>6.1} {:>6.1} {:>5.2} {:>5.1}k {:>11} {:>4.0} {:>5} {:>5}  {:<9} {:<9} {:<9} {:<9}",
                a.number,
                a.name,
                a.chip,
                a.icon,
                a.stored_gain,
                a.single_peak_db,
                a.phrase_peak_db,
                a.phrase_rms_db,
                a.level(),
                a.width.min(9.99),
                a.centroid_hz / 1000.0,
                pitch,
                a.timing.attack_ms,
                fmt_opt_ms(a.timing.decay_ms),
                tail,
                a.macros[0].headline(),
                a.macros[1].headline(),
                a.macros[2].headline(),
                a.macros[3].headline()
            );
            if details {
                println!(
                    "         C4 v100 peak {:.1} dBFS at the stored gain, expected note {:.2}, pitch frames {}, attack {:.0} ms, decay {}, release {}, momentary max {:.1} LUFS, last voice +{:.0} ms, engine silent +{:.0} ms, silent +{:.0} ms ({:?})",
                    a.c4_peak_db,
                    a.expected_note,
                    a.pitch_frames,
                    a.timing.attack_ms,
                    fmt_opt_ms(a.timing.decay_ms),
                    a.timing.release_ms.map_or("-".to_string(), |v| format!("{v:.0} ms")),
                    a.lufs_m,
                    a.voice_count_off_ms,
                    a.voices_off_ms,
                    a.silent_ms,
                    a.final_status
                );
                for (m, e) in a.macros.iter().enumerate() {
                    println!(
                        "         M{} {:<6} level {:+.1} dB  centroid {:+.0} %  width {:+.2}  active {:+.0} ms  pitch {:+.1} c  vib {:+.1} c  mod {:.0} c  dist {:.2} / p95 {:.2} dB",
                        m + 1,
                        MACRO_NAMES[m],
                        e.d_level_db,
                        e.d_centroid_pct,
                        e.d_width,
                        e.d_active_ms,
                        e.d_pitch_c,
                        e.d_vibrato_c,
                        e.d_pitch_mod_c,
                        e.distance_db,
                        e.distance_p95_db
                    );
                }
            }
            for prob in &a.problems {
                println!("         !! {prob}");
                ok = false;
            }
            for known in &a.known {
                println!("         ~~ known: {known} ({INIT_STATE_NOTE})");
            }
        }
        let stat = |f: &dyn Fn(&Audit) -> f64| {
            let v: Vec<f64> = rows.iter().map(|a| f(a)).filter(|x| x.is_finite()).collect();
            let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |(lo, hi), x| (lo.min(*x), hi.max(*x)));
            (lo, hi)
        };
        let (llo, lhi) = stat(&|a| a.level());
        let (rlo, rhi) = stat(&|a| a.phrase_rms_db);
        let target = tgt.as_ref().and_then(|t| t.iter().find(|(c, _)| *c == cat)).map(|(_, t)| *t);
        println!(
            "       loudness ({}) {llo:.1}..{lhi:.1} LUFS (spread {:.1} LU{}), phrase RMS {rlo:.1}..{rhi:.1} dBFS (spread {:.1} dB)",
            level_name(cat),
            lhi - llo,
            target.map_or(String::new(), |t| format!(", target {t:.1}")),
            rhi - rlo
        );
        // One-shot categories are balanced on momentary loudness only: their phrase RMS mostly
        // measures how long the hits ring.
        let one_shot = matches!(cat, Category::Drum | Category::Sfx);
        if rows.len() > 1 && (lhi - llo > SPREAD_LIMIT || (!one_shot && rhi - rlo > SPREAD_LIMIT)) {
            println!("         !! {} loudness spread exceeds {SPREAD_LIMIT} dB", cat.tag());
            ok = false;
        }
    }
    ok
}

fn write_wav(path: &Path, l: &[f32], r: &[f32]) -> std::io::Result<()> {
    let n = l.len().min(r.len());
    let data_len = (n * 2 * 3) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&(SR as u32).to_le_bytes());
    b.extend_from_slice(&(SR as u32 * 6).to_le_bytes());
    b.extend_from_slice(&6u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        for v in [l[i], r[i]] {
            let s = (v.clamp(-1.0, 1.0) as f64 * 8_388_607.0).round() as i32;
            b.extend_from_slice(&s.to_le_bytes()[..3]);
        }
    }
    std::fs::write(path, b)
}

/// Print the telemetry and output level of the category's probe note (or phrase) every 50 ms.
fn trace_probe(f: &FactoryPreset, phrase: bool, gain: Option<f32>) {
    let mat = material(f.preset.category);
    let (ev, end) = if phrase {
        (mat.phrase.clone(), mat.phrase_end)
    } else {
        single(mat.probe_note, PHRASE_VELOCITY, mat.probe_hold)
    };
    let mut lines = Vec::new();
    let mut last = 0usize;
    let values = gain.map_or(f.state.values, |g| values_with(&f.state, g, None));
    let r = render_traced(&f.state, &values, &ev, end + secs(6.0), |pos, t| {
        if pos >= last + secs(0.05) {
            last = pos;
            lines.push((pos, *t));
        }
    });
    if phrase {
        println!("{:03} {}: phrase, last note-off at {:.2} s", f.preset.number, f.preset.name, end as f64 / SR as f64);
    } else {
        println!(
            "{:03} {}: probe note {} held {:.1} s",
            f.preset.number, f.preset.name, mat.probe_note, mat.probe_hold
        );
    }
    let w = secs(0.05);
    for (pos, t) in lines {
        let a = pos.saturating_sub(w);
        let lvl = db(rms(&r.left[a..pos], &r.right[a..pos]));
        println!(
            "  {:>6.3} s  out {:>6.1} dBFS  voices {}  env [{:.3} {:.3} {:.3} {:.3}]  pitch {:+.2} st  arp {:?}",
            pos as f64 / SR as f64,
            lvl,
            t.active_voices,
            t.env_level[0],
            t.env_level[1],
            t.env_level[2],
            t.env_level[3],
            t.pitch_offset_st,
            t.arp_step
        );
    }
}

fn file_stem(a: &Audit) -> String {
    let name: String = a.name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    format!("{:03}_{}_{}", a.number, a.category.tag(), name.trim_matches('_'))
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut library_path = root.join("presets/factory.json");
    let mut only: Option<BTreeSet<u32>> = None;
    let (mut details, mut calibrate) = (false, false);
    let mut wav_dir: Option<PathBuf> = None;
    let mut trace: Option<u32> = None;
    let mut trace_phrase = false;
    let mut trace_gain: Option<f32> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--only" => match args.next().map(|s| parse_only(&s)) {
                Some(Ok(set)) => only = Some(set),
                _ => {
                    eprintln!("--only needs a list like 1,5-9");
                    std::process::exit(2);
                }
            },
            "--details" => details = true,
            "--trace" => trace = args.next().and_then(|s| s.parse().ok()),
            "--phrase" => trace_phrase = true,
            "--gain" => trace_gain = args.next().and_then(|s| s.parse().ok()),
            "--calibrate" => calibrate = true,
            "--wav" => wav_dir = args.next().map(PathBuf::from),
            "--library" => library_path = args.next().map(PathBuf::from).unwrap_or(library_path),
            "-h" | "--help" => {
                println!("audition [--only 1,5-9] [--details] [--calibrate] [--wav DIR] [--library PATH] [--trace N [--phrase] [--gain DB]]");
                return;
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let text = match std::fs::read_to_string(&library_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read {}: {e}", library_path.display());
            std::process::exit(2);
        }
    };
    let lib = FactoryLibrary::parse(&text);
    for w in &lib.warnings {
        println!("library warning: {w}");
    }
    if let Some(n) = trace {
        let Some(f) = lib.presets.iter().find(|f| f.preset.number == n) else {
            eprintln!("no preset {n}");
            std::process::exit(2);
        };
        trace_probe(f, trace_phrase, trace_gain);
        return;
    }
    // 014 is always measured: it anchors the loudness targets.
    let selected: Vec<&FactoryPreset> = lib
        .presets
        .iter()
        .filter(|f| only.as_ref().is_none_or(|s| s.contains(&f.preset.number) || f.preset.number == INIT_NUMBER))
        .collect();
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(selected.len()));
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let started = std::time::Instant::now();
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(f) = selected.get(i) else { break };
                let a = audit(f);
                results.lock().unwrap_or_else(|e| e.into_inner()).push(a);
            });
        }
    });
    let mut audits = results.into_inner().unwrap_or_else(|e| e.into_inner());
    audits.sort_by_key(|a| a.number);
    let tgt = targets(&audits);
    for a in &mut audits {
        check(a);
        if let Some(t) = tgt.as_ref().and_then(|t| t.iter().find(|(c, _)| *c == a.category)) {
            a.suggested_gain = suggest_gain(a, t.1);
        }
    }
    if let Some(only) = &only {
        if !only.contains(&INIT_NUMBER) {
            audits.retain(|a| a.number != INIT_NUMBER);
        }
    }
    let ok = print_tables(&audits, details) && lib.warnings.is_empty();
    println!();
    println!("{} presets auditioned in {:.1} s", audits.len(), started.elapsed().as_secs_f64());

    if let Some(dir) = &wav_dir {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("cannot create {}: {e}", dir.display());
            std::process::exit(2);
        }
        for a in &audits {
            let Some(f) = lib.presets.iter().find(|f| f.preset.number == a.number) else { continue };
            let mat = material(a.category);
            let r = render_phrase(f, &mat, a.stored_gain, None, 3.0);
            let path = dir.join(format!("{}.wav", file_stem(a)));
            if let Err(e) = write_wav(&path, &r.left, &r.right) {
                eprintln!("cannot write {}: {e}", path.display());
            }
        }
        println!("phrases written to {}", dir.display());
    }

    if calibrate {
        let path = root.join("tools/presets/levels.json");
        let mut existing: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.get("out_gain").and_then(|g| g.as_object()).cloned())
            .unwrap_or_default();
        let mut changed = String::new();
        for a in audits.iter().filter(|a| a.number != INIT_NUMBER) {
            let key = format!("{:03}", a.number);
            if (a.suggested_gain - a.stored_gain).abs() >= 0.05 {
                let _ =
                    writeln!(changed, "  {key} {:<17} {:+.1} -> {:+.1} dB", a.name, a.stored_gain, a.suggested_gain);
            }
            existing.insert(key, serde_json::json!(a.suggested_gain));
        }
        let doc = serde_json::json!({
            "about": "Output gain (dB) per factory preset, measured by tools/presets/audition --calibrate \
                      (loudness targets per category, bounded by the peak limits). Read by generate.py.",
            "out_gain": existing,
        });
        let text = serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n";
        match std::fs::write(&path, text) {
            Ok(()) => println!(
                "calibration written to {}{}",
                path.display(),
                if changed.is_empty() { " (no changes)" } else { ":" }
            ),
            Err(e) => {
                eprintln!("cannot write {}: {e}", path.display());
                std::process::exit(2);
            }
        }
        print!("{changed}");
    }
    if !ok {
        std::process::exit(1);
    }
}
