//! Text: styles, shaping (parley), layout caching, measurement, caret geometry.
//!
//! The entry point is [`TextSystem`] (one per view; reachable through
//! [`Canvas::text_system`](crate::Canvas::text_system) and
//! [`EventCtx::text`](crate::EventCtx::text)). Most code never touches it directly and calls
//! [`Canvas::text`](crate::Canvas::text) / [`Canvas::text_in`](crate::Canvas::text_in) /
//! [`Canvas::measure`](crate::Canvas::measure) instead.
//!
//! Shaped layouts are cached by *text + style* (the colour is not part of the key, so
//! re-colouring a label on hover costs nothing). Entries that have not been used for a few
//! seconds are evicted automatically, so labels that animate through many values do not grow
//! the cache without bound.
//!
//! Characters missing from Geist are replaced by vector [`Icon`]s (see [`crate::icons`]).

use crate::fonts;
use crate::icons::Icon;
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamilyName, FontWeight, InlineBox, InlineBoxKind, LayoutContext,
    LineHeight, PositionedLayoutItem, StyleProperty, VerticalAlign,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;
use vello::kurbo::{Affine, Point, Rect, Size};
use vello::peniko::{BrushRef, Color, Fill, FontData};

/// Font family. Both are embedded (see [`crate::fonts`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontFamily {
    /// Geist (400, 500, 600, 700).
    #[default]
    Sans = 0,
    /// Geist Mono (400, 500, 600; heavier requests use 600).
    Mono = 1,
}

/// Horizontal alignment of text relative to its anchor / box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAlign {
    /// Anchor is the left edge.
    #[default]
    Left,
    /// Anchor is the horizontal centre.
    Center,
    /// Anchor is the right edge.
    Right,
}

/// What happens when text is wider than [`TextStyle::max_width`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextOverflow {
    /// Break into multiple lines at word boundaries (long words break anywhere).
    #[default]
    Wrap,
    /// Keep one line and cut it with `…` so it fits.
    Ellipsis,
    /// Keep one line and let it overflow (the width only defines the alignment box).
    Visible,
}

/// Default text colour (the design's ink, `#161616`).
pub const INK: Color = Color::from_rgb8(0x16, 0x16, 0x16);

/// How a piece of text looks. Cheap to clone; build with the chained setters:
///
/// ```
/// use pixl_gfx::{TextStyle, TextAlign, rgb};
/// let label = TextStyle::mono(10.0).weight(500.0).letter_spacing(0.06).color(rgb(0x6E6A63));
/// let title = TextStyle::sans(18.0).weight(700.0).align(TextAlign::Center);
/// assert_eq!(label.size, 10.0);
/// assert_eq!(title.align, TextAlign::Center);
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Font family.
    pub family: FontFamily,
    /// CSS weight (100..900). Geist has 400/500/600/700, Geist Mono 400/500/600; other
    /// values pick the nearest face.
    pub weight: f32,
    /// Font size in logical pixels.
    pub size: f32,
    /// Fill colour (not part of the layout cache key).
    pub color: Color,
    /// Extra space after every character, in em (CSS `letter-spacing: 0.06em` → `0.06`).
    pub letter_spacing: f32,
    /// Line height as a multiple of the font size (CSS unitless `line-height`); `None` uses
    /// the font's natural line height (CSS `normal`, 1.3 for Geist).
    pub line_height: Option<f32>,
    /// Horizontal alignment.
    pub align: TextAlign,
    /// Width of the text box in logical pixels: wrapping width for [`TextOverflow::Wrap`],
    /// truncation width for [`TextOverflow::Ellipsis`], alignment box width always.
    pub max_width: Option<f32>,
    /// Overflow behaviour when `max_width` is set.
    pub overflow: TextOverflow,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            family: FontFamily::Sans,
            weight: 400.0,
            size: 13.0,
            color: INK,
            letter_spacing: 0.0,
            line_height: None,
            align: TextAlign::Left,
            max_width: None,
            overflow: TextOverflow::Wrap,
        }
    }
}

impl TextStyle {
    /// Geist at `size` px, weight 400, ink colour.
    pub fn sans(size: f32) -> Self {
        Self { size, ..Self::default() }
    }

    /// Geist Mono at `size` px, weight 400, ink colour.
    pub fn mono(size: f32) -> Self {
        Self { family: FontFamily::Mono, size, ..Self::default() }
    }

    /// Sets the weight (CSS 100..900).
    #[must_use]
    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    /// Sets the font size in logical pixels.
    #[must_use]
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Sets the colour.
    #[must_use]
    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Sets the letter spacing in em.
    #[must_use]
    pub fn letter_spacing(mut self, em: f32) -> Self {
        self.letter_spacing = em;
        self
    }

    /// Sets the line height as a multiple of the font size.
    #[must_use]
    pub fn line_height(mut self, multiple: f32) -> Self {
        self.line_height = Some(multiple);
        self
    }

    /// Sets the alignment.
    #[must_use]
    pub fn align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    /// Wraps text at `width` logical pixels.
    #[must_use]
    pub fn wrap(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self.overflow = TextOverflow::Wrap;
        self
    }

    /// Truncates text with `…` to fit `width` logical pixels (single line).
    #[must_use]
    pub fn ellipsis(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self.overflow = TextOverflow::Ellipsis;
        self
    }

    /// Sets the box width without wrapping (alignment only).
    #[must_use]
    pub fn box_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self.overflow = TextOverflow::Visible;
        self
    }

    /// A copy with every field forced into a sane range (no NaN, no zero sizes).
    fn sanitized(&self) -> TextStyle {
        let finite_or = |v: f32, d: f32| if v.is_finite() { v } else { d };
        TextStyle {
            family: self.family,
            weight: finite_or(self.weight, 400.0).clamp(100.0, 900.0),
            size: finite_or(self.size, 13.0).clamp(1.0, 1000.0),
            color: self.color,
            letter_spacing: finite_or(self.letter_spacing, 0.0).clamp(-0.5, 2.0),
            line_height: self.line_height.filter(|v| v.is_finite() && *v > 0.0).map(|v| v.min(10.0)),
            align: self.align,
            max_width: self.max_width.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0e6)),
            overflow: self.overflow,
        }
    }

    fn cache_hash(&self, text: &str) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        self.family.hash(&mut h);
        self.weight.to_bits().hash(&mut h);
        self.size.to_bits().hash(&mut h);
        self.letter_spacing.to_bits().hash(&mut h);
        self.line_height.map(f32::to_bits).hash(&mut h);
        self.align.hash(&mut h);
        self.max_width.map(f32::to_bits).hash(&mut h);
        self.overflow.hash(&mut h);
        h.finish()
    }

    fn same_layout(&self, other: &TextStyle) -> bool {
        self.family == other.family
            && self.weight.to_bits() == other.weight.to_bits()
            && self.size.to_bits() == other.size.to_bits()
            && self.letter_spacing.to_bits() == other.letter_spacing.to_bits()
            && self.line_height.map(f32::to_bits) == other.line_height.map(f32::to_bits)
            && self.align == other.align
            && self.max_width.map(f32::to_bits) == other.max_width.map(f32::to_bits)
            && self.overflow == other.overflow
    }
}

/// Size information about laid-out text (logical pixels).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMetrics {
    /// Advance width of the widest line (includes letter spacing, excludes trailing spaces).
    pub width: f64,
    /// Total height of all line boxes.
    pub height: f64,
    /// Distance from the top of the first line box to its baseline.
    pub baseline: f64,
    /// Number of lines.
    pub lines: usize,
}

impl TextMetrics {
    /// `width × height`.
    pub fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }
}

/// A shaped, immutable piece of text. Obtain one with [`TextSystem::layout`] or
/// [`Canvas::layout`](crate::Canvas::layout); draw it with
/// [`Canvas::draw_layout`](crate::Canvas::draw_layout).
///
/// Coordinates are relative to the top-left corner of the layout box, whose width is
/// [`TextLayout::box_width`] (the style's `max_width`, or the text width when unset).
/// Byte indices always refer to the original string passed in (also when icons were
/// substituted).
pub struct TextLayout {
    text: Box<str>,
    /// Number of leading bytes of `text` that are shown (less than `text.len()` when the
    /// text was ellipsized).
    shown_len: usize,
    metrics: TextMetrics,
    box_width: f64,
    lines: Vec<LineInfo>,
    runs: Vec<GlyphRunData>,
    icons: Vec<IconBox>,
    /// Caret stops, sorted by line then x.
    stops: Vec<CaretStop>,
}

#[derive(Clone, Copy, Debug)]
struct LineInfo {
    top: f64,
    bottom: f64,
    baseline: f64,
}

struct GlyphRunData {
    font: FontData,
    size: f32,
    coords: Vec<i16>,
    glyphs: Vec<vello::Glyph>,
}

#[derive(Clone, Copy, Debug)]
struct IconBox {
    icon: Icon,
    /// Left edge of the advance box, on the baseline.
    origin: Point,
    em: f64,
    stroke: f64,
}

#[derive(Clone, Copy, Debug)]
struct CaretStop {
    line: u32,
    x: f32,
    byte: u32,
}

impl std::fmt::Debug for TextLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextLayout")
            .field("text", &self.text)
            .field("metrics", &self.metrics)
            .field("box_width", &self.box_width)
            .field("icons", &self.icons.len())
            .finish_non_exhaustive()
    }
}

impl TextLayout {
    /// The original text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text actually displayed (a prefix of [`TextLayout::text`] followed by `…` when
    /// the style truncates and the text did not fit).
    pub fn is_truncated(&self) -> bool {
        self.shown_len < self.text.len()
    }

    /// Size information.
    pub fn metrics(&self) -> TextMetrics {
        self.metrics
    }

    /// Advance width of the widest line.
    pub fn width(&self) -> f64 {
        self.metrics.width
    }

    /// Total height of the line boxes.
    pub fn height(&self) -> f64 {
        self.metrics.height
    }

    /// Width of the box the lines are aligned in (≥ [`TextLayout::width`] unless the text
    /// overflows a [`TextOverflow::Visible`] box).
    pub fn box_width(&self) -> f64 {
        self.box_width
    }

    /// Number of lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Baseline of line `line` from the top of the box.
    pub fn line_baseline(&self, line: usize) -> Option<f64> {
        self.lines.get(line).map(|l| l.baseline)
    }

    /// Caret rectangle (zero width; top/bottom of the line box) for byte index `index`.
    /// Indices inside a multi-byte character or cluster snap to the preceding boundary;
    /// indices past the end snap to the end.
    pub fn caret_rect(&self, index: usize) -> Rect {
        let Some(stop) = self.stop_for(index) else {
            let h = self.lines.first().map_or(self.metrics.height, |l| l.bottom - l.top);
            return Rect::new(0.0, 0.0, 0.0, h);
        };
        let line = self.lines.get(stop.line as usize).copied().unwrap_or(LineInfo {
            top: 0.0,
            bottom: self.metrics.height,
            baseline: self.metrics.baseline,
        });
        let x = f64::from(stop.x);
        Rect::new(x, line.top, x, line.bottom)
    }

    /// Byte index of the caret position closest to `point` (layout-box coordinates).
    pub fn hit_test(&self, point: Point) -> usize {
        if self.stops.is_empty() {
            return 0;
        }
        let line = self.line_at_y(point.y);
        let x = point.x as f32;
        let mut best: Option<(f32, u32)> = None;
        for s in self.stops.iter().filter(|s| s.line as usize == line) {
            let d = (s.x - x).abs();
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, s.byte));
            }
        }
        best.map_or(0, |(_, b)| b as usize).min(self.shown_len)
    }

    /// Rectangles covering the byte range `range` (one per line), for selection highlights.
    pub fn selection_rects(&self, range: Range<usize>) -> Vec<Rect> {
        let (start, end) = (range.start.min(range.end), range.end.max(range.start));
        let mut out = Vec::new();
        if start == end {
            return out;
        }
        for (li, line) in self.lines.iter().enumerate() {
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for s in self.stops.iter().filter(|s| s.line as usize == li) {
                let b = s.byte as usize;
                if b >= start && b <= end {
                    lo = lo.min(s.x);
                    hi = hi.max(s.x);
                }
            }
            if lo < hi {
                out.push(Rect::new(f64::from(lo), line.top, f64::from(hi), line.bottom));
            }
        }
        out
    }

    /// The caret boundary after `index` (next character/cluster), clamped to the end.
    pub fn next_boundary(&self, index: usize) -> usize {
        self.boundaries().find(|&b| b > index).unwrap_or(self.shown_len)
    }

    /// The caret boundary before `index` (previous character/cluster), clamped to 0.
    pub fn prev_boundary(&self, index: usize) -> usize {
        self.boundaries().filter(|&b| b < index).max().unwrap_or(0)
    }

    /// Start of the word before `index` (Option+← in text fields).
    pub fn prev_word(&self, index: usize) -> usize {
        let text = &self.text[..self.shown_len];
        let mut i = index.min(text.len());
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        let mut chars = text[..i].char_indices().rev().peekable();
        while chars.peek().is_some_and(|(_, c)| !is_word_char(*c)) {
            chars.next();
        }
        let mut pos = chars.peek().map_or(0, |(p, _)| *p);
        while let Some(&(p, c)) = chars.peek() {
            if !is_word_char(c) {
                break;
            }
            pos = p;
            chars.next();
        }
        if chars.peek().is_none() && text[..i].chars().all(|c| !is_word_char(c)) {
            0
        } else {
            pos
        }
    }

    /// End of the word after `index` (Option+→ in text fields).
    pub fn next_word(&self, index: usize) -> usize {
        let text = &self.text[..self.shown_len];
        let mut i = index.min(text.len());
        while !text.is_char_boundary(i) {
            i += 1;
        }
        let mut chars = text[i..].char_indices().peekable();
        while chars.peek().is_some_and(|(_, c)| !is_word_char(*c)) {
            chars.next();
        }
        while chars.peek().is_some_and(|(_, c)| is_word_char(*c)) {
            chars.next();
        }
        chars.peek().map_or(text.len(), |(p, _)| i + p)
    }

    fn boundaries(&self) -> impl Iterator<Item = usize> + '_ {
        let mut v: Vec<usize> = self.stops.iter().map(|s| s.byte as usize).collect();
        v.sort_unstable();
        v.dedup();
        v.into_iter()
    }

    fn line_at_y(&self, y: f64) -> usize {
        for (i, l) in self.lines.iter().enumerate() {
            if y < l.bottom {
                return i;
            }
        }
        self.lines.len().saturating_sub(1)
    }

    fn stop_for(&self, index: usize) -> Option<CaretStop> {
        let index = index.min(self.shown_len) as u32;
        // Exact boundary: prefer the downstream (next line) position on soft wraps.
        let exact = self.stops.iter().filter(|s| s.byte == index).max_by_key(|s| s.line);
        if exact.is_some() {
            return exact.copied();
        }
        // Inside a cluster: snap to the nearest preceding boundary.
        self.stops.iter().filter(|s| s.byte <= index).max_by_key(|s| (s.byte, s.line)).copied()
    }

    /// Encodes the glyphs (and icons) into `scene`, with the layout box's top-left mapped by
    /// `transform`.
    pub(crate) fn encode(&self, scene: &mut vello::Scene, transform: Affine, brush: BrushRef<'_>) {
        for run in &self.runs {
            scene
                .draw_glyphs(&run.font)
                .font_size(run.size)
                .transform(transform)
                .normalized_coords(&run.coords)
                .brush(brush.clone())
                .hint(false)
                .draw(Fill::NonZero, run.glyphs.iter().copied());
        }
        for b in &self.icons {
            let t = transform * Affine::translate(b.origin.to_vec2()) * Affine::scale(b.em);
            b.icon.encode(scene, t, b.stroke, brush.clone());
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A substituted character: removed from the shaped text, replaced by an inline box.
#[derive(Clone, Copy, Debug)]
struct Substitution {
    icon: Icon,
    /// Byte index in the shaped text where the box sits.
    shaped: usize,
    /// Byte index of the character in the original text.
    orig: usize,
    /// UTF-8 length of the removed character.
    len: usize,
}

/// Text prepared for shaping: icon characters removed and recorded.
struct Prepared<'a> {
    shaped: Cow<'a, str>,
    subs: Vec<Substitution>,
}

impl<'a> Prepared<'a> {
    fn new(text: &'a str, family: FontFamily) -> Self {
        let needs = |c: char| !fonts::has_glyph(family, c) && Icon::from_char(c).is_some();
        if !text.chars().any(needs) {
            return Prepared { shaped: Cow::Borrowed(text), subs: Vec::new() };
        }
        let mut shaped = String::with_capacity(text.len());
        let mut subs = Vec::new();
        for (i, c) in text.char_indices() {
            match Icon::from_char(c).filter(|_| needs(c)) {
                Some(icon) => subs.push(Substitution { icon, shaped: shaped.len(), orig: i, len: c.len_utf8() }),
                None => shaped.push(c),
            }
        }
        Prepared { shaped: Cow::Owned(shaped), subs }
    }

    /// Maps a byte index of the shaped text to the original text. `after_boxes` selects the
    /// side of icon boxes sitting exactly at `shaped`: `true` maps to after them (cluster
    /// starts), `false` to before them (cluster ends).
    fn to_orig(&self, shaped: usize, after_boxes: bool) -> usize {
        let mut extra = 0;
        for s in &self.subs {
            if s.shaped < shaped || (after_boxes && s.shaped == shaped) {
                extra += s.len;
            }
        }
        shaped + extra
    }
}

/// Owns fonts, the shaping contexts and the layout cache. One per view.
pub struct TextSystem {
    fcx: FontContext,
    lcx: LayoutContext<()>,
    cache: HashMap<u64, Vec<CacheEntry>>,
    entries: usize,
    frame: u64,
    hits: u64,
    misses: u64,
}

struct CacheEntry {
    text: Box<str>,
    style: TextStyle,
    layout: Arc<TextLayout>,
    last_used: u64,
}

/// Cache entries unused for this many frames are evicted (when the cache is large).
const CACHE_TTL_FRAMES: u64 = 300;
/// Above this many entries, stale entries are evicted at the end of a frame.
const CACHE_SOFT_LIMIT: usize = 512;
/// Hard cap: beyond this, the least recently used entries are evicted immediately.
const CACHE_HARD_LIMIT: usize = 8192;

impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TextSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextSystem")
            .field("cached", &self.entries)
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .finish_non_exhaustive()
    }
}

impl TextSystem {
    /// Creates a text system with the embedded fonts registered (no system fonts).
    pub fn new() -> Self {
        let mut fcx = FontContext {
            collection: parley::fontique::Collection::new(parley::fontique::CollectionOptions {
                shared: false,
                system_fonts: false,
            }),
            source_cache: parley::fontique::SourceCache::default(),
        };
        for face in fonts::FACES {
            let blob = parley::fontique::Blob::new(Arc::new(face.data));
            let registered = fcx.collection.register_fonts(blob, None);
            if registered.is_empty() {
                log::error!("pixl-gfx: failed to register embedded font {}", face.file);
            }
        }
        Self { fcx, lcx: LayoutContext::new(), cache: HashMap::new(), entries: 0, frame: 0, hits: 0, misses: 0 }
    }

    /// Shapes `text` with `style` (cached). The colour of `style` is ignored.
    pub fn layout(&mut self, text: &str, style: &TextStyle) -> Arc<TextLayout> {
        let style = style.sanitized();
        let hash = style.cache_hash(text);
        if let Some(bucket) = self.cache.get_mut(&hash) {
            if let Some(e) = bucket.iter_mut().find(|e| &*e.text == text && e.style.same_layout(&style)) {
                e.last_used = self.frame;
                self.hits += 1;
                return e.layout.clone();
            }
        }
        self.misses += 1;
        let layout = Arc::new(self.build(text, &style));
        if self.entries >= CACHE_HARD_LIMIT {
            self.evict_older_than(self.frame.saturating_sub(1));
            if self.entries >= CACHE_HARD_LIMIT {
                self.cache.clear();
                self.entries = 0;
            }
        }
        self.cache.entry(hash).or_default().push(CacheEntry {
            text: text.into(),
            style,
            layout: layout.clone(),
            last_used: self.frame,
        });
        self.entries += 1;
        layout
    }

    /// Measures `text` (uses the same cache as drawing).
    pub fn measure(&mut self, text: &str, style: &TextStyle) -> TextMetrics {
        self.layout(text, style).metrics()
    }

    /// Number of cached layouts.
    pub fn cache_len(&self) -> usize {
        self.entries
    }

    /// `(hits, misses)` of the layout cache since creation.
    pub fn cache_stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Drops every cached layout.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.entries = 0;
    }

    /// Called by the renderer after every frame: advances the frame clock and evicts stale
    /// entries when the cache is large.
    pub(crate) fn end_frame(&mut self) {
        self.frame += 1;
        if self.entries > CACHE_SOFT_LIMIT && self.frame % 30 == 0 {
            self.evict_older_than(self.frame.saturating_sub(CACHE_TTL_FRAMES));
        }
    }

    fn evict_older_than(&mut self, frame: u64) {
        let mut removed = 0;
        self.cache.retain(|_, bucket| {
            let before = bucket.len();
            bucket.retain(|e| e.last_used >= frame);
            removed += before - bucket.len();
            !bucket.is_empty()
        });
        self.entries -= removed;
    }

    fn build(&mut self, text: &str, style: &TextStyle) -> TextLayout {
        let (display, shown_len): (Cow<'_, str>, usize) = match (style.overflow, style.max_width) {
            (TextOverflow::Ellipsis, Some(w)) => self.ellipsize(text, style, w as f32),
            _ => (Cow::Borrowed(text), text.len()),
        };
        let wrap = match style.overflow {
            TextOverflow::Wrap => style.max_width,
            _ => None,
        };
        let prepared = Prepared::new(&display, style.family);
        let layout = self.shape(&prepared, style, wrap);
        extract(text, shown_len, &prepared, &layout, style)
    }

    /// Returns the text to display and how many bytes of the original it shows.
    fn ellipsize<'t>(&mut self, text: &'t str, style: &TextStyle, max_w: f32) -> (Cow<'t, str>, usize) {
        let single: Cow<'t, str> = if text.contains(['\n', '\r']) {
            Cow::Owned(text.replace(['\n', '\r'], " "))
        } else {
            Cow::Borrowed(text)
        };
        let fits = |sys: &mut Self, s: &str| -> bool {
            let p = Prepared::new(s, style.family);
            sys.shape(&p, style, None).width() <= max_w + 0.01
        };
        if fits(self, &single) {
            let len = single.len();
            return (single, len);
        }
        let bounds: Vec<usize> = single.char_indices().map(|(i, _)| i).collect();
        let candidate = |k: usize| -> String {
            let end = bounds.get(k).copied().unwrap_or(single.len());
            format!("{}…", single[..end].trim_end())
        };
        // Invariant: `lo` chars fit (0 always "fits" — an ellipsis alone is the minimum),
        // `hi` chars do not.
        let (mut lo, mut hi) = (0usize, bounds.len());
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if fits(self, &candidate(mid)) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let end = bounds.get(lo).copied().unwrap_or(single.len());
        let shown = single[..end].trim_end().len();
        (Cow::Owned(candidate(lo)), shown)
    }

    fn shape(&mut self, prepared: &Prepared<'_>, style: &TextStyle, wrap: Option<f32>) -> parley::Layout<()> {
        static SANS: [FontFamilyName<'static>; 2] =
            [FontFamilyName::named(fonts_family_name(FontFamily::Sans)), FontFamilyName::named("Geist Mono")];
        static MONO: [FontFamilyName<'static>; 2] =
            [FontFamilyName::named(fonts_family_name(FontFamily::Mono)), FontFamilyName::named("Geist")];
        let stack: &'static [FontFamilyName<'static>] = match style.family {
            FontFamily::Sans => &SANS,
            FontFamily::Mono => &MONO,
        };
        let size = style.size;
        let spacing = style.letter_spacing * size;
        let mut b = self.lcx.ranged_builder(&mut self.fcx, &prepared.shaped, 1.0, false);
        b.push_default(StyleProperty::FontFamily(parley::FontFamily::List(Cow::Borrowed(stack))));
        b.push_default(StyleProperty::FontSize(size));
        b.push_default(StyleProperty::FontWeight(FontWeight::new(style.weight)));
        if spacing != 0.0 {
            b.push_default(StyleProperty::LetterSpacing(spacing));
        }
        b.push_default(StyleProperty::LineHeight(match style.line_height {
            Some(m) => LineHeight::FontSizeRelative(m),
            None => LineHeight::MetricsRelative(1.0),
        }));
        let metrics = fonts::family_metrics(style.family);
        for (i, s) in prepared.subs.iter().enumerate() {
            b.push_inline_box(InlineBox {
                id: i as u64,
                kind: InlineBoxKind::InFlow,
                index: s.shaped,
                width: (s.icon.advance_em() as f32) * size + spacing,
                height: metrics.cap_height * size,
                baseline: None,
                vertical_align: VerticalAlign::BASELINE,
            });
        }
        let mut layout = b.build(&*prepared.shaped);
        layout.break_all_lines(wrap.map(|w| w.max(1.0)));
        let alignment = match style.align {
            TextAlign::Left => Alignment::Left,
            TextAlign::Center => Alignment::Center,
            TextAlign::Right => Alignment::Right,
        };
        layout.align(alignment, AlignmentOptions { align_when_overflowing: true, last_line_alignment: None });
        layout
    }
}

const fn fonts_family_name(family: FontFamily) -> &'static str {
    match family {
        FontFamily::Sans => "Geist",
        FontFamily::Mono => "Geist Mono",
    }
}

/// Converts a parley layout into the immutable, draw-ready [`TextLayout`].
fn extract(
    original: &str,
    shown_len: usize,
    prepared: &Prepared<'_>,
    layout: &parley::Layout<()>,
    style: &TextStyle,
) -> TextLayout {
    let size = f64::from(style.size);
    let icon_stroke = Icon::stroke_width_em(style.weight);
    let mut lines = Vec::new();
    let mut runs = Vec::new();
    let mut icons = Vec::new();
    let mut stops: Vec<CaretStop> = Vec::new();
    let clamp_orig = |b: usize| b.min(shown_len) as u32;

    for (li, line) in layout.lines().enumerate() {
        let m = line.metrics();
        lines.push(LineInfo {
            top: f64::from(m.block_min_coord),
            bottom: f64::from(m.block_max_coord),
            baseline: f64::from(m.baseline),
        });
        let li = li as u32;
        let mut push_stop = |x: f32, byte: u32| {
            if let Some(last) = stops.last() {
                if last.line == li && last.byte == byte && (last.x - x).abs() < 0.01 {
                    return;
                }
            }
            stops.push(CaretStop { line: li, x, byte });
        };
        for item in line.items() {
            match item {
                PositionedLayoutItem::GlyphRun(gr) => {
                    let run = gr.run();
                    let glyphs: Vec<vello::Glyph> =
                        gr.positioned_glyphs().map(|g| vello::Glyph { id: g.id, x: g.x, y: g.y }).collect();
                    if !glyphs.is_empty() {
                        runs.push(GlyphRunData {
                            font: run.font().clone(),
                            size: run.font_size(),
                            coords: run.normalized_coords().iter().map(|c| c.to_bits()).collect(),
                            glyphs,
                        });
                    }
                    let mut x = gr.offset();
                    for cluster in run.visual_clusters() {
                        let r = cluster.text_range();
                        let (lead, trail) = if cluster.is_rtl() { (r.end, r.start) } else { (r.start, r.end) };
                        push_stop(x, clamp_orig(prepared.to_orig(lead, !cluster.is_rtl())));
                        x += cluster.advance();
                        push_stop(x, clamp_orig(prepared.to_orig(trail, cluster.is_rtl())));
                    }
                }
                PositionedLayoutItem::InlineBox(b) => {
                    let Some(sub) = usize::try_from(b.id).ok().and_then(|i| prepared.subs.get(i)) else {
                        continue;
                    };
                    let baseline = f64::from(b.y + b.height);
                    icons.push(IconBox {
                        icon: sub.icon,
                        origin: Point::new(f64::from(b.x), baseline),
                        em: size,
                        stroke: icon_stroke,
                    });
                    // Caret stops on both sides of the icon (original indices).
                    push_stop(b.x, clamp_orig(sub.orig));
                    push_stop(b.x + b.width, clamp_orig(sub.orig + sub.len));
                }
            }
        }
        // Empty lines (e.g. empty text) still get a caret stop.
        if !stops.iter().any(|s| s.line == li) {
            let r = line.text_range();
            let x = m.offset;
            push_stop(x, clamp_orig(prepared.to_orig(r.start, true)));
        }
    }
    if lines.is_empty() {
        // Empty layout: synthesize one line from the font metrics.
        let fm = fonts::family_metrics(style.family);
        let lh = match style.line_height {
            Some(mul) => f64::from(mul) * size,
            None => f64::from(fm.ascent + fm.descent) * size,
        };
        let content = f64::from(fm.ascent + fm.descent) * size;
        let top_pad = (lh - content) * 0.5;
        lines.push(LineInfo { top: 0.0, bottom: lh, baseline: top_pad + f64::from(fm.ascent) * size });
        let x = match style.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => style.max_width.unwrap_or(0.0) * 0.5,
            TextAlign::Right => style.max_width.unwrap_or(0.0),
        };
        stops.push(CaretStop { line: 0, x, byte: 0 });
    }
    stops.sort_by(|a, b| a.line.cmp(&b.line).then(a.x.total_cmp(&b.x)));

    let width = f64::from(layout.width());
    let height = lines.last().map_or(0.0, |l| l.bottom).max(f64::from(layout.height()));
    let box_width = style.max_width.map_or(width, f64::from);
    TextLayout {
        text: original.into(),
        shown_len,
        metrics: TextMetrics { width, height, baseline: lines.first().map_or(0.0, |l| l.baseline), lines: lines.len() },
        box_width,
        lines,
        runs,
        icons,
        stops,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys() -> TextSystem {
        TextSystem::new()
    }

    #[test]
    fn measures_known_strings() {
        let mut t = sys();
        let m = t.measure("PIXL", &TextStyle::sans(18.0).weight(700.0));
        assert!(m.width > 30.0 && m.width < 60.0, "{m:?}");
        // CSS `normal` line height for Geist = (1005 + 295) / 1000 em.
        assert!((m.height - 18.0 * 1.3).abs() < 0.05, "{m:?}");
        assert!((m.baseline - 18.0 * 1.005).abs() < 0.05, "{m:?}");
        assert_eq!(m.lines, 1);
        let empty = t.measure("", &TextStyle::sans(18.0));
        assert_eq!(empty.width, 0.0);
        assert!((empty.height - 18.0 * 1.3).abs() < 0.05, "{empty:?}");
    }

    #[test]
    fn heavier_weight_and_spacing_are_wider() {
        let mut t = sys();
        let base = t.measure("Hyrule Fanfare", &TextStyle::sans(13.0)).width;
        let bold = t.measure("Hyrule Fanfare", &TextStyle::sans(13.0).weight(700.0)).width;
        let spaced = t.measure("Hyrule Fanfare", &TextStyle::sans(13.0).letter_spacing(0.1)).width;
        assert!(bold > base, "{bold} vs {base}");
        // 14 characters × 1.3 px of letter spacing.
        assert!((spaced - base - 14.0 * 1.3).abs() < 0.6, "{spaced} vs {base}");
    }

    #[test]
    fn mono_is_monospaced() {
        let mut t = sys();
        let s = TextStyle::mono(10.0);
        let a = t.measure("iiii", &s).width;
        let b = t.measure("WWWW", &s).width;
        let c = t.measure("ЖЖЖЖ", &s).width;
        assert!((a - b).abs() < 0.01 && (a - c).abs() < 0.01, "{a} {b} {c}");
    }

    #[test]
    fn cache_hits_and_colour_is_not_part_of_the_key() {
        let mut t = sys();
        let a = t.layout("ГОТОВО", &TextStyle::mono(10.0));
        let b = t.layout("ГОТОВО", &TextStyle::mono(10.0).color(Color::WHITE));
        assert!(Arc::ptr_eq(&a, &b));
        let c = t.layout("ГОТОВО", &TextStyle::mono(11.0));
        assert!(!Arc::ptr_eq(&a, &c));
        assert_eq!(t.cache_stats(), (1, 2));
        assert_eq!(t.cache_len(), 2);
    }

    #[test]
    fn cache_evicts_stale_entries() {
        let mut t = sys();
        for i in 0..(CACHE_SOFT_LIMIT + 100) {
            t.layout(&format!("{i}"), &TextStyle::mono(10.0));
        }
        let before = t.cache_len();
        for _ in 0..(CACHE_TTL_FRAMES + 60) {
            t.layout("keep", &TextStyle::mono(10.0));
            t.end_frame();
        }
        assert!(t.cache_len() < before / 2, "{} → {}", before, t.cache_len());
        assert!(t.cache_len() >= 1);
    }

    #[test]
    fn wraps_at_max_width() {
        let mut t = sys();
        let text = "Яркий лид на самодельной волне, лёгкий свип вниз на атаке и вибрато с задержкой.";
        let one = t.measure(text, &TextStyle::sans(12.0));
        let wrapped = t.measure(text, &TextStyle::sans(12.0).wrap(160.0));
        assert_eq!(one.lines, 1);
        assert!(wrapped.lines >= 3, "{wrapped:?}");
        assert!(wrapped.width <= 160.0 + 0.01, "{wrapped:?}");
        let lh = one.height;
        assert!((wrapped.height - lh * wrapped.lines as f64).abs() < 0.1, "{wrapped:?}");
    }

    #[test]
    fn ellipsis_fits_and_reports_truncation() {
        let mut t = sys();
        let style = TextStyle::sans(13.0).weight(500.0).ellipsis(80.0);
        let l = t.layout("A very long user preset name that cannot fit", &style);
        assert!(l.is_truncated());
        assert!(l.width() <= 80.01, "{}", l.width());
        assert!(l.width() > 50.0, "{}", l.width());
        assert_eq!(l.line_count(), 1);
        let short = t.layout("Kick 8", &style);
        assert!(!short.is_truncated());
        // Pathological width: just the ellipsis.
        let tiny = t.layout("Kick 8", &TextStyle::sans(13.0).ellipsis(1.0));
        assert!(tiny.is_truncated());
    }

    #[test]
    fn alignment_box() {
        let mut t = sys();
        let w = t.measure("ESC", &TextStyle::mono(10.0)).width;
        let centered = t.layout("ESC", &TextStyle::mono(10.0).align(TextAlign::Center).box_width(100.0));
        assert_eq!(centered.box_width(), 100.0);
        let left = centered.caret_rect(0).x0;
        assert!((left - (100.0 - w) * 0.5).abs() < 0.05, "{left} vs {}", (100.0 - w) * 0.5);
        let right = t.layout("ESC", &TextStyle::mono(10.0).align(TextAlign::Right).box_width(100.0));
        assert!((right.caret_rect(3).x0 - 100.0).abs() < 0.05);
    }

    #[test]
    fn missing_glyphs_become_icons() {
        let mut t = sys();
        let s = TextStyle::mono(10.0).weight(500.0);
        let with_icon = t.layout("✎ DRAW", &s);
        assert_eq!(with_icon.icons.len(), 1);
        assert_eq!(with_icon.icons[0].icon, Icon::Pencil);
        // The icon takes horizontal space like a glyph.
        let plain = t.layout(" DRAW", &s);
        let expected = Icon::Pencil.advance_em() * 10.0;
        assert!((with_icon.width() - plain.width() - expected).abs() < 0.05);
        // ╱ exists in Geist Mono (no icon), but not in Geist (icon).
        assert!(t.layout("╱ LINE", &s).icons.is_empty());
        assert_eq!(t.layout("╱ LINE", &TextStyle::sans(10.0)).icons.len(), 1);
        // Line metrics are not disturbed by the inline box.
        assert!((with_icon.height() - plain.height()).abs() < 0.01);
    }

    #[test]
    fn caret_geometry_and_hit_testing() {
        let mut t = sys();
        let s = TextStyle::sans(12.0);
        let text = "Поиск: ab";
        let l = t.layout(text, &s);
        let mut prev = -1.0;
        let mut idx = 0;
        loop {
            let x = l.caret_rect(idx).x0;
            assert!(x > prev, "caret x must increase: {idx} {x} {prev}");
            prev = x;
            assert_eq!(l.hit_test(Point::new(x + 0.1, 5.0)), idx);
            if idx == text.len() {
                break;
            }
            idx = l.next_boundary(idx);
        }
        assert_eq!(l.prev_boundary(2), 0); // 'П' is two bytes
        assert_eq!(l.next_boundary(0), 2);
        assert_eq!(l.hit_test(Point::new(-50.0, 5.0)), 0);
        assert_eq!(l.hit_test(Point::new(1000.0, 5.0)), text.len());
        let sel = l.selection_rects(0..text.len());
        assert_eq!(sel.len(), 1);
        assert!((sel[0].width() - l.width()).abs() < 0.05);
        assert!(l.selection_rects(3..3).is_empty());
        // Words.
        assert_eq!(l.next_word(0), "Поиск".len());
        assert_eq!(l.prev_word(text.len()), text.len() - 2);
        assert_eq!(l.prev_word(0), 0);
        assert_eq!(l.next_word(text.len()), text.len());
    }

    #[test]
    fn caret_around_icons_maps_to_original_indices() {
        let mut t = sys();
        let text = "a★b";
        let l = t.layout(text, &TextStyle::sans(12.0));
        let star = text.find('★').expect("star");
        let after = star + '★'.len_utf8();
        let x_before = l.caret_rect(star).x0;
        let x_after = l.caret_rect(after).x0;
        assert!(x_after - x_before > Icon::Star.advance_em() * 12.0 * 0.9, "{x_before} {x_after}");
        assert_eq!(l.next_boundary(star), after);
        assert_eq!(l.hit_test(Point::new(x_after + 0.2, 5.0)), after);
        assert_eq!(l.caret_rect(text.len()).x0, l.width());
    }

    #[test]
    fn multiline_hit_test() {
        let mut t = sys();
        let l = t.layout("one\ntwo", &TextStyle::sans(12.0));
        assert_eq!(l.line_count(), 2);
        let y2 = l.line_baseline(1).expect("second line");
        assert_eq!(l.hit_test(Point::new(0.0, y2)), 4);
        assert!(l.caret_rect(5).y0 > l.caret_rect(1).y0);
    }

    #[test]
    fn hostile_styles_do_not_panic() {
        let mut t = sys();
        let weird = TextStyle {
            size: f32::NAN,
            weight: f32::INFINITY,
            letter_spacing: f32::NAN,
            line_height: Some(-1.0),
            max_width: Some(f32::NAN),
            ..TextStyle::default()
        };
        let m = t.measure("x\u{0}\u{FFFF}\u{10FFFF}🙂", &weird);
        assert!(m.width.is_finite() && m.height.is_finite());
        let zero = t.measure("wrap me please", &TextStyle::sans(12.0).wrap(0.0));
        assert!(zero.height.is_finite());
    }
}
