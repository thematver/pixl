//! Real-time safety of the audio path (SPEC §0): once prepared, everything the plugin does in
//! `process()` — reading the host parameters, mapping MIDI, picking up wave/macro edits,
//! macro modulation, the engine, the effect chain, telemetry publishing, and the
//! re-`prepare`/`reset` nih-plug performs after a state load — runs without a single heap
//! allocation or deallocation, for any parameters, events and block sizes.
//!
//! A counting global allocator is armed only around those calls (per thread, so the test
//! harness and other threads are not counted).

mod common;

use common::Rng;
use nih_plug::prelude::NoteEvent;
use pixl::midi::{map_note_event, EventStream, TimedEvent};
use pixl::params::PixlParams;
use pixl::presets::init_state;
use pixl::processor::{BlockInput, Processor};
use pixl_core::sync::{AtomicMacros, AtomicTelemetry, AtomicWave};
use pixl_core::{wave, MacroAssignments, ParamId, ParamValues, PARAMS};
use pixl_dsp::Transport;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}
static EVENTS: AtomicUsize = AtomicUsize::new(0);

fn note_heap_use() {
    if ARMED.try_with(|a| a.get()).unwrap_or(false) {
        EVENTS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: forwards to the system allocator unchanged; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_heap_use();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        note_heap_use();
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_heap_use();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_heap_use();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `f` with heap use counted on this thread; returns the number of heap operations.
fn counted(f: impl FnOnce()) -> usize {
    let before = EVENTS.load(Ordering::SeqCst);
    ARMED.with(|a| a.set(true));
    f();
    ARMED.with(|a| a.set(false));
    EVENTS.load(Ordering::SeqCst) - before
}

#[test]
fn the_counter_works() {
    assert!(counted(|| drop(std::hint::black_box(vec![1u8; 100]))) >= 2);
    assert_eq!(
        counted(|| {
            std::hint::black_box([1u8; 100]);
        }),
        0
    );
}

/// One host block, prepared before the measured region.
struct Block {
    len: usize,
    midi: Vec<NoteEvent<()>>,
    values: ParamValues,
    tempo: f64,
    wave: Option<[u8; 32]>,
    macros: Option<MacroAssignments>,
}

fn random_block(rng: &mut Rng, base: &ParamValues, i: usize) -> Block {
    let len = [1, 16, 64, 100, 128, 333, 512, 1024, 2048][rng.below(9)];
    let mut midi = Vec::new();
    for _ in 0..rng.below(12) {
        let timing = rng.below(len + 1) as u32;
        let note = (24 + rng.below(80)) as u8;
        midi.push(match rng.below(9) {
            0..=2 => NoteEvent::NoteOn { timing, voice_id: None, channel: 0, note, velocity: rng.unit() },
            3..=4 => NoteEvent::NoteOff { timing, voice_id: None, channel: 0, note, velocity: 0.0 },
            5 => NoteEvent::MidiPitchBend { timing, channel: 0, value: rng.unit() },
            6 => NoteEvent::MidiCC { timing, channel: 0, cc: [1, 64, 120, 121, 123][rng.below(5)], value: rng.unit() },
            7 => NoteEvent::MidiChannelPressure { timing, channel: 0, pressure: rng.unit() },
            _ => NoteEvent::Choke { timing, voice_id: None, channel: 0, note },
        });
    }
    midi.sort_by_key(|e| e.timing());
    let mut values = *base;
    if i.is_multiple_of(3) {
        // Automation: a few parameters jump, including structural ones.
        for _ in 0..4 {
            let spec = &PARAMS[rng.below(PARAMS.len())];
            values.set(spec.id, spec.denormalize(rng.unit()));
        }
    }
    let wave = (rng.below(8) == 0).then(|| std::array::from_fn(|_| rng.below(16) as u8));
    let macros = (rng.below(8) == 0).then(|| {
        let mut a = MacroAssignments::factory_default();
        a.set(rng.below(4), ParamId::EchoTime, rng.unit() * 2.0 - 1.0);
        a.set(rng.below(4), ParamId::CrushRate, rng.unit() * 2.0 - 1.0);
        a
    });
    Block { len, midi, values, tempo: 60.0 + rng.unit() as f64 * 180.0, wave, macros }
}

fn rich_base() -> ParamValues {
    let mut v = init_state().values;
    for id in [ParamId::P1On, ParamId::P2On, ParamId::NsOn, ParamId::EchoOn, ParamId::CrushOn, ParamId::ArpOn] {
        v.set(id, 1.0);
    }
    v.set(ParamId::UniVoices, 4.0);
    v.set(ParamId::Polyphony, 16.0);
    v.set(ParamId::EchoFb, 90.0);
    v
}

#[test]
fn the_audio_path_never_touches_the_heap() {
    let mut rng = Rng(0x00c0_ffee_1234_5678);
    for (chip, sr) in [(0.0f32, 44_100.0f32), (1.0, 48_000.0), (2.0, 96_000.0)] {
        let mut base = rich_base();
        base.set(ParamId::Chip, chip);
        // Everything below is set up before the measured region (it allocates).
        let params = PixlParams::new();
        let mut host_values = ParamValues::defaults();
        let wave = AtomicWave::new(wave::default_wave());
        let macros = AtomicMacros::new(&MacroAssignments::factory_default());
        let telemetry = AtomicTelemetry::default();
        let mut processor = Processor::new();
        processor.prepare(sr);
        let blocks: Vec<Block> = (0..400).map(|i| random_block(&mut rng, &base, i)).collect();
        let (mut left, mut right) = (vec![0.0f32; 2048], vec![0.0f32; 2048]);

        let mut heap_ops = 0;
        let mut beat = 0.0f64;
        for (i, block) in blocks.iter().enumerate() {
            // GUI-side edits between blocks (lock-free stores, also checked).
            heap_ops += counted(|| {
                if let Some(w) = &block.wave {
                    wave.store(w);
                }
                if let Some(m) = &block.macros {
                    macros.store(m);
                }
            });
            let transport = Transport { tempo_bpm: block.tempo, playing: true, beat_pos: Some(beat) };
            beat += block.len as f64 / sr as f64 * block.tempo / 60.0;
            let (l, r) = (&mut left[..block.len], &mut right[..block.len]);
            heap_ops += counted(|| {
                // What `Pixl::process` does: read the host parameters, map MIDI, process,
                // publish telemetry.
                params.read_values(&mut host_values);
                let input = BlockInput { params: &block.values, wave: &wave, macros: &macros, transport };
                let mut source = block.midi.iter();
                let events = EventStream::new(|| source.next().map(map_note_event));
                processor.process(&input, events, l, r);
                telemetry.publish(processor.telemetry());
                if i % 97 == 0 {
                    // nih-plug re-initializes and resets after a state load (same config).
                    processor.prepare(sr);
                    processor.reset();
                }
            });
            assert!(l.iter().chain(r.iter()).all(|x| x.is_finite()), "block {i}");
        }
        assert_eq!(heap_ops, 0, "chip {chip} at {sr} Hz: the audio path used the heap {heap_ops} times");
        // Plain timed events (the Processor API used directly) are covered too.
        let ev = [
            TimedEvent { offset: 0, event: pixl_dsp::EngineEvent::NoteOn { note: 60, velocity: 1.0 } },
            TimedEvent { offset: 10, event: pixl_dsp::EngineEvent::AllSoundOff },
        ];
        let input = BlockInput { params: &base, wave: &wave, macros: &macros, transport: Transport::default() };
        assert_eq!(
            counted(|| {
                std::hint::black_box(processor.process(&input, ev, &mut left[..256], &mut right[..256]));
            }),
            0
        );
    }
}
