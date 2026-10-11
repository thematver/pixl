//! MIDI / note-event mapping: nih-plug [`NoteEvent`]s → PIXL [`EngineEvent`]s.
//!
//! | input | engine event |
//! |---|---|
//! | Note On (velocity > 0) | `NoteOn { note, velocity }` |
//! | Note On (velocity 0), Note Off | `NoteOff { note }` |
//! | Choke (CLAP) | `NoteOff { note }` (the engine API has no per-note hard stop) |
//! | Pitch bend (14-bit) | `PitchBend { value: −1..=1 }`, exactly 0 at 8192 |
//! | CC 1 mod wheel | `ModWheel { value: 0..=1 }` |
//! | CC 64 sustain | `Sustain { on: value ≥ 64 }` |
//! | CC 120 all sound off | `AllSoundOff` |
//! | CC 121 reset all controllers | bend 0, mod wheel 0, aftertouch 0, sustain off |
//! | CC 123 all notes off (and 124–127, which imply it per the MIDI spec) | `AllNotesOff` |
//! | Channel pressure | `Aftertouch { value: 0..=1 }` |
//!
//! PIXL is omni: the MIDI channel is ignored. Everything else (poly pressure, note
//! expressions, program changes, other CCs) is ignored.

use nih_plug::prelude::NoteEvent;
use pixl_dsp::EngineEvent;

/// Maximum number of engine events one note event expands to (CC 121).
pub const MAX_MAPPED: usize = 4;

/// The engine events produced by one note event, all at the same sample offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MappedEvents {
    timing: u32,
    len: u8,
    events: [EngineEvent; MAX_MAPPED],
}

impl MappedEvents {
    /// No events.
    pub fn none(timing: u32) -> Self {
        Self { timing, len: 0, events: [EngineEvent::AllNotesOff; MAX_MAPPED] }
    }

    fn with(timing: u32, list: &[EngineEvent]) -> Self {
        let mut out = Self::none(timing);
        for (dst, e) in out.events.iter_mut().zip(list) {
            *dst = *e;
        }
        out.len = list.len().min(MAX_MAPPED) as u8;
        out
    }

    /// Sample offset within the current block.
    pub fn timing(&self) -> u32 {
        self.timing
    }

    /// The events, in the order they must be applied.
    pub fn as_slice(&self) -> &[EngineEvent] {
        &self.events[..self.len as usize]
    }

    /// Whether nothing was produced.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Convert a normalized nih-plug pitch-bend value (`0..=1`, 0.5 = centre, derived from the
/// 14-bit MIDI value / 16383) into −1..=1 with exactly 0 at the MIDI centre 8192 and ±1 at
/// the extremes.
pub fn bend_from_normalized(value: f32) -> f32 {
    if !value.is_finite() {
        return 0.0;
    }
    let raw = (value.clamp(0.0, 1.0) * 16383.0).round() as i32;
    let centered = raw - 8192;
    if centered >= 0 {
        centered as f32 / 8191.0
    } else {
        centered as f32 / 8192.0
    }
}

#[inline]
fn unit(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Map one note event. Allocation-free; called on the audio thread.
pub fn map_note_event<S>(event: &NoteEvent<S>) -> MappedEvents {
    let timing = event.timing();
    match *event {
        NoteEvent::NoteOn { note, velocity, .. } if note < 128 => {
            if velocity.is_finite() && velocity > 0.0 {
                MappedEvents::with(timing, &[EngineEvent::NoteOn { note, velocity: velocity.min(1.0) }])
            } else {
                MappedEvents::with(timing, &[EngineEvent::NoteOff { note }])
            }
        }
        NoteEvent::NoteOff { note, .. } | NoteEvent::Choke { note, .. } if note < 128 => {
            MappedEvents::with(timing, &[EngineEvent::NoteOff { note }])
        }
        NoteEvent::MidiPitchBend { value, .. } => {
            MappedEvents::with(timing, &[EngineEvent::PitchBend { value: bend_from_normalized(value) }])
        }
        NoteEvent::MidiChannelPressure { pressure, .. } => {
            MappedEvents::with(timing, &[EngineEvent::Aftertouch { value: unit(pressure) }])
        }
        NoteEvent::MidiCC { cc, value, .. } => map_cc(timing, cc, value),
        _ => MappedEvents::none(timing),
    }
}

fn map_cc(timing: u32, cc: u8, value: f32) -> MappedEvents {
    match cc {
        1 => MappedEvents::with(timing, &[EngineEvent::ModWheel { value: unit(value) }]),
        // MIDI: 0–63 off, 64–127 on. nih-plug delivers value / 127.
        64 => MappedEvents::with(timing, &[EngineEvent::Sustain { on: unit(value) * 127.0 >= 63.5 }]),
        120 => MappedEvents::with(timing, &[EngineEvent::AllSoundOff]),
        121 => MappedEvents::with(
            timing,
            &[
                EngineEvent::PitchBend { value: 0.0 },
                EngineEvent::ModWheel { value: 0.0 },
                EngineEvent::Aftertouch { value: 0.0 },
                EngineEvent::Sustain { on: false },
            ],
        ),
        123..=127 => MappedEvents::with(timing, &[EngineEvent::AllNotesOff]),
        _ => MappedEvents::none(timing),
    }
}

/// One engine event at a sample offset within the current block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedEvent {
    pub offset: u32,
    pub event: EngineEvent,
}

/// Adapter that turns a pull-based source of note events into a flat iterator of
/// [`TimedEvent`]s (one note event can expand to several engine events).
pub struct EventStream<F> {
    pull: F,
    current: MappedEvents,
    index: usize,
}

impl<F: FnMut() -> Option<MappedEvents>> EventStream<F> {
    /// `pull` returns the next mapped note event, or `None` when the block has no more.
    pub fn new(pull: F) -> Self {
        Self { pull, current: MappedEvents::none(0), index: 0 }
    }
}

impl<F: FnMut() -> Option<MappedEvents>> Iterator for EventStream<F> {
    type Item = TimedEvent;

    fn next(&mut self) -> Option<TimedEvent> {
        loop {
            if let Some(event) = self.current.as_slice().get(self.index) {
                self.index += 1;
                return Some(TimedEvent { offset: self.current.timing(), event: *event });
            }
            self.current = (self.pull)()?;
            self.index = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Ev = NoteEvent<()>;

    fn map(e: Ev) -> Vec<EngineEvent> {
        map_note_event(&e).as_slice().to_vec()
    }

    fn cc(cc: u8, raw: u8) -> Ev {
        NoteEvent::MidiCC { timing: 7, channel: 3, cc, value: raw as f32 / 127.0 }
    }

    #[test]
    fn notes() {
        let on = NoteEvent::NoteOn { timing: 12, voice_id: None, channel: 0, note: 60, velocity: 0.5 };
        let m = map_note_event::<()>(&on);
        assert_eq!(m.timing(), 12);
        assert_eq!(m.as_slice(), &[EngineEvent::NoteOn { note: 60, velocity: 0.5 }]);
        // Velocity 0 is a note off (CLAP hosts may send it as a note on).
        let on0 = NoteEvent::NoteOn { timing: 0, voice_id: None, channel: 9, note: 61, velocity: 0.0 };
        assert_eq!(map(on0), [EngineEvent::NoteOff { note: 61 }]);
        let nan = NoteEvent::NoteOn { timing: 0, voice_id: None, channel: 0, note: 62, velocity: f32::NAN };
        assert_eq!(map(nan), [EngineEvent::NoteOff { note: 62 }]);
        let loud = NoteEvent::NoteOn { timing: 0, voice_id: Some(4), channel: 0, note: 1, velocity: 7.0 };
        assert_eq!(map(loud), [EngineEvent::NoteOn { note: 1, velocity: 1.0 }]);
        let off = NoteEvent::NoteOff { timing: 3, voice_id: None, channel: 0, note: 60, velocity: 0.3 };
        assert_eq!(map(off), [EngineEvent::NoteOff { note: 60 }]);
        let choke = NoteEvent::Choke { timing: 3, voice_id: None, channel: 0, note: 64 };
        assert_eq!(map(choke), [EngineEvent::NoteOff { note: 64 }]);
        let bad = NoteEvent::NoteOn { timing: 0, voice_id: None, channel: 0, note: 200, velocity: 1.0 };
        assert!(map_note_event::<()>(&bad).is_empty());
    }

    #[test]
    fn pitch_bend_is_exact_at_center_and_extremes() {
        let pb = |raw: u16| bend_from_normalized(raw as f32 / 16383.0);
        assert_eq!(pb(8192), 0.0);
        assert_eq!(pb(0), -1.0);
        assert_eq!(pb(16383), 1.0);
        assert!((pb(4096) + 0.5).abs() < 1e-6);
        assert_eq!(bend_from_normalized(0.5), 0.0);
        assert_eq!(bend_from_normalized(f32::NAN), 0.0);
        assert_eq!(bend_from_normalized(3.0), 1.0);
        let e = NoteEvent::MidiPitchBend { timing: 1, channel: 0, value: 1.0 };
        assert_eq!(map(e), [EngineEvent::PitchBend { value: 1.0 }]);
    }

    #[test]
    fn controllers() {
        assert_eq!(map(cc(1, 127)), [EngineEvent::ModWheel { value: 1.0 }]);
        assert_eq!(map(cc(1, 0)), [EngineEvent::ModWheel { value: 0.0 }]);
        assert_eq!(map(cc(64, 64)), [EngineEvent::Sustain { on: true }]);
        assert_eq!(map(cc(64, 63)), [EngineEvent::Sustain { on: false }]);
        assert_eq!(map(cc(64, 127)), [EngineEvent::Sustain { on: true }]);
        assert_eq!(map(cc(120, 0)), [EngineEvent::AllSoundOff]);
        assert_eq!(
            map(cc(121, 0)),
            [
                EngineEvent::PitchBend { value: 0.0 },
                EngineEvent::ModWheel { value: 0.0 },
                EngineEvent::Aftertouch { value: 0.0 },
                EngineEvent::Sustain { on: false },
            ]
        );
        for n in 123..=127 {
            assert_eq!(map(cc(n, 0)), [EngineEvent::AllNotesOff], "cc {n}");
        }
        for n in [0u8, 2, 7, 10, 11, 65, 122] {
            assert!(map(cc(n, 100)).is_empty(), "cc {n}");
        }
        let at = NoteEvent::MidiChannelPressure { timing: 0, channel: 0, pressure: 0.25 };
        assert_eq!(map(at), [EngineEvent::Aftertouch { value: 0.25 }]);
        let poly = NoteEvent::PolyPressure { timing: 0, voice_id: None, channel: 0, note: 60, pressure: 1.0 };
        assert!(map(poly).is_empty());
        let pc = NoteEvent::MidiProgramChange { timing: 0, channel: 0, program: 3 };
        assert!(map(pc).is_empty());
    }

    #[test]
    fn event_stream_flattens_in_order() {
        let source = vec![
            map_note_event::<()>(&NoteEvent::NoteOn { timing: 0, voice_id: None, channel: 0, note: 60, velocity: 1.0 }),
            map_note_event::<()>(&cc(7, 1)), // ignored
            map_note_event::<()>(&cc(121, 0)),
            map_note_event::<()>(&NoteEvent::NoteOff {
                timing: 9,
                voice_id: None,
                channel: 0,
                note: 60,
                velocity: 0.0,
            }),
        ];
        let mut it = source.into_iter();
        let events: Vec<_> = EventStream::new(move || it.next()).collect();
        assert_eq!(events.len(), 6);
        assert_eq!(events[0], TimedEvent { offset: 0, event: EngineEvent::NoteOn { note: 60, velocity: 1.0 } });
        assert!(events[1..5].iter().all(|e| e.offset == 7));
        assert_eq!(events[4].event, EngineEvent::Sustain { on: false });
        assert_eq!(events[5], TimedEvent { offset: 9, event: EngineEvent::NoteOff { note: 60 } });
    }
}
