use skrifa::MetadataProvider;
fn main() {
    let chars = "✎╱∿⇅★‹›←→↓↑−·×●⏎▸▾▴▲▼◀▶✓…–—±∞°♪♯♭⌘⇧⌥⌫↵⎋·÷≈≤≥АБВабвЁёЙйЖжЩщЪъЭэЮюЯя№«»„“”’ÄÖÜäöüßéèçñ€$%&@#~^|\\/<>[]{}";
    for f in ["Geist-Regular", "Geist-Bold", "GeistMono-Medium"] {
        let bytes = std::fs::read(format!("{}/assets/fonts/{f}.ttf", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let font = skrifa::FontRef::new(&bytes).unwrap();
        let cm = font.charmap();
        let missing: String = chars.chars().filter(|c| cm.map(*c).is_none()).collect();
        let m = font.metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());
        println!("{f}: missing [{missing}] upem={} asc={} desc={} gap={} cap={:?} x={:?}", m.units_per_em, m.ascent, m.descent, m.leading, m.cap_height, m.x_height);
        let total = cm.mappings().count();
        println!("  total mapped: {total}");
    }
}
