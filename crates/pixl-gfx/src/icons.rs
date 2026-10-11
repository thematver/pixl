//! Vector icons.
//!
//! Geist and Geist Mono do not contain every symbol the design uses (`✎ ╱ ∿ ⇅ ★ 🔒` …). Rather
//! than rendering "tofu" boxes, the text engine replaces every character that the requested
//! family lacks — and for which an [`Icon`] exists — by the icon, drawn as crisp vector
//! geometry in the text colour, sized to the font size and weight, and spaced like a glyph.
//!
//! Icons can also be drawn directly with [`Canvas::icon`](crate::Canvas::icon) /
//! [`Canvas::icon_in`](crate::Canvas::icon_in).
//!
//! Geometry is defined in *em units* relative to the glyph origin: `x` grows to the right from
//! the left edge of the icon's advance box, `y` grows downwards with the baseline at `0`
//! (so the cap height is at about `y = -0.71`).

use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Point, Rect, RoundedRect, Shape, Stroke, Vec2};
use vello::peniko::Fill;

/// A vector icon. See the [module docs](self).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Icon {
    /// `✎` pencil (draw tool).
    Pencil,
    /// `╱` diagonal line (line tool).
    Slash,
    /// `∿` sine wave (smooth).
    Sine,
    /// `⇅` up/down arrows (invert, sort).
    UpDown,
    /// `★` filled star (favourite).
    Star,
    /// `☆` outlined star (not favourite).
    StarOutline,
    /// `🔒` padlock (locked).
    Lock,
    /// `‹` single left chevron.
    ChevronLeft,
    /// `›` single right chevron.
    ChevronRight,
    /// `←` arrow left.
    ArrowLeft,
    /// `→` arrow right.
    ArrowRight,
    /// `↑` arrow up.
    ArrowUp,
    /// `↓` arrow down.
    ArrowDown,
    /// `−` minus sign.
    Minus,
    /// `+` plus sign.
    Plus,
    /// `×` multiplication sign.
    Times,
    /// `✕` close cross (larger than [`Icon::Times`]).
    Close,
    /// `·` middle dot.
    Dot,
    /// `●` large filled circle.
    Bullet,
    /// `⏎` return key.
    Return,
    /// `▸` small triangle pointing right (submenu, disclosure).
    TriangleRight,
    /// `◂` small triangle pointing left.
    TriangleLeft,
    /// `▾` small triangle pointing down.
    TriangleDown,
    /// `▴` small triangle pointing up.
    TriangleUp,
    /// `✓` check mark.
    Check,
    /// `♪` eighth note.
    Note,
    /// `♯` sharp sign.
    Sharp,
    /// `♭` flat sign.
    Flat,
    /// `⌘` command key.
    Command,
    /// `⌥` option key.
    Option,
    /// `⌫` delete-backward key.
    Backspace,
    /// `⎋` escape key.
    Escape,
    /// `🔍` magnifying glass (search).
    Search,
}

/// One drawing operation of an icon, in em units (see the [module docs](self)).
#[derive(Clone, Debug)]
pub(crate) enum IconOp {
    /// Fill with the non-zero rule.
    Fill(BezPath),
    /// Fill with the even-odd rule (shapes with holes).
    FillEvenOdd(BezPath),
    /// Stroke with the given style (width in em).
    Stroke(BezPath, Stroke),
}

/// Vertical centre of symbol glyphs (the "math axis" of Geist), in em.
const AXIS: f64 = -0.34;

impl Icon {
    /// Every icon, in declaration order.
    pub const ALL: [Icon; 33] = [
        Icon::Pencil,
        Icon::Slash,
        Icon::Sine,
        Icon::UpDown,
        Icon::Star,
        Icon::StarOutline,
        Icon::Lock,
        Icon::ChevronLeft,
        Icon::ChevronRight,
        Icon::ArrowLeft,
        Icon::ArrowRight,
        Icon::ArrowUp,
        Icon::ArrowDown,
        Icon::Minus,
        Icon::Plus,
        Icon::Times,
        Icon::Close,
        Icon::Dot,
        Icon::Bullet,
        Icon::Return,
        Icon::TriangleRight,
        Icon::TriangleLeft,
        Icon::TriangleDown,
        Icon::TriangleUp,
        Icon::Check,
        Icon::Note,
        Icon::Sharp,
        Icon::Flat,
        Icon::Command,
        Icon::Option,
        Icon::Backspace,
        Icon::Escape,
        Icon::Search,
    ];

    /// The character this icon stands for.
    pub const fn char(self) -> char {
        match self {
            Icon::Pencil => '✎',
            Icon::Slash => '╱',
            Icon::Sine => '∿',
            Icon::UpDown => '⇅',
            Icon::Star => '★',
            Icon::StarOutline => '☆',
            Icon::Lock => '🔒',
            Icon::ChevronLeft => '‹',
            Icon::ChevronRight => '›',
            Icon::ArrowLeft => '←',
            Icon::ArrowRight => '→',
            Icon::ArrowUp => '↑',
            Icon::ArrowDown => '↓',
            Icon::Minus => '−',
            Icon::Plus => '+',
            Icon::Times => '×',
            Icon::Close => '✕',
            Icon::Dot => '·',
            Icon::Bullet => '●',
            Icon::Return => '⏎',
            Icon::TriangleRight => '▸',
            Icon::TriangleLeft => '◂',
            Icon::TriangleDown => '▾',
            Icon::TriangleUp => '▴',
            Icon::Check => '✓',
            Icon::Note => '♪',
            Icon::Sharp => '♯',
            Icon::Flat => '♭',
            Icon::Command => '⌘',
            Icon::Option => '⌥',
            Icon::Backspace => '⌫',
            Icon::Escape => '⎋',
            Icon::Search => '🔍',
        }
    }

    /// The icon for `c`, if there is one. Also accepts common aliases (`✔`, `✗`, `⬆`, …).
    pub fn from_char(c: char) -> Option<Icon> {
        let alias = match c {
            '✔' | '√' => Some(Icon::Check),
            '✗' | '✖' | '⨯' => Some(Icon::Close),
            '⬆' => Some(Icon::ArrowUp),
            '⬇' => Some(Icon::ArrowDown),
            '⬅' => Some(Icon::ArrowLeft),
            '➡' => Some(Icon::ArrowRight),
            '↵' => Some(Icon::Return),
            '▶' | '►' => Some(Icon::TriangleRight),
            '◀' | '◄' => Some(Icon::TriangleLeft),
            '▼' => Some(Icon::TriangleDown),
            '▲' => Some(Icon::TriangleUp),
            '✏' => Some(Icon::Pencil),
            '⧸' | '∕' => Some(Icon::Slash),
            '〜' | '∼' => Some(Icon::Sine),
            '⭐' => Some(Icon::Star),
            '🔎' => Some(Icon::Search),
            '•' | '⏺' => Some(Icon::Bullet),
            _ => None,
        };
        alias.or_else(|| Icon::ALL.iter().copied().find(|i| i.char() == c))
    }

    /// Horizontal advance of the icon when it replaces a character in text, in em.
    pub fn advance_em(self) -> f64 {
        match self {
            Icon::Pencil => 0.80,
            Icon::Slash => 0.62,
            Icon::Sine => 0.80,
            Icon::UpDown => 0.72,
            Icon::Star | Icon::StarOutline => 0.82,
            Icon::Lock => 0.76,
            Icon::ChevronLeft | Icon::ChevronRight => 0.42,
            Icon::ArrowLeft | Icon::ArrowRight => 0.80,
            Icon::ArrowUp | Icon::ArrowDown => 0.62,
            Icon::Minus | Icon::Plus | Icon::Times => 0.60,
            Icon::Close => 0.70,
            Icon::Dot => 0.30,
            Icon::Bullet => 0.70,
            Icon::Return => 0.80,
            Icon::TriangleRight | Icon::TriangleLeft | Icon::TriangleDown | Icon::TriangleUp => 0.60,
            Icon::Check => 0.70,
            Icon::Note => 0.62,
            Icon::Sharp => 0.62,
            Icon::Flat => 0.52,
            Icon::Command => 0.82,
            Icon::Option => 0.82,
            Icon::Backspace => 0.92,
            Icon::Escape => 0.86,
            Icon::Search => 0.80,
        }
    }

    /// Stroke width (em) that visually matches the stems of Geist at `weight` (CSS 100..900).
    pub fn stroke_width_em(weight: f32) -> f64 {
        let w = f64::from(if weight.is_finite() { weight.clamp(100.0, 900.0) } else { 400.0 });
        // Geist stems: ~0.08 em at 400, ~0.135 em at 700.
        (0.08 + (w - 400.0) * (0.055 / 300.0)).clamp(0.045, 0.17)
    }

    /// The drawing operations for this icon with stroke width `sw` (em).
    pub(crate) fn ops(self, sw: f64) -> Vec<IconOp> {
        let round = Stroke::new(sw).with_caps(Cap::Round).with_join(Join::Round);
        let butt = Stroke::new(sw).with_caps(Cap::Butt).with_join(Join::Miter);
        let mut out = Vec::with_capacity(4);
        match self {
            Icon::Pencil => {
                // A pencil pointing to the lower left: filled tip, body and eraser, separated by
                // thin gaps like the classic dingbat.
                let tip = Point::new(0.08, -0.03);
                let dir = Vec2::new(1.0, -1.0).normalize();
                let n = Vec2::new(dir.y, -dir.x);
                let half = (0.06 + sw * 0.35).min(0.11);
                let at = |s: f64, side: f64| tip + dir * s + n * (half * side);
                let mut p = BezPath::new();
                p.move_to(tip);
                p.line_to(at(0.20, 1.0));
                p.line_to(at(0.20, -1.0));
                p.close_path();
                quad(&mut p, at(0.25, 1.0), at(0.63, 1.0), at(0.63, -1.0), at(0.25, -1.0));
                quad(&mut p, at(0.68, 1.0), at(0.80, 1.0), at(0.80, -1.0), at(0.68, -1.0));
                out.push(IconOp::Fill(p));
            }
            Icon::Slash => out.push(IconOp::Stroke(line(0.07, 0.10, 0.55, -0.84), butt)),
            Icon::Sine => {
                let (x0, x1, cy, amp) = (0.08, 0.72, -0.28, 0.12);
                let mut p = BezPath::new();
                for i in 0..=32 {
                    let t = f64::from(i) / 32.0;
                    let pt = Point::new(x0 + (x1 - x0) * t, cy - amp * (t * std::f64::consts::TAU).sin());
                    if i == 0 {
                        p.move_to(pt);
                    } else {
                        p.line_to(pt);
                    }
                }
                out.push(IconOp::Stroke(p, round));
            }
            Icon::UpDown => {
                let (top, bot, head) = (-0.68, -0.02, 0.15);
                let mut p = line(0.20, bot, 0.20, top);
                p.extend(polyline(&[(0.20 - head, top + head), (0.20, top), (0.20 + head, top + head)]));
                p.extend(line(0.52, top, 0.52, bot));
                p.extend(polyline(&[(0.52 - head, bot - head), (0.52, bot), (0.52 + head, bot - head)]));
                out.push(IconOp::Stroke(p, round));
            }
            Icon::Star => out.push(IconOp::Fill(star(Point::new(0.41, AXIS), 0.37))),
            Icon::StarOutline => {
                out.push(IconOp::Stroke(star(Point::new(0.41, AXIS), 0.37 - sw * 0.5), round));
            }
            Icon::Lock => {
                let mut body = RoundedRect::new(0.11, -0.44, 0.65, 0.0, 0.07).to_path(0.001);
                body.extend(Circle::new((0.38, -0.25), 0.06).path_elements(0.001));
                body.extend(Rect::new(0.36, -0.25, 0.40, -0.12).path_elements(0.001));
                out.push(IconOp::FillEvenOdd(body));
                let r = 0.165;
                let mut shackle = BezPath::new();
                shackle.move_to((0.38 - r, -0.42));
                shackle.line_to((0.38 - r, -0.57));
                shackle.extend(
                    vello::kurbo::Arc::new((0.38, -0.57), (r, r), std::f64::consts::PI, std::f64::consts::PI, 0.0)
                        .append_iter(0.001),
                );
                shackle.line_to((0.38 + r, -0.42));
                out.push(IconOp::Stroke(shackle, Stroke::new(sw * 1.15).with_caps(Cap::Butt)));
            }
            Icon::ChevronLeft => out.push(IconOp::Stroke(polyline(&[(0.30, -0.58), (0.12, -0.36), (0.30, -0.14)]), round)),
            Icon::ChevronRight => {
                out.push(IconOp::Stroke(polyline(&[(0.12, -0.58), (0.30, -0.36), (0.12, -0.14)]), round));
            }
            Icon::ArrowLeft => out.push(IconOp::Stroke(arrow((0.70, AXIS), (0.10, AXIS)), round)),
            Icon::ArrowRight => out.push(IconOp::Stroke(arrow((0.10, AXIS), (0.70, AXIS)), round)),
            Icon::ArrowUp => out.push(IconOp::Stroke(arrow((0.31, 0.0), (0.31, -0.70)), round)),
            Icon::ArrowDown => out.push(IconOp::Stroke(arrow((0.31, -0.70), (0.31, 0.0)), round)),
            Icon::Minus => out.push(IconOp::Stroke(line(0.10, AXIS, 0.50, AXIS), butt)),
            Icon::Plus => {
                let mut p = line(0.10, AXIS, 0.50, AXIS);
                p.extend(line(0.30, AXIS - 0.20, 0.30, AXIS + 0.20));
                out.push(IconOp::Stroke(p, butt));
            }
            Icon::Times => out.push(IconOp::Stroke(cross(Point::new(0.30, AXIS), 0.15), butt)),
            Icon::Close => out.push(IconOp::Stroke(cross(Point::new(0.35, AXIS), 0.22), round)),
            Icon::Dot => out.push(IconOp::Fill(Circle::new((0.15, AXIS), 0.07 + sw * 0.15).to_path(0.001))),
            Icon::Bullet => out.push(IconOp::Fill(Circle::new((0.35, AXIS), 0.28).to_path(0.001))),
            Icon::Return => {
                let mut p = polyline(&[(0.68, -0.68), (0.68, -0.30), (0.12, -0.30)]);
                p.extend(polyline(&[(0.28, -0.46), (0.12, -0.30), (0.28, -0.14)]));
                out.push(IconOp::Stroke(p, round));
            }
            Icon::TriangleRight => out.push(IconOp::Fill(triangle((0.17, AXIS - 0.21), (0.47, AXIS), (0.17, AXIS + 0.21)))),
            Icon::TriangleLeft => out.push(IconOp::Fill(triangle((0.43, AXIS - 0.21), (0.13, AXIS), (0.43, AXIS + 0.21)))),
            Icon::TriangleDown => out.push(IconOp::Fill(triangle((0.09, AXIS - 0.15), (0.51, AXIS - 0.15), (0.30, AXIS + 0.16)))),
            Icon::TriangleUp => out.push(IconOp::Fill(triangle((0.09, AXIS + 0.15), (0.51, AXIS + 0.15), (0.30, AXIS - 0.16)))),
            Icon::Check => out.push(IconOp::Stroke(polyline(&[(0.10, -0.33), (0.27, -0.12), (0.60, -0.62)]), round)),
            Icon::Note => {
                let head = Affine::translate((0.21, -0.08)) * Affine::rotate(-0.35) * Affine::scale_non_uniform(0.14, 0.095);
                out.push(IconOp::Fill(head * Circle::new((0.0, 0.0), 1.0).to_path(0.0001)));
                let mut p = line(0.335, -0.10, 0.335, -0.72);
                p.move_to((0.335, -0.72));
                p.curve_to((0.36, -0.58), (0.52, -0.55), (0.50, -0.38));
                out.push(IconOp::Stroke(p, round));
            }
            Icon::Sharp => {
                let mut p = line(0.25, 0.04, 0.27, -0.72);
                p.extend(line(0.41, 0.02, 0.43, -0.74));
                p.extend(line(0.10, -0.43, 0.56, -0.51));
                p.extend(line(0.08, -0.19, 0.54, -0.27));
                out.push(IconOp::Stroke(p, butt));
            }
            Icon::Flat => {
                let mut p = line(0.14, -0.76, 0.14, 0.0);
                p.curve_to((0.36, -0.10), (0.48, -0.26), (0.38, -0.36));
                p.curve_to((0.30, -0.44), (0.18, -0.38), (0.14, -0.30));
                out.push(IconOp::Stroke(p, round));
            }
            Icon::Command => {
                let (l, r, t, b, rad) = (0.30, 0.52, -0.56, -0.34, 0.10);
                let mut p = line(l, t - rad, l, b + rad);
                p.extend(line(r, t - rad, r, b + rad));
                p.extend(line(l - rad, t, r + rad, t));
                p.extend(line(l - rad, b, r + rad, b));
                for (cx, cy) in [(l - rad, t - rad), (r + rad, t - rad), (l - rad, b + rad), (r + rad, b + rad)] {
                    p.extend(Circle::new((cx, cy), rad).path_elements(0.001));
                }
                out.push(IconOp::Stroke(p, round));
            }
            Icon::Option => {
                let mut p = polyline(&[(0.08, -0.62), (0.30, -0.62), (0.52, -0.08), (0.74, -0.08)]);
                p.extend(line(0.48, -0.62, 0.74, -0.62));
                out.push(IconOp::Stroke(p, round));
            }
            Icon::Backspace => {
                let mut p = polyline(&[(0.08, -0.34), (0.30, -0.62), (0.84, -0.62), (0.84, -0.06), (0.30, -0.06)]);
                p.close_path();
                p.extend(cross(Point::new(0.57, -0.34), 0.11));
                out.push(IconOp::Stroke(p, round));
            }
            Icon::Escape => {
                let c = Point::new(0.46, -0.32);
                let rad = 0.30;
                let start = -std::f64::consts::FRAC_PI_2 - 0.35;
                let mut p = BezPath::new();
                p.extend(vello::kurbo::Arc::new(c, (rad, rad), start, -(std::f64::consts::TAU - 1.1), 0.0).append_iter(0.001));
                let tip = Point::new(0.18, -0.60);
                p.extend(line(c.x - 0.02, c.y - 0.02, tip.x, tip.y));
                p.extend(polyline(&[(tip.x, tip.y + 0.15), (tip.x, tip.y), (tip.x + 0.15, tip.y)]));
                out.push(IconOp::Stroke(p, round));
            }
            Icon::Search => {
                let mut p = Circle::new((0.34, -0.40), 0.22).to_path(0.001);
                p.extend(line(0.50, -0.24, 0.70, -0.04));
                out.push(IconOp::Stroke(p, round));
            }
        }
        out
    }

    /// Visual bounds of the icon in em units (including stroke width `sw`).
    pub(crate) fn bounds_em(self, sw: f64) -> Rect {
        let mut bounds: Option<Rect> = None;
        for op in self.ops(sw) {
            let r = match &op {
                IconOp::Fill(p) | IconOp::FillEvenOdd(p) => p.bounding_box(),
                IconOp::Stroke(p, s) => p.bounding_box().inflate(s.width * 0.5, s.width * 0.5),
            };
            bounds = Some(bounds.map_or(r, |b| b.union(r)));
        }
        bounds.unwrap_or(Rect::new(0.0, -0.5, 0.5, 0.0))
    }

    /// Emits the icon into `scene` with its em box mapped by `transform`.
    pub(crate) fn encode(
        self,
        scene: &mut vello::Scene,
        transform: Affine,
        sw: f64,
        brush: vello::peniko::BrushRef<'_>,
    ) {
        for op in self.ops(sw) {
            match &op {
                IconOp::Fill(p) => scene.fill(Fill::NonZero, transform, brush.clone(), None, p),
                IconOp::FillEvenOdd(p) => scene.fill(Fill::EvenOdd, transform, brush.clone(), None, p),
                IconOp::Stroke(p, s) => scene.stroke(s, transform, brush.clone(), None, p),
            }
        }
    }
}

fn line(x0: f64, y0: f64, x1: f64, y1: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((x0, y0));
    p.line_to((x1, y1));
    p
}

fn polyline(points: &[(f64, f64)]) -> BezPath {
    let mut p = BezPath::new();
    for (i, &pt) in points.iter().enumerate() {
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p
}

fn quad(p: &mut BezPath, a: Point, b: Point, c: Point, d: Point) {
    p.move_to(a);
    p.line_to(b);
    p.line_to(c);
    p.line_to(d);
    p.close_path();
}

fn triangle(a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> BezPath {
    let mut p = polyline(&[a, b, c]);
    p.close_path();
    p
}

fn cross(c: Point, half: f64) -> BezPath {
    let mut p = line(c.x - half, c.y - half, c.x + half, c.y + half);
    p.extend(line(c.x - half, c.y + half, c.x + half, c.y - half));
    p
}

/// Arrow from `from` to `to` with the head at `to`.
fn arrow(from: (f64, f64), to: (f64, f64)) -> BezPath {
    let (f, t) = (Point::from(from), Point::from(to));
    let dir = (t - f).normalize();
    let n = Vec2::new(-dir.y, dir.x);
    let head = 0.22;
    let mut p = line(f.x, f.y, t.x, t.y);
    let a = t - dir * head + n * head;
    let b = t - dir * head - n * head;
    p.extend(polyline(&[(a.x, a.y), (t.x, t.y), (b.x, b.y)]));
    p
}

fn star(c: Point, outer: f64) -> BezPath {
    let inner = outer * 0.40;
    let mut p = BezPath::new();
    for i in 0..10 {
        let r = if i % 2 == 0 { outer } else { inner };
        let a = -std::f64::consts::FRAC_PI_2 + f64::from(i) * std::f64::consts::PI / 5.0;
        let pt = Point::new(c.x + r * a.cos(), c.y + r * a.sin());
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chars_roundtrip_and_are_unique() {
        for (i, icon) in Icon::ALL.iter().enumerate() {
            assert_eq!(Icon::from_char(icon.char()), Some(*icon), "{icon:?}");
            for other in &Icon::ALL[i + 1..] {
                assert_ne!(icon.char(), other.char(), "{icon:?} and {other:?} share a char");
            }
        }
        assert_eq!(Icon::from_char('✔'), Some(Icon::Check));
        assert_eq!(Icon::from_char('a'), None);
    }

    #[test]
    fn geometry_is_finite_and_inside_the_glyph_box() {
        for icon in Icon::ALL {
            for weight in [100.0, 400.0, 700.0, 900.0] {
                let sw = Icon::stroke_width_em(weight);
                let ops = icon.ops(sw);
                assert!(!ops.is_empty(), "{icon:?} draws nothing");
                let b = icon.bounds_em(sw);
                assert!(b.is_finite(), "{icon:?} bounds {b:?}");
                // Stay roughly within the advance box and between descender and ascender.
                assert!(b.x0 >= -0.05 && b.x1 <= icon.advance_em() + 0.05, "{icon:?} x-bounds {b:?}");
                assert!(b.y0 >= -0.95 && b.y1 <= 0.2, "{icon:?} y-bounds {b:?}");
                assert!(b.width() > 0.05 && b.height() > 0.05, "{icon:?} degenerate {b:?}");
            }
        }
    }

    #[test]
    fn stroke_width_tracks_weight() {
        assert!(Icon::stroke_width_em(700.0) > Icon::stroke_width_em(400.0));
        assert!(Icon::stroke_width_em(f32::NAN).is_finite());
        assert!(Icon::stroke_width_em(10_000.0) <= 0.17);
    }
}
