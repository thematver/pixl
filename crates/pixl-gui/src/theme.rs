//! Palette, typography and the CSS text metrics of the design.
//!
//! Every colour and text style used by the editor is defined here, taken from
//! `docs/design/source/PIXL.dc.html` (SPEC §7.2). Geometry lives next to the code that draws it
//! (one geometry source per element), but the CSS rules that turn a font size into a line box
//! are shared and live here.

use pixl_gfx::{rgb, rgba, Color, FontFamily, TextStyle};

/// Window background `#F7F6F3`.
pub const BG: Color = rgb(0xF7F6F3);
/// Canvas / track colour `#E7E4DE` (macro bar track).
pub const CANVAS: Color = rgb(0xE7E4DE);
/// Text and active-control colour `#161616`.
pub const INK: Color = rgb(0x161616);
/// Accent orange `#FF5A1F`.
pub const ACCENT: Color = rgb(0xFF5A1F);
/// Secondary text `#6E6A63`.
pub const MUTED: Color = rgb(0x6E6A63);
/// Hairlines and borders `#E2DFD8`.
pub const LINE: Color = rgb(0xE2DFD8);
/// Plot grid `#EFEDE8`.
pub const GRID: Color = rgb(0xEFEDE8);
/// Chip background `#F1EFEA`.
pub const CHIP_BG: Color = rgb(0xF1EFEA);
/// Segmented-control and ASSIGN background `#ECE9E3`.
pub const SEG_BG: Color = rgb(0xECE9E3);
/// Inactive rings and bars `#C9C5BC`.
pub const INACTIVE: Color = rgb(0xC9C5BC);
/// Muted list text `#8A867E`.
pub const LIST_MUTED: Color = rgb(0x8A867E);
/// Secondary text on dark backgrounds `#A9A49A`.
pub const DARK_SUB: Color = rgb(0xA9A49A);
/// Pill / tag outline `#DAD7D0`.
pub const TAG_LINE: Color = rgb(0xDAD7D0);
/// Preset description text `#4A4741`.
pub const DESC: Color = rgb(0x4A4741);
/// Unset LFSR register cell border `#D6D2CA`.
pub const REG_OFF: Color = rgb(0xD6D2CA);
/// Pure white.
pub const WHITE: Color = rgb(0xFFFFFF);
/// Accent area fill (`fill-opacity .13`).
pub const ACCENT_AREA: Color = rgba(0xFF5A1F, 0.13);
/// Dashed reference line (`stroke-opacity .28`).
pub const BASE_LINE: Color = rgba(0x161616, 0.28);
/// Dashed ideal curve (`stroke-opacity .38`).
pub const GHOST_LINE: Color = rgba(0x161616, 0.38);
/// Segmented-control active shadow `0 1px 2px rgba(0,0,0,.08)`.
pub const SEG_SHADOW: Color = rgba(0x000000, 0.08);

/// Letter spacing of the design's mono labels (`.06em`).
pub const LABEL_SPACING: f32 = 0.06;
/// Letter spacing of the large Geist numbers and titles (`-.02em`).
pub const TIGHT_SPACING: f32 = -0.02;

/// Geist at `size` px and CSS `weight`, ink colour.
pub fn sans(size: f32, weight: f32) -> TextStyle {
    TextStyle::sans(size).weight(weight).color(INK)
}

/// Geist Mono at `size` px and CSS `weight`, ink colour.
pub fn mono(size: f32, weight: f32) -> TextStyle {
    TextStyle::mono(size).weight(weight).color(INK)
}

/// Ascent of the line box above the baseline for `line-height: normal`, as WebKit computes it
/// for Geist / Geist Mono (hhea ascender 1.005 em, rounded to whole pixels).
pub fn ascent(size: f32) -> f64 {
    (1.005 * f64::from(size)).round()
}

/// Height of a `line-height: normal` line box: WebKit rounds ascent (1.005 em) and descent
/// (0.295 em) separately, so 10 px text has a 13 px line box and 11 px text a 14 px one.
pub fn line_height(size: f32) -> f64 {
    ascent(size) + (0.295 * f64::from(size)).round()
}

/// Line height of a text style (`line-height: normal` unless the style sets a multiple).
pub fn line_height_of(style: &TextStyle) -> f64 {
    match style.line_height {
        Some(m) => f64::from(m * style.size),
        None => line_height(style.size),
    }
}

/// Whether `style` uses the monospaced family.
pub fn is_mono(style: &TextStyle) -> bool {
    style.family == FontFamily::Mono
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webkit_line_boxes() {
        assert_eq!(line_height(10.0), 13.0);
        assert_eq!(line_height(11.0), 14.0);
        assert_eq!(line_height(9.0), 12.0);
        assert_eq!(line_height(12.5), 17.0);
        assert_eq!(line_height(21.0), 27.0);
        assert_eq!(ascent(13.0), 13.0);
    }
}
