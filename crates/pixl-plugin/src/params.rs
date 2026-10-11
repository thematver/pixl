//! Host-facing parameters, built from the `pixl_core` parameter table.
//!
//! [`PixlParams`] implements nih-plug's [`Params`] trait by hand so that
//! [`pixl_core::PARAMS`] stays the single source of truth: every table entry becomes exactly
//! one host parameter with the table's stable key, name, group, range, default and
//! formatting. The mapping is:
//!
//! | table kind | nih-plug parameter | host range |
//! |---|---|---|
//! | `Float { skew == 1 }` | [`FloatParam`] with `FloatRange::Linear` | `min..=max` |
//! | `Float { skew != 1 }` | [`FloatParam`] with `FloatRange::Skewed` | identical normalization |
//! | `Int` | [`IntParam`] | `min..=max` |
//! | `Bool` | [`BoolParam`] | off/on |
//! | `Enum` | [`IntParam`] `0..=n-1`, labels via the table | variant index |
//!
//! Display strings and text entry go through [`ParamId::format`] / [`ParamId::parse`], so
//! the host shows exactly what the PIXL GUI shows. Host strings are additionally made
//! *canonical* ([`display_text`]): the text of a value is a fixed point of parse → format, so
//! value → text → value → text is always stable, also at precision boundaries ("9.96 ms" →
//! "10.0 ms" → 10 → "10 ms") and for values exactly between two display steps.
//!
//! The struct also owns the non-parameter state that is persisted with the plugin
//! (`wave`, `macros`, `preset`, `ui`, see [`crate::state`]) and the lock-free containers the
//! audio thread and the editor share ([`SharedState`]).

use crate::state::SharedState;
use nih_plug::prelude::*;
use pixl_core::{ParamId, ParamKind, ParamSpec, ParamValues, PARAMS, PARAM_COUNT};
use std::collections::BTreeMap;
use std::sync::Arc;

/// One host parameter. The variant follows the table kind (enums are integer parameters).
pub(crate) enum ParamSlot {
    Float(FloatParam),
    Int(IntParam),
    Bool(BoolParam),
}

impl ParamSlot {
    /// Build the nih-plug parameter for a table entry.
    fn new(spec: &'static ParamSpec) -> Self {
        let id = spec.id;
        match spec.kind {
            ParamKind::Float { min, max, skew } => {
                let range = if skew == 1.0 {
                    FloatRange::Linear { min, max }
                } else {
                    FloatRange::Skewed { min, max, factor: skew }
                };
                ParamSlot::Float(
                    FloatParam::new(spec.name, spec.default, range)
                        .with_value_to_string(Arc::new(move |v| display_text(id, v)))
                        .with_string_to_value(Arc::new(move |s| id.parse(s))),
                )
            }
            ParamKind::Int { min, max } => ParamSlot::Int(int_param(spec, min, max)),
            ParamKind::Enum { variants } => {
                let max = i32::try_from(variants.len().saturating_sub(1)).unwrap_or(0);
                ParamSlot::Int(int_param(spec, 0, max))
            }
            ParamKind::Bool => ParamSlot::Bool(
                BoolParam::new(spec.name, spec.default >= 0.5)
                    .with_value_to_string(Arc::new(move |b| display_text(id, bool_plain(b))))
                    .with_string_to_value(Arc::new(move |s| id.parse(s).map(|v| v >= 0.5))),
            ),
        }
    }

    /// Type-erased pointer for [`Params::param_map`] and [`GuiContext`] calls.
    pub(crate) fn as_ptr(&self) -> ParamPtr {
        match self {
            ParamSlot::Float(p) => p.as_ptr(),
            ParamSlot::Int(p) => p.as_ptr(),
            ParamSlot::Bool(p) => p.as_ptr(),
        }
    }

    /// Current plain value including host (CLAP) modulation, as `f32`.
    #[inline]
    fn modulated(&self) -> f32 {
        match self {
            ParamSlot::Float(p) => p.modulated_plain_value(),
            ParamSlot::Int(p) => p.modulated_plain_value() as f32,
            ParamSlot::Bool(p) => bool_plain(p.modulated_plain_value()),
        }
    }

    /// Current plain value before host modulation (what the knob shows), as `f32`.
    #[inline]
    fn unmodulated(&self) -> f32 {
        match self {
            ParamSlot::Float(p) => p.unmodulated_plain_value(),
            ParamSlot::Int(p) => p.unmodulated_plain_value() as f32,
            ParamSlot::Bool(p) => bool_plain(p.unmodulated_plain_value()),
        }
    }

    /// Plain value the host would store for a normalized value (typed preview; nih-plug's
    /// `ParamPtr::preview_plain` passes booleans through unchanged).
    #[cfg(test)]
    pub(crate) fn preview_plain(&self, normalized: f32) -> f32 {
        match self {
            ParamSlot::Float(p) => p.preview_plain(normalized),
            ParamSlot::Int(p) => p.preview_plain(normalized) as f32,
            ParamSlot::Bool(p) => bool_plain(p.preview_plain(normalized)),
        }
    }

    /// Default plain value as the host sees it.
    #[cfg(test)]
    pub(crate) fn default_plain(&self) -> f32 {
        match self {
            ParamSlot::Float(p) => p.default_plain_value(),
            ParamSlot::Int(p) => p.default_plain_value() as f32,
            ParamSlot::Bool(p) => bool_plain(p.default_plain_value()),
        }
    }

    /// Add this parameter to a CLAP remote-controls page.
    fn add_to_page(&self, page: &mut impl RemoteControlsPage) {
        match self {
            ParamSlot::Float(p) => page.add_param(p),
            ParamSlot::Int(p) => page.add_param(p),
            ParamSlot::Bool(p) => page.add_param(p),
        }
    }
}

fn int_param(spec: &'static ParamSpec, min: i32, max: i32) -> IntParam {
    let id = spec.id;
    let default = (spec.default.round() as i32).clamp(min, max);
    IntParam::new(spec.name, default, IntRange::Linear { min, max })
        .with_value_to_string(Arc::new(move |v| display_text(id, v as f32)))
        .with_string_to_value(Arc::new(move |s| id.parse(s).map(|v| v.round() as i32)))
}

/// Host display text of a plain value: [`ParamId::format`], made canonical.
///
/// The table's formatter rounds to a unit-dependent precision, and the precision changes at
/// unit boundaries (`9.96 ms` → "10.0 ms", but 10 → "10 ms"). Hosts and validators expect
/// value → text → value → text to reproduce the text, so the text is iterated through
/// parse → format until it no longer changes (one or two steps in practice). The result
/// parses back to a value that formats to the same text again.
pub fn display_text(id: ParamId, plain: f32) -> String {
    // `+ 0.0` turns −0.0 into +0.0: "-0.0 dB" would not survive the host's normalization.
    let mut text = id.format(plain + 0.0);
    for _ in 0..4 {
        let Some(value) = id.parse(&text) else { break };
        let next = id.format(value + 0.0);
        if next == text {
            break;
        }
        text = next;
    }
    text
}

#[inline]
fn bool_plain(b: bool) -> f32 {
    if b {
        1.0
    } else {
        0.0
    }
}

/// All PIXL host parameters plus the persisted non-parameter state.
///
/// Lives in an `Arc` shared by the plugin, the nih-plug wrapper and the editor bridge.
pub struct PixlParams {
    /// One slot per table entry, in [`ParamId`] order. Never resized after construction, so
    /// the [`ParamPtr`]s handed to nih-plug stay valid for the lifetime of this object.
    slots: Box<[ParamSlot]>,
    /// Wave RAM, macro routings, preset metadata, UI state, telemetry, runtime status.
    pub shared: SharedState,
}

impl Default for PixlParams {
    fn default() -> Self {
        Self::new()
    }
}

impl PixlParams {
    /// A fresh instance: every parameter at its default, default wave, factory macro
    /// routings, header showing the init preset (factory 014 "Hyrule Fanfare"), unmodified.
    pub fn new() -> Self {
        let slots: Box<[ParamSlot]> = PARAMS.iter().map(ParamSlot::new).collect();
        Self { slots, shared: SharedState::new() }
    }

    #[inline]
    pub(crate) fn slot(&self, id: ParamId) -> &ParamSlot {
        // `slots` has exactly PARAM_COUNT entries and every ParamId indexes into it.
        &self.slots[id.index()]
    }

    /// Type-erased nih-plug pointer of a parameter.
    pub fn ptr(&self, id: ParamId) -> ParamPtr {
        self.slot(id).as_ptr()
    }

    /// Plain value the DSP should use (includes host modulation), sanitized into range.
    #[inline]
    pub fn value(&self, id: ParamId) -> f32 {
        id.sanitize(self.slot(id).modulated())
    }

    /// Plain value without host modulation (the knob position), sanitized into range.
    #[inline]
    pub fn base_value(&self, id: ParamId) -> f32 {
        id.sanitize(self.slot(id).unmodulated())
    }

    /// Read every parameter (with host modulation) into `out`. Allocation- and lock-free;
    /// called on the audio thread once per processing call. Values are sanitized, so a
    /// misbehaving host can never push NaN or out-of-range values into the DSP.
    pub fn read_values(&self, out: &mut ParamValues) {
        for (i, (slot, dst)) in self.slots.iter().zip(out.0.iter_mut()).enumerate() {
            let v = slot.modulated();
            *dst = PARAMS[i].sanitize(v);
        }
    }

    /// Snapshot of the unmodulated parameter values (what a saved preset would contain).
    pub fn base_values(&self) -> ParamValues {
        let mut out = ParamValues::defaults();
        for (i, (slot, dst)) in self.slots.iter().zip(out.0.iter_mut()).enumerate() {
            *dst = PARAMS[i].sanitize(slot.unmodulated());
        }
        out
    }

    /// Number of host parameters (equals [`PARAM_COUNT`]).
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Always false; present for API symmetry with [`Self::len`].
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Populate the CLAP remote-controls pages (Bitwig & co.): macros first, then one page
    /// per channel and the effect sections.
    pub(crate) fn remote_controls(&self, context: &mut impl RemoteControlsContext) {
        use ParamId as P;
        const PAGES: &[(&str, &str, [ParamId; 8])] = &[
            (
                "Performance",
                "Macros",
                [P::Macro1, P::Macro2, P::Macro3, P::Macro4, P::OutGain, P::Tune, P::VelSens, P::PbRange],
            ),
            (
                "Channels",
                "Pulse 1",
                [P::P1Level, P::P1Pan, P::P1Duty, P::P1Coarse, P::P1Attack, P::P1Decay, P::P1Sustain, P::P1Release],
            ),
            (
                "Channels",
                "Pulse 2",
                [P::P2Level, P::P2Pan, P::P2Duty, P::P2Coarse, P::P2Attack, P::P2Decay, P::P2Sustain, P::P2Release],
            ),
            (
                "Channels",
                "Wave",
                [P::WvLevel, P::WvPan, P::WvCoarse, P::WvFine, P::WvAttack, P::WvDecay, P::WvSustain, P::WvRelease],
            ),
            (
                "Channels",
                "Noise",
                [P::NsLevel, P::NsPan, P::NsPeriod, P::NsMode, P::NsAttack, P::NsDecay, P::NsSustain, P::NsRelease],
            ),
            (
                "Effects",
                "Pitch",
                [P::SweepAmt, P::SweepTime, P::VibDepth, P::VibRate, P::VibDelay, P::GlideTime, P::ArpRate, P::ArpOct],
            ),
            (
                "Effects",
                "Tone",
                [
                    P::CrushBits,
                    P::CrushRate,
                    P::UniDetune,
                    P::UniWidth,
                    P::EchoTime,
                    P::EchoFb,
                    P::EchoMix,
                    P::EchoWidth,
                ],
            ),
        ];
        let mut section_start = 0;
        while section_start < PAGES.len() {
            let section = PAGES[section_start].0;
            let section_end =
                PAGES[section_start..].iter().position(|p| p.0 != section).map_or(PAGES.len(), |n| section_start + n);
            let pages = &PAGES[section_start..section_end];
            context.add_section(section, |s| {
                for (_, page_name, ids) in pages {
                    s.add_page(*page_name, |page| {
                        for id in ids {
                            self.slot(*id).add_to_page(page);
                        }
                    });
                }
            });
            section_start = section_end;
        }
    }
}

// SAFETY: every `ParamPtr` returned by `param_map` points into `self.slots`, a boxed slice
// that is created once in `new()` and never resized or moved out of `self`. nih-plug keeps
// the `Arc<PixlParams>` alive for as long as it uses the pointers.
unsafe impl Params for PixlParams {
    fn param_map(&self) -> Vec<(String, ParamPtr, String)> {
        debug_assert_eq!(self.slots.len(), PARAM_COUNT);
        PARAMS
            .iter()
            .zip(self.slots.iter())
            .map(|(spec, slot)| (spec.key.to_string(), slot.as_ptr(), spec.group.to_string()))
            .collect()
    }

    fn serialize_fields(&self) -> BTreeMap<String, String> {
        crate::state::serialize_fields(self)
    }

    fn deserialize_fields(&self, serialized: &BTreeMap<String, String>) {
        crate::state::deserialize_fields(self, serialized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixl_core::Unit;

    /// nih-plug's CLAP/VST3 parameter-ID hash (`wrapper::util::hash_param_id`).
    fn nih_hash(id: &str) -> u32 {
        let mut hash: u32 = 0;
        for b in id.bytes() {
            hash = hash.wrapping_mul(31).wrapping_add(b as u32);
        }
        hash & !(1 << 31)
    }

    /// Normalized test points that avoid exact half-steps of discrete parameters (nih-plug
    /// rounds `n × span` while the table rounds `min + n × span`, which only differ exactly
    /// on a .5 boundary, and nih-plug's booleans switch at `> 0.5` where the table rounds).
    fn sample_points(count: usize) -> Vec<f32> {
        (0..=count)
            .map(|k| k as f32 / count as f32)
            .map(|n| if n > 0.0 && n < 1.0 { n + 1.234e-4 } else { n })
            .collect()
    }

    #[test]
    fn param_map_matches_table_order_keys_and_groups() {
        let params = PixlParams::new();
        let map = params.param_map();
        assert_eq!(map.len(), PARAM_COUNT);
        assert_eq!(params.len(), PARAM_COUNT);
        assert!(!params.is_empty());
        for (i, (key, ptr, group)) in map.iter().enumerate() {
            let spec = &PARAMS[i];
            assert_eq!(key, spec.key);
            assert_eq!(group, spec.group);
            assert_eq!(*ptr, params.ptr(spec.id));
            // SAFETY: `params` outlives this loop.
            unsafe {
                assert_eq!(ptr.name(), spec.name);
                assert_eq!(ptr.unit(), "", "units are part of the formatted value");
            }
        }
    }

    #[test]
    fn host_parameter_ids_do_not_collide() {
        let mut hashes = std::collections::HashSet::new();
        for spec in PARAMS.iter() {
            assert!(hashes.insert(nih_hash(spec.key)), "hash collision for {}", spec.key);
        }
    }

    #[test]
    fn kinds_ranges_and_defaults_match_table() {
        let params = PixlParams::new();
        for spec in PARAMS.iter() {
            let slot = params.slot(spec.id);
            let ptr = params.ptr(spec.id);
            match (spec.kind, ptr) {
                (ParamKind::Float { .. }, ParamPtr::FloatParam(_)) => {}
                (ParamKind::Int { .. } | ParamKind::Enum { .. }, ParamPtr::IntParam(_)) => {}
                (ParamKind::Bool, ParamPtr::BoolParam(_)) => {}
                (kind, ptr) => panic!("{}: table kind {kind:?} mapped to {ptr:?}", spec.key),
            }
            assert_eq!(slot.default_plain(), spec.default, "{} default", spec.key);
            let (min, max) = spec.range();
            assert_eq!(slot.preview_plain(0.0), min, "{} min", spec.key);
            assert_eq!(slot.preview_plain(1.0), max, "{} max", spec.key);
            // SAFETY: `params` outlives every pointer use in this test.
            unsafe {
                assert_eq!(ptr.step_count(), spec.step_count(), "{} step count", spec.key);
                assert!(
                    (ptr.default_normalized_value() - spec.normalize(spec.default)).abs() < 1e-6,
                    "{} default normalized",
                    spec.key
                );
            }
            assert_eq!(params.value(spec.id), spec.default);
            assert_eq!(params.base_value(spec.id), spec.default);
        }
        let mut v = ParamValues::defaults();
        v.0.fill(f32::NAN);
        params.read_values(&mut v);
        assert_eq!(v, ParamValues::defaults());
        assert_eq!(params.base_values(), ParamValues::defaults());
    }

    #[test]
    fn normalization_matches_table() {
        let params = PixlParams::new();
        for spec in PARAMS.iter() {
            let slot = params.slot(spec.id);
            let ptr = params.ptr(spec.id);
            for n in sample_points(400) {
                let host_plain = slot.preview_plain(n);
                let table_plain = spec.denormalize(n);
                let tol = match spec.kind {
                    ParamKind::Float { min, max, .. } => 2e-5 * (max - min).abs().max(1.0),
                    _ => 0.0,
                };
                assert!(
                    (host_plain - table_plain).abs() <= tol,
                    "{} at n={n}: host {host_plain} vs table {table_plain}",
                    spec.key
                );
                // SAFETY: `params` outlives every pointer use in this test.
                let host_norm = unsafe { ptr.preview_normalized(table_plain) };
                let table_norm = spec.normalize(table_plain);
                assert!(
                    (host_norm - table_norm).abs() <= 1e-5,
                    "{} at n={n}: host norm {host_norm} vs table {table_norm}",
                    spec.key
                );
            }
        }
    }

    #[test]
    fn display_text_is_canonical_at_precision_boundaries() {
        // Each case: (param, plain, expected canonical text).
        let cases = [
            (ParamId::GlideTime, 9.96, "10 ms"),
            (ParamId::GlideTime, 10.0, "10 ms"),
            (ParamId::GlideTime, 9.94, "9.9 ms"),
            (ParamId::P1Attack, 999.7, "1.00 s"),
            (ParamId::P1Attack, 1000.0, "1.00 s"),
            (ParamId::VibRate, 9.998, "10.0 Hz"),
            (ParamId::CrushRate, 999.96, "1.00 kHz"),
            (ParamId::P1Pan, -0.5, "C"),
            (ParamId::P1Pan, 0.4, "C"),
            (ParamId::P1Pan, -30.0, "L30"),
            (ParamId::Chip, 2.0, "FREE"),
            (ParamId::NsPeriod, 5.0, "6/16"),
            (ParamId::P1Coarse, 0.0, "+0 st"),
            (ParamId::OutGain, -6.25, "-6.2 dB"),
            (ParamId::OutGain, -0.01, "0.0 dB"),
            (ParamId::OutGain, -0.0, "0.0 dB"),
        ];
        for (id, plain, expected) in cases {
            let text = display_text(id, plain);
            assert_eq!(text, expected, "{} at {plain}", id.key());
            let back = id.parse(&text).unwrap();
            assert_eq!(display_text(id, back), text, "{} fixed point", id.key());
        }
        // The default crush rate lies exactly between two display steps: whichever side the
        // host's float round trip lands on, the text is stable.
        for v in [11024.999f32, 11025.0, 11025.001] {
            let text = display_text(ParamId::CrushRate, v);
            assert_eq!(display_text(ParamId::CrushRate, ParamId::CrushRate.parse(&text).unwrap()), text);
        }
    }

    /// The same chain clap-validator's `param-conversions` test runs, on a dense grid.
    #[test]
    fn host_text_roundtrips_are_stable_for_every_parameter() {
        let params = PixlParams::new();
        for spec in PARAMS.iter() {
            let slot = params.slot(spec.id);
            let ptr = params.ptr(spec.id);
            for k in 0..=2000 {
                let n = k as f32 / 2000.0;
                // SAFETY: `params` outlives every pointer use in this test.
                unsafe {
                    let text = ptr.normalized_value_to_string(n, true);
                    assert_eq!(text, display_text(spec.id, slot.preview_plain(n)), "{} at n={n}", spec.key);
                    assert_eq!(ptr.normalized_value_to_string(n, false), text);
                    let back = ptr
                        .string_to_normalized_value(&text)
                        .unwrap_or_else(|| panic!("{}: cannot parse its own text '{text}'", spec.key));
                    let back_text = ptr.normalized_value_to_string(back, true);
                    assert_eq!(back_text, text, "{}: '{text}' -> {back} -> '{back_text}'", spec.key);
                    assert_eq!(ptr.string_to_normalized_value(&back_text), Some(back), "{}", spec.key);
                }
            }
        }
    }

    #[test]
    fn host_strings_and_text_entry() {
        let params = PixlParams::new();
        let s = |id: ParamId, plain: f32| {
            let slot = params.slot(id);
            let ptr = params.ptr(id);
            // SAFETY: `params` outlives the pointer use.
            unsafe {
                let n = match slot {
                    ParamSlot::Bool(_) => plain,
                    _ => ptr.preview_normalized(plain),
                };
                ptr.normalized_value_to_string(n, true)
            }
        };
        assert_eq!(s(ParamId::Chip, 1.0), "GB");
        assert_eq!(s(ParamId::EchoDiv, 2.0), "1/8D");
        assert_eq!(s(ParamId::SweepTime, 180.0), "180 ms");
        assert_eq!(s(ParamId::P1Pan, -30.0), "L30");
        assert_eq!(s(ParamId::NsPeriod, 5.0), "6/16");
        assert_eq!(s(ParamId::WvOn, 1.0), "On");
        assert_eq!(s(ParamId::P1On, 0.0), "Off");
        assert_eq!(s(ParamId::P1Coarse, -12.0), "-12 st");
        assert_eq!(s(ParamId::Polyphony, 8.0), "8");
        assert_eq!(s(ParamId::OutGain, 0.0), "0.0 dB");
        // SAFETY: `params` outlives every pointer use.
        unsafe {
            // Enums parse by label, case-insensitively, and by index.
            let p = params.ptr(ParamId::Chip);
            assert_eq!(p.string_to_normalized_value("free"), Some(1.0));
            assert_eq!(p.string_to_normalized_value("1"), Some(0.5));
            let p = params.ptr(ParamId::ArpChord);
            let n = p.string_to_normalized_value("min7").unwrap();
            assert_eq!(params.slot(ParamId::ArpChord).preview_plain(n), 6.0);
            // Typed-in values with or without units.
            let p = params.ptr(ParamId::SweepTime);
            let n = p.string_to_normalized_value("1.5 s").unwrap();
            assert!((params.slot(ParamId::SweepTime).preview_plain(n) - 1500.0).abs() < 0.01);
            let n = p.string_to_normalized_value("250").unwrap();
            assert!((params.slot(ParamId::SweepTime).preview_plain(n) - 250.0).abs() < 0.01);
            assert_eq!(params.ptr(ParamId::P1On).string_to_normalized_value("on"), Some(1.0));
            assert_eq!(params.ptr(ParamId::P1On).string_to_normalized_value("maybe"), None);
            assert_eq!(params.ptr(ParamId::OutGain).string_to_normalized_value("loud"), None);
        }
    }

    #[test]
    fn units_are_rendered_by_the_table_formatter() {
        // Every unit kind produces a parseable string through the host path.
        let params = PixlParams::new();
        let mut seen = std::collections::HashSet::new();
        for spec in PARAMS.iter() {
            if seen.insert(format!("{:?}", spec.unit)) {
                let ptr = params.ptr(spec.id);
                // SAFETY: `params` outlives the pointer use.
                let text = unsafe { ptr.normalized_value_to_string(ptr.default_normalized_value(), true) };
                assert!(!text.is_empty(), "{}", spec.key);
                assert!(spec.parse(&text).is_some(), "{}: '{text}'", spec.key);
                if spec.unit == Unit::Db {
                    assert!(text.ends_with("dB"));
                }
            }
        }
    }

    #[test]
    fn remote_control_pages_reference_valid_parameters() {
        struct Ctx(Vec<(String, Vec<String>)>);
        struct Section(Vec<(String, Vec<String>)>);
        struct Page(Vec<String>);
        impl RemoteControlsContext for Ctx {
            type Section = Section;
            fn add_section(&mut self, _name: impl Into<String>, f: impl FnOnce(&mut Self::Section)) {
                let mut s = Section(Vec::new());
                f(&mut s);
                self.0.extend(s.0);
            }
        }
        impl RemoteControlsSection for Section {
            type Page = Page;
            fn add_page(&mut self, name: impl Into<String>, f: impl FnOnce(&mut Self::Page)) {
                let mut p = Page(Vec::new());
                f(&mut p);
                self.0.push((name.into(), p.0));
            }
        }
        impl RemoteControlsPage for Page {
            fn add_param(&mut self, param: &impl Param) {
                self.0.push(param.name().to_string());
            }
            fn add_spacer(&mut self) {}
        }
        let params = PixlParams::new();
        let mut ctx = Ctx(Vec::new());
        params.remote_controls(&mut ctx);
        assert_eq!(ctx.0.len(), 7);
        let mut all = std::collections::HashSet::new();
        for (_, names) in &ctx.0 {
            assert_eq!(names.len(), 8);
            for n in names {
                assert!(PARAMS.iter().any(|p| p.name == n), "{n}");
                assert!(all.insert(n.clone()), "{n} on two pages");
            }
        }
        assert_eq!(ctx.0[0].1[0], "Macro 1");
    }
}
