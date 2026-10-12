//! The fixed page structure of the design (900 × 560 logical px), derived from the flex
//! column in `PIXL.dc.html`:
//!
//! | block    | CSS                                              | box (x0, y0, x1, y1)  |
//! |----------|--------------------------------------------------|-----------------------|
//! | header   | `height:52px; padding:0 20px; border-bottom:1px` | 0, 0, 900, 52         |
//! | tabs     | `padding:12px 20px 0; height:44px`               | 20, 64, 880, 96       |
//! | hero     | `margin:10px 20px 0; height:250px; border:1px`   | 20, 106, 880, 356     |
//! | tiles    | `padding:12px 20px 0; height:122px; gap:10px`    | 20, 368, 880, 478     |
//! | macros   | `padding:12px 20px 0; align-items:center`        | 20, 490, 880, ~520    |
//! | browser  | `position:absolute; top:52px` (overlay)          | 0, 52, 900, 560       |
//!
//! Finer geometry (chip rows, plot coordinates) lives with the code that draws it.

use pixl_gfx::kurbo::{Point, Rect};

/// Design width.
pub const WIDTH: f64 = 900.0;
/// Design height.
pub const HEIGHT: f64 = 560.0;
/// Horizontal page padding.
pub const PAD_X: f64 = 20.0;

/// Header height (including its 1 px bottom border).
pub const HEADER_H: f64 = 52.0;
/// Vertical centre of the header's content box (51 px tall above the border).
pub const HEADER_CY: f64 = 25.5;

/// The tabs row content box.
pub const TABS: Rect = Rect::new(20.0, 64.0, 880.0, 96.0);

/// The hero display's outer box (1 px border, radius 10).
pub const HERO: Rect = Rect::new(20.0, 106.0, 880.0, 356.0);
/// Hero corner radius.
pub const HERO_RADIUS: f64 = 10.0;
/// Origin of the hero's inner 858 × 248 drawing area (the design's SVG).
pub const PLOT_ORIGIN: Point = Point::new(21.0, 107.0);
/// Width of the hero drawing area.
pub const HW: f64 = 858.0;
/// Height of the hero drawing area.
pub const HH: f64 = 248.0;

/// Top of the tiles row.
pub const TILES_TOP: f64 = 368.0;
/// Bottom of the tiles row.
pub const TILES_BOTTOM: f64 = 478.0;
/// Gap between tiles.
pub const TILE_GAP: f64 = 10.0;
/// Number of tiles (the design's five plus ARP, SPEC §7.3).
pub const TILE_COUNT: usize = 6;

/// Top of the macro row's content.
pub const MACROS_TOP: f64 = 490.0;

/// Top of the preset browser overlay.
pub const BROWSER_TOP: f64 = 52.0;

/// The hero's inner drawing area in window coordinates.
pub fn plot_rect() -> Rect {
    Rect::new(PLOT_ORIGIN.x, PLOT_ORIGIN.y, PLOT_ORIGIN.x + HW, PLOT_ORIGIN.y + HH)
}

/// Converts a point in hero (SVG) coordinates to window coordinates.
pub fn plot_to_window(p: Point) -> Point {
    Point::new(p.x + PLOT_ORIGIN.x, p.y + PLOT_ORIGIN.y)
}

/// Converts a window point to hero (SVG) coordinates.
pub fn window_to_plot(p: Point) -> Point {
    Point::new(p.x - PLOT_ORIGIN.x, p.y - PLOT_ORIGIN.y)
}

/// The rectangle of tile `index` (equal grid columns).
pub fn tile_rect(index: usize) -> Rect {
    let w = (WIDTH - 2.0 * PAD_X - TILE_GAP * (TILE_COUNT as f64 - 1.0)) / TILE_COUNT as f64;
    let x = PAD_X + index as f64 * (w + TILE_GAP);
    Rect::new(x, TILES_TOP, x + w, TILES_BOTTOM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_fill_the_row() {
        assert_eq!(tile_rect(0).x0, 20.0);
        assert!((tile_rect(TILE_COUNT - 1).x1 - 880.0).abs() < 1e-9);
        assert!((tile_rect(0).width() - 135.0).abs() < 1e-9);
    }
}
