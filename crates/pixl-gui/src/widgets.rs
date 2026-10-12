//! Small reusable controls in the design's styles: chips, value chips, tooltips, toggle dots.
//!
//! Every widget comes as a pair: a `*_width` function (layout) and a draw function taking the
//! final rectangle, so rows can be laid out right-to-left like the design's flex rows.

use crate::theme::{self, ACCENT, CHIP_BG, INACTIVE, INK, LINE, MUTED, WHITE};
use crate::ui::Ui;
use pixl_gfx::kurbo::{Point, Rect, Stroke};
use pixl_gfx::{Color, TextStyle};

/// Font of hero chips, tooltips and value chips: Geist Mono 500 10 px.
pub fn chip_font() -> TextStyle {
    theme::mono(10.0, 500.0)
}

/// Horizontal padding of a chip (`padding: 4px 8px`).
pub const CHIP_PAD_X: f64 = 8.0;
/// Vertical padding of a chip.
pub const CHIP_PAD_Y: f64 = 4.0;
/// Height of a chip: a 13 px line box plus padding.
pub const CHIP_H: f64 = 21.0;
/// Corner radius of chips and tooltips.
pub const CHIP_RADIUS: f64 = 5.0;

/// Visual variant of a chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipKind {
    /// `#F1EFEA` background, ink text.
    Normal,
    /// Ink background, white text (selected).
    Active,
    /// 1 px `#E2DFD8` outline, no fill (the back chip).
    Outline,
    /// Like `Normal` but drawn at reduced opacity (disabled).
    Disabled,
}

/// Width of a chip showing `label`.
pub fn chip_width(ui: &mut Ui, label: &str) -> f64 {
    ui.text_width(label, &chip_font()) + 2.0 * CHIP_PAD_X
}

/// Width of an outlined chip (1 px border on each side).
pub fn outline_chip_width(ui: &mut Ui, label: &str) -> f64 {
    chip_width(ui, label) + 2.0
}

/// Draws a chip in `rect`. The label sits in a line box starting `CHIP_PAD_Y` below the top
/// (plus the border for outlined chips), centred when the row stretched the chip taller.
pub fn chip(ui: &mut Ui, rect: Rect, label: &str, kind: ChipKind, hovered: bool) {
    let font = chip_font();
    let (bg, fg) = match kind {
        ChipKind::Active => (Some(INK), WHITE),
        ChipKind::Normal | ChipKind::Disabled => (Some(if hovered { theme::SEG_BG } else { CHIP_BG }), INK),
        ChipKind::Outline => (if hovered { Some(CHIP_BG) } else { None }, INK),
    };
    if kind == ChipKind::Disabled {
        ui.push_opacity(0.4);
    }
    let border = if kind == ChipKind::Outline { 1.0 } else { 0.0 };
    ui.css_box(rect, CHIP_RADIUS, bg, (border > 0.0).then_some((border, LINE)));
    let content_h = rect.height() - 2.0 * border;
    ui.text_centered_in(rect.x0 + CHIP_PAD_X + border, rect.y0 + border, content_h, label, &font.color(fg));
    if kind == ChipKind::Disabled {
        ui.pop();
    }
}

/// Width of a drag-value chip: muted `label`, a space, ink `value`.
pub fn value_chip_width(ui: &mut Ui, label: &str, value: &str) -> f64 {
    let font = chip_font();
    let lw = if label.is_empty() { 0.0 } else { ui.text_width(label, &font) + ui.text_width(" ", &font) };
    lw + ui.text_width(value, &font) + 2.0 * CHIP_PAD_X
}

/// Draws a drag-value chip (SPEC §7.3 channel strip style).
pub fn value_chip(ui: &mut Ui, rect: Rect, label: &str, value: &str, hovered: bool, enabled: bool) {
    let font = chip_font();
    if !enabled {
        ui.push_opacity(0.4);
    }
    ui.rrect(rect, CHIP_RADIUS, if hovered && enabled { theme::SEG_BG } else { CHIP_BG });
    let mut x = rect.x0 + CHIP_PAD_X;
    let top = rect.y0 + (rect.height() - theme::line_height(10.0)) * 0.5;
    if !label.is_empty() {
        x += ui.text(x, top, label, &font.clone().color(MUTED));
        x += ui.text_width(" ", &font);
    }
    ui.text(x, top, value, &font.color(INK));
    if !enabled {
        ui.pop();
    }
}

/// Size of a tooltip box (`padding: 4px 7px`).
pub fn tip_size(ui: &mut Ui, text: &str) -> (f64, f64) {
    (ui.text_width(text, &chip_font()) + 14.0, CHIP_H)
}

/// Draws a tooltip (ink background, white mono text) with its top-left at `at`.
pub fn tip(ui: &mut Ui, at: Point, text: &str) -> Rect {
    let (w, h) = tip_size(ui, text);
    let r = Rect::new(at.x, at.y, at.x + w, at.y + h);
    ui.rrect(r, CHIP_RADIUS, INK);
    ui.text(at.x + 7.0, at.y + 4.0, text, &chip_font().color(WHITE));
    r
}

/// A tooltip centred horizontally above `anchor` (used for drag values), kept inside the
/// window.
pub fn tip_above(ui: &mut Ui, anchor: Point, text: &str) -> Rect {
    let (w, h) = tip_size(ui, text);
    let x = (anchor.x - w * 0.5).clamp(4.0, crate::layout::WIDTH - w - 4.0);
    let y = (anchor.y - h - 6.0).max(4.0);
    tip(ui, Point::new(x, y), text)
}

/// The tiles' enable dot: 8 px, 1.5 px border (`on` = accent fill).
pub fn toggle_dot(ui: &mut Ui, center: Point, on: bool, ring: Color) {
    if on {
        ui.circle(center, 4.0, ACCENT);
    } else {
        ui.stroke(&pixl_gfx::kurbo::Circle::new(center, 3.25), &Stroke::new(1.5), ring);
    }
}

/// The SPEC's 7 px channel / echo enable dot (`on` = accent fill, off = 1.5 px ring).
pub fn small_dot(ui: &mut Ui, center: Point, on: bool) {
    if on {
        ui.circle(center, 3.5, ACCENT);
    } else {
        ui.stroke(&pixl_gfx::kurbo::Circle::new(center, 2.75), &Stroke::new(1.5), INACTIVE);
    }
}
