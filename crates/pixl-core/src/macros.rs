//! Macro assignments and modulation.
//!
//! PIXL has four macros (BRIGHT, DIRT, MOTION, SPACE by default). Each macro value is a
//! regular host parameter (`macro1`..`macro4`, 0..100 %). A macro can target up to
//! [`MACRO_SLOTS`] modulatable parameters, each with a bipolar depth in −1..1.
//!
//! Semantics (normalized domain): for every target,
//! `effective_norm = clamp(base_norm + Σ macro_value(0..1) × depth, 0, 1)`,
//! then denormalized and snapped for integer parameters. A macro at 0 leaves the preset's
//! base value untouched; depth +1 sweeps the full range above the base.

use crate::params::{ParamId, ParamValues};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Number of macros.
pub const MACRO_COUNT: usize = 4;
/// Targets per macro.
pub const MACRO_SLOTS: usize = 8;

/// Default macro names (shown in the GUI; English labels by design).
pub const MACRO_NAMES: [&str; MACRO_COUNT] = ["BRIGHT", "DIRT", "MOTION", "SPACE"];

/// One macro → parameter routing.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct MacroSlot {
    /// Target parameter (must be [`crate::ParamSpec::modulatable`]).
    pub target: Option<ParamId>,
    /// Bipolar depth in the normalized domain, −1..1.
    pub depth: f32,
}

/// All macro routings.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct MacroAssignments {
    pub slots: [[MacroSlot; MACRO_SLOTS]; MACRO_COUNT],
}

/// Depth magnitudes below this are treated as "no assignment".
pub const MIN_DEPTH: f32 = 0.005;

impl MacroAssignments {
    /// No routings.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Routings of a fresh instance and of the init preset (factory 014 "Hyrule Fanfare").
    pub fn factory_default() -> Self {
        let mut a = Self::empty();
        // 1 BRIGHT
        a.set(0, ParamId::SweepAmt, 0.10);
        a.set(0, ParamId::UniDetune, -0.10);
        // 2 DIRT
        a.set(1, ParamId::CrushBits, -0.40);
        a.set(1, ParamId::UniDetune, 0.20);
        // 3 MOTION
        a.set(2, ParamId::VibDepth, 0.30);
        a.set(2, ParamId::GlideTime, 0.20);
        // 4 SPACE (the wave release makes it audible with the echo off and on GB, whose
        // hard L/C/R panning ignores the unison width)
        a.set(3, ParamId::UniWidth, 0.40);
        a.set(3, ParamId::EchoMix, 0.50);
        a.set(3, ParamId::WvRelease, 0.15);
        a
    }

    /// Current depth of `macro_index` → `target`, if assigned.
    pub fn depth(&self, macro_index: usize, target: ParamId) -> Option<f32> {
        self.slots.get(macro_index)?.iter().find(|s| s.target == Some(target)).map(|s| s.depth)
    }

    /// Iterate the active routings of one macro.
    pub fn targets(&self, macro_index: usize) -> impl Iterator<Item = (ParamId, f32)> + '_ {
        self.slots
            .get(macro_index)
            .into_iter()
            .flat_map(|slots| slots.iter())
            .filter_map(|s| s.target.map(|t| (t, s.depth)))
    }

    /// Whether any macro targets `target`.
    pub fn is_targeted(&self, target: ParamId) -> bool {
        (0..MACRO_COUNT).any(|m| self.depth(m, target).is_some())
    }

    /// Create, update or (when `|depth| < MIN_DEPTH`) remove a routing.
    /// Returns `false` if the target is not modulatable or the macro has no free slot.
    pub fn set(&mut self, macro_index: usize, target: ParamId, depth: f32) -> bool {
        if macro_index >= MACRO_COUNT || !target.spec().modulatable {
            return false;
        }
        let depth = if depth.is_finite() { depth.clamp(-1.0, 1.0) } else { 0.0 };
        let slots = &mut self.slots[macro_index];
        if let Some(slot) = slots.iter_mut().find(|s| s.target == Some(target)) {
            if depth.abs() < MIN_DEPTH {
                *slot = MacroSlot::default();
            } else {
                slot.depth = depth;
            }
            return true;
        }
        if depth.abs() < MIN_DEPTH {
            return true;
        }
        match slots.iter_mut().find(|s| s.target.is_none()) {
            Some(slot) => {
                *slot = MacroSlot { target: Some(target), depth };
                true
            }
            None => false,
        }
    }

    /// Remove a routing.
    pub fn remove(&mut self, macro_index: usize, target: ParamId) {
        self.set(macro_index, target, 0.0);
    }

    /// Drop routings to non-modulatable targets and clamp depths (used after loading).
    pub fn sanitize(&mut self) {
        for slots in &mut self.slots {
            for s in slots.iter_mut() {
                match s.target {
                    Some(t) if t.spec().modulatable && s.depth.is_finite() && s.depth.abs() >= MIN_DEPTH => {
                        s.depth = s.depth.clamp(-1.0, 1.0)
                    }
                    _ => *s = MacroSlot::default(),
                }
            }
        }
    }
}

/// Apply macro modulation to `base`, writing the effective values to `out`.
///
/// `macro_values` are 0..1 (i.e. the `macroN` parameters divided by 100). Allocation-free
/// and safe to call on the audio thread every block.
pub fn apply_macros(
    base: &ParamValues,
    macro_values: [f32; MACRO_COUNT],
    assignments: &MacroAssignments,
    out: &mut ParamValues,
) {
    *out = *base;
    // Accumulate normalized offsets per touched target. Targets are few (≤ 32), so a
    // small fixed array beats a map.
    let mut touched: [(Option<ParamId>, f32); MACRO_COUNT * MACRO_SLOTS] = [(None, 0.0); MACRO_COUNT * MACRO_SLOTS];
    let mut n = 0;
    for (m, slots) in assignments.slots.iter().enumerate() {
        let mv = macro_values[m];
        if !mv.is_finite() || mv == 0.0 {
            continue;
        }
        for s in slots {
            let Some(t) = s.target else { continue };
            let off = mv.clamp(0.0, 1.0) * s.depth;
            if let Some(e) = touched[..n].iter_mut().find(|e| e.0 == Some(t)) {
                e.1 += off;
            } else {
                touched[n] = (Some(t), off);
                n += 1;
            }
        }
    }
    for (t, off) in &touched[..n] {
        let Some(t) = *t else { continue };
        let norm = t.normalize(base.get(t)) + off;
        out.0[t as usize] = t.denormalize(norm);
    }
}

// ----- serde: stored as [[{"target":"crush_bits","depth":-0.25}, ...] x4] -----

#[derive(Serialize, Deserialize)]
struct SlotRepr {
    target: String,
    depth: f32,
}

impl Serialize for MacroAssignments {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v: Vec<Vec<SlotRepr>> = (0..MACRO_COUNT)
            .map(|m| self.targets(m).map(|(t, d)| SlotRepr { target: t.key().to_string(), depth: d }).collect())
            .collect();
        v.serialize(s)
    }
}

impl<'de> Deserialize<'de> for MacroAssignments {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v: Vec<Vec<SlotRepr>> = Vec::deserialize(d)?;
        let mut out = MacroAssignments::empty();
        for (m, slots) in v.into_iter().take(MACRO_COUNT).enumerate() {
            for slot in slots {
                // Unknown keys (e.g. from a newer version) are skipped, not fatal.
                if let Some(t) = ParamId::from_key(&slot.target) {
                    out.set(m, t, slot.depth);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_update_remove() {
        let mut a = MacroAssignments::empty();
        assert!(a.set(1, ParamId::CrushBits, 0.4));
        assert_eq!(a.depth(1, ParamId::CrushBits), Some(0.4));
        assert!(a.set(1, ParamId::CrushBits, -0.2));
        assert_eq!(a.depth(1, ParamId::CrushBits), Some(-0.2));
        a.remove(1, ParamId::CrushBits);
        assert_eq!(a.depth(1, ParamId::CrushBits), None);
        assert!(!a.set(0, ParamId::Chip, 0.5), "enums are not modulatable");
    }

    #[test]
    fn slot_capacity() {
        let mut a = MacroAssignments::empty();
        let targets = [
            ParamId::P1Level,
            ParamId::P2Level,
            ParamId::WvLevel,
            ParamId::NsLevel,
            ParamId::P1Duty,
            ParamId::P2Duty,
            ParamId::CrushBits,
            ParamId::CrushRate,
        ];
        for t in targets {
            assert!(a.set(0, t, 0.5));
        }
        assert!(!a.set(0, ParamId::EchoMix, 0.5));
    }

    #[test]
    fn modulation_is_normalized_and_clamped() {
        let base = ParamValues::defaults();
        let mut a = MacroAssignments::empty();
        a.set(0, ParamId::EchoMix, 1.0);
        a.set(1, ParamId::UniVoices, 1.0);
        let mut out = ParamValues::defaults();
        apply_macros(&base, [0.5, 1.0, 0.0, 0.0], &a, &mut out);
        // echo_mix 25 % (linear 0..100) + 0.5 × 1.0 → 75 %
        assert!((out.get(ParamId::EchoMix) - 75.0).abs() < 1e-3);
        // uni_voices clamps to max and stays integral
        assert_eq!(out.get(ParamId::UniVoices), 4.0);
        // untouched params unchanged
        assert_eq!(out.get(ParamId::GlideTime), base.get(ParamId::GlideTime));
        apply_macros(&base, [0.0; 4], &a, &mut out);
        assert_eq!(out, base);
    }

    #[test]
    fn serde_roundtrip() {
        let mut a = MacroAssignments::empty();
        a.set(2, ParamId::VibDepth, 0.6);
        a.set(3, ParamId::UniWidth, 0.5);
        let json = serde_json::to_string(&a).unwrap();
        let b: MacroAssignments = serde_json::from_str(&json).unwrap();
        assert_eq!(a.depth(2, ParamId::VibDepth), b.depth(2, ParamId::VibDepth));
        assert_eq!(b.depth(3, ParamId::UniWidth), Some(0.5));
        let c: MacroAssignments = serde_json::from_str(r#"[[{"target":"nope","depth":1.0}],[],[],[]]"#).unwrap();
        assert_eq!(c, MacroAssignments::empty());
    }
}
