//! Embedded fonts (Geist and Geist Mono) and glyph-coverage queries.
//!
//! The fonts are compiled into the binary so the editor never depends on what is installed
//! on the user's machine. They are licensed under the SIL Open Font License 1.1; the licence
//! text is available as [`OFL_LICENSE`] and must ship with the product (e.g. in the manual or
//! an "about" screen).

use crate::text::FontFamily;
use skrifa::MetadataProvider;
use std::sync::OnceLock;

/// Full text of the SIL Open Font License 1.1 covering Geist and Geist Mono.
pub const OFL_LICENSE: &str = include_str!("../assets/fonts/OFL-Geist.txt");

/// One embedded font face.
#[derive(Clone, Copy, Debug)]
pub struct EmbeddedFace {
    /// Which family the face belongs to.
    pub family: FontFamily,
    /// CSS weight of the face (400 regular … 700 bold).
    pub weight: u16,
    /// File name inside `assets/fonts`.
    pub file: &'static str,
    /// The TrueType data.
    pub data: &'static [u8],
}

/// Every embedded face: Geist 400/500/600/700 and Geist Mono 400/500/600.
pub const FACES: [EmbeddedFace; 7] = [
    face(FontFamily::Sans, 400, "Geist-Regular.ttf", include_bytes!("../assets/fonts/Geist-Regular.ttf")),
    face(FontFamily::Sans, 500, "Geist-Medium.ttf", include_bytes!("../assets/fonts/Geist-Medium.ttf")),
    face(FontFamily::Sans, 600, "Geist-SemiBold.ttf", include_bytes!("../assets/fonts/Geist-SemiBold.ttf")),
    face(FontFamily::Sans, 700, "Geist-Bold.ttf", include_bytes!("../assets/fonts/Geist-Bold.ttf")),
    face(FontFamily::Mono, 400, "GeistMono-Regular.ttf", include_bytes!("../assets/fonts/GeistMono-Regular.ttf")),
    face(FontFamily::Mono, 500, "GeistMono-Medium.ttf", include_bytes!("../assets/fonts/GeistMono-Medium.ttf")),
    face(FontFamily::Mono, 600, "GeistMono-SemiBold.ttf", include_bytes!("../assets/fonts/GeistMono-SemiBold.ttf")),
];

const fn face(family: FontFamily, weight: u16, file: &'static str, data: &'static [u8]) -> EmbeddedFace {
    EmbeddedFace { family, weight, file, data }
}

/// Vertical metrics of a family, as fractions of the font size (em).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FamilyMetrics {
    /// Typographic ascent above the baseline (positive).
    pub ascent: f32,
    /// Typographic descent below the baseline (positive).
    pub descent: f32,
    /// Height of capital letters above the baseline.
    pub cap_height: f32,
    /// Height of lowercase `x` above the baseline.
    pub x_height: f32,
}

struct Coverage {
    charmap: skrifa::charmap::Charmap<'static>,
    metrics: FamilyMetrics,
}

fn coverage(family: FontFamily) -> Option<&'static Coverage> {
    static COVERAGE: OnceLock<[Option<Coverage>; 2]> = OnceLock::new();
    let all = COVERAGE.get_or_init(|| {
        let load = |family: FontFamily| -> Option<Coverage> {
            // All faces of a family share one character set (verified by a unit test), so the
            // regular face is representative.
            let face = FACES.iter().find(|f| f.family == family && f.weight == 400)?;
            let font = skrifa::FontRef::new(face.data).ok()?;
            let m = font.metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());
            let upem = f32::from(m.units_per_em.max(1));
            let metrics = FamilyMetrics {
                ascent: m.ascent / upem,
                descent: -m.descent / upem,
                cap_height: m.cap_height.unwrap_or(0.7 * upem) / upem,
                x_height: m.x_height.unwrap_or(0.5 * upem) / upem,
            };
            Some(Coverage { charmap: font.charmap(), metrics })
        };
        [load(FontFamily::Sans), load(FontFamily::Mono)]
    });
    all[family as usize].as_ref()
}

/// Whether `family` has a real glyph for `c`.
///
/// Characters for which this returns `false` would render as "tofu" (the `.notdef` box);
/// the text engine substitutes a vector [`Icon`](crate::Icon) for them when one exists (see
/// [`Icon::from_char`](crate::Icon::from_char)).
pub fn has_glyph(family: FontFamily, c: char) -> bool {
    // Control characters (e.g. `\n`) are handled by the line breaker, not drawn.
    if c.is_control() {
        return true;
    }
    coverage(family).is_some_and(|cov| cov.charmap.map(c).is_some())
}

/// Vertical metrics of `family` in em (fractions of the font size).
pub fn family_metrics(family: FontFamily) -> FamilyMetrics {
    coverage(family).map(|c| c.metrics).unwrap_or(FamilyMetrics {
        ascent: 1.005,
        descent: 0.295,
        cap_height: 0.71,
        x_height: 0.53,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn charset(data: &[u8]) -> Vec<u32> {
        let font = skrifa::FontRef::new(data).expect("valid font");
        font.charmap().mappings().map(|(c, _)| c).collect()
    }

    #[test]
    fn faces_of_a_family_share_one_charset() {
        for family in [FontFamily::Sans, FontFamily::Mono] {
            let mut sets = FACES.iter().filter(|f| f.family == family).map(|f| (f.file, charset(f.data)));
            let (first_name, first) = sets.next().expect("family has faces");
            for (name, set) in sets {
                assert_eq!(set, first, "{name} and {first_name} differ in coverage");
            }
        }
    }

    #[test]
    fn metrics_are_sane() {
        for family in [FontFamily::Sans, FontFamily::Mono] {
            let m = family_metrics(family);
            assert!((m.ascent - 1.005).abs() < 1e-3, "{m:?}");
            assert!((m.descent - 0.295).abs() < 1e-3, "{m:?}");
            assert!(m.cap_height > 0.6 && m.cap_height < 0.8, "{m:?}");
            assert!(m.x_height > 0.4 && m.x_height < m.cap_height, "{m:?}");
        }
    }

    #[test]
    fn coverage_matches_known_gaps() {
        // Latin, Cyrillic and the common UI punctuation are covered by both families.
        for c in "AZaz09АЯаяЁё№«»…–—±×·−‹›←→↑↓●".chars() {
            assert!(has_glyph(FontFamily::Sans, c), "Sans lacks {c:?}");
            assert!(has_glyph(FontFamily::Mono, c), "Mono lacks {c:?}");
        }
        // Symbols the design uses that Geist does not have (drawn as icons instead).
        for c in "✎∿⇅★".chars() {
            assert!(!has_glyph(FontFamily::Sans, c), "Sans unexpectedly has {c:?}");
            assert!(!has_glyph(FontFamily::Mono, c), "Mono unexpectedly has {c:?}");
        }
        assert!(!has_glyph(FontFamily::Sans, '╱'));
        assert!(has_glyph(FontFamily::Mono, '╱'));
        assert!(has_glyph(FontFamily::Sans, '\n'));
    }

    #[test]
    fn licence_is_embedded() {
        assert!(OFL_LICENSE.contains("SIL Open Font License"));
    }
}
