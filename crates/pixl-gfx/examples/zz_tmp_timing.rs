use std::time::Instant;
fn main() {
    let t = Instant::now();
    let mut ts = pixl_gfx::TextSystem::new();
    println!("TextSystem::new {:?}", t.elapsed());
    let t = Instant::now();
    let _ = ts.measure("Hyrule Fanfare", &pixl_gfx::TextStyle::sans(13.0));
    println!("first measure {:?}", t.elapsed());
    let t = Instant::now();
    let _ts2 = pixl_gfx::TextSystem::new();
    println!("TextSystem::new #2 {:?}", t.elapsed());
    let t = Instant::now();
    let mut c = parley::fontique::Collection::new(parley::fontique::CollectionOptions { shared: true, system_fonts: true });
    println!("system collection {:?}", t.elapsed());
    let t = Instant::now();
    let n = c.family_names().count();
    println!("family names {n} {:?}", t.elapsed());
    let t = Instant::now();
    let f = c.fallback_families(parley::fontique::FallbackKey::new(parley::fontique::Script::from_bytes(*b"Hani"), None)).count();
    println!("fallback {f} {:?}", t.elapsed());
    let t = Instant::now();
    let g = pixl_gfx::GpuContext::get_blocking(std::time::Duration::from_secs(30)).unwrap();
    println!("gpu {:?} (created_in {:?})", t.elapsed(), g.creation_time());
}
