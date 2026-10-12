mod common;
use common::*;
use pixl_core::{ChipMode, ParamId};
const FS: f32 = 48_000.0;
#[test]
fn probe() {
    // Reviewer DSP-07 scenario: FREE wave alternating 0/15, 16 voices x 4 unison, notes 112-127.
    let alt: [u8; 32] = std::array::from_fn(|i| if i % 2 == 0 { 0 } else { 15 });
    for coarse in [0.0f32, 6.0, 10.0] {
        let mut p = clean(ChipMode::Free, 2);
        p.set(ParamId::UniOn, 1.0);
        p.set(ParamId::UniVoices, 4.0);
        p.set(ParamId::Polyphony, 16.0);
        p.set(ParamId::WvCoarse, coarse);
        let mut rig = Rig::new(FS, p);
        rig.e.set_wave(&alt);
        for n in 112u8..128 {
            rig.on(n, 1.0);
        }
        rig.left(4800);
        let n = secs(FS, 2.0);
        let t0 = std::time::Instant::now();
        let (l, _) = rig.render(n);
        let cpu = t0.elapsed().as_secs_f64() / (n as f64 / FS as f64) * 100.0;
        println!("coarse {coarse}: {cpu:.2} % of one core, finite {}", l.iter().all(|x| x.is_finite()));
    }
    // Long idle then reuse of fast wave voices.
    let mut p = clean(ChipMode::Free, 2);
    p.set(ParamId::WvRelease, 0.0);
    let mut rig = Rig::new(FS, p);
    rig.e.set_wave(&alt);
    for n in [118u8, 120, 123, 127] {
        rig.on(n, 1.0);
    }
    rig.left(4800);
    for n in [118u8, 120, 123, 127] {
        rig.off(n);
    }
    rig.left(secs(FS, 60.0));
    for n in [118u8, 120, 123, 127] {
        rig.on(n, 1.0);
    }
    let mut l = [0.0f32; SUB];
    let mut r = [0.0f32; SUB];
    let mut worst = 0.0f64;
    for _ in 0..8 {
        let t0 = std::time::Instant::now();
        rig.render_into(&mut l, &mut r);
        worst = worst.max(t0.elapsed().as_secs_f64());
    }
    println!("fast wave reuse after 60 s idle: worst block {:.4} ms", worst * 1e3);
}
