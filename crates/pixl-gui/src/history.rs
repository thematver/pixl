//! Bounded editor history. Only fields changed by an edit are restored, preserving unrelated automation.
use pixl_core::{Bridge, MacroAssignments, ParamId, ParamValues, WaveRam};

#[derive(Clone, PartialEq)]
pub(crate) struct Snapshot {
    values: ParamValues,
    wave: WaveRam,
    macros: MacroAssignments,
}
impl Snapshot {
    pub fn capture(bridge: &dyn Bridge) -> Self {
        let mut values = ParamValues::defaults();
        for &id in ParamId::ALL { values.set(id, bridge.param(id)); }
        Self { values, wave: bridge.wave(), macros: bridge.macro_assignments() }
    }
    pub(crate) fn restore_changes(&self, other: &Self, bridge: &dyn Bridge) {
        for &id in ParamId::ALL {
            if self.values.get(id) != other.values.get(id) { bridge.set_param_once(id, self.values.get(id)); }
        }
        if self.wave != other.wave { bridge.set_wave(self.wave); }
        if self.macros != other.macros {
            // Remove changed routes first so replacement never needs an extra free slot.
            for m in 0..4 {
                for (id, depth) in other.macros.targets(m) {
                    if self.macros.depth(m, id) != Some(depth) { bridge.set_macro_assignment(m, id, 0.0); }
                }
                for (id, depth) in self.macros.targets(m) {
                    if other.macros.depth(m, id) != Some(depth) { bridge.set_macro_assignment(m, id, depth); }
                }
            }
        }
    }
}
#[derive(Default)]
pub(crate) struct History {
    undo: Vec<(Snapshot, Snapshot)>,
    redo: Vec<(Snapshot, Snapshot)>,
}
impl History {
    pub fn record(&mut self, before: Snapshot, after: Snapshot) {
        if before == after { return; }
        if self.undo.len() == 100 { self.undo.remove(0); }
        self.undo.push((before, after)); self.redo.clear();
    }
    pub fn can_undo(&self) -> bool { !self.undo.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo.is_empty() }
    pub fn undo(&mut self, bridge: &dyn Bridge) {
        if let Some((before, after)) = self.undo.pop() {
            before.restore_changes(&after, bridge); self.redo.push((before, after));
        }
    }
    pub fn redo(&mut self, bridge: &dyn Bridge) {
        if let Some((before, after)) = self.redo.pop() {
            after.restore_changes(&before, bridge); self.undo.push((before, after));
        }
    }
    pub fn clear(&mut self) { self.undo.clear(); self.redo.clear(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn undo_and_redo_preserve_unrelated_host_changes() {
        let bridge = crate::MockBridge::new();
        let before = Snapshot::capture(&bridge);
        bridge.set_param_once(ParamId::OutGain, -9.0);
        let after = Snapshot::capture(&bridge);
        let mut history = History::default();
        history.record(before.clone(), after);
        bridge.set_param_once(ParamId::Tune, 30.0);
        history.undo(&bridge);
        assert_eq!(bridge.param(ParamId::OutGain), before.values.get(ParamId::OutGain));
        assert_eq!(bridge.param(ParamId::Tune), 30.0);
        history.redo(&bridge);
        assert_eq!(bridge.param(ParamId::OutGain), -9.0);
        assert_eq!(bridge.param(ParamId::Tune), 30.0);
    }
}
