//! The immediate-mode context every section draws through.
//!
//! Painting and hit-testing share one code path: a section's `paint` function computes its
//! geometry, draws it (when a canvas is present) and registers the interactive rectangles with
//! [`Ui::hit`]. For hit-testing, the editor runs the same functions on a measuring-only [`Ui`]
//! (no canvas), so what is drawn and what is clickable can never drift apart.
//!
//! Coordinates are the design's logical pixels (900 × 560). Text is positioned like CSS: a
//! line box of [`theme::line_height`] whose baseline sits [`theme::ascent`] below its top.

use crate::theme;
use pixl_core::{Channel, ChipMode, Focus, Lang, ParamId, WaveTool};
use pixl_gfx::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, RoundedRectRadii, Shape, Stroke};
use pixl_gfx::{Canvas, Color, Cursor, Icon, TextStyle, TextSystem};

/// Something the user can point at. Hit-test results and hover state are expressed in these.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    /// The PIXL wordmark (settings menu).
    Logo,
    /// The preset pill (opens the browser).
    PresetPill,
    /// ‹ (−1) / › (+1) in the preset pill.
    PresetStep(i8),
    /// A NES/GB/FREE segment.
    ChipMode(ChipMode),
    /// The output meter (drag = output gain).
    Meter,
    /// A channel tab.
    Tab(Channel),
    /// A channel tab's enable dot.
    TabDot(Channel),
    /// A drag-value chip of the channel strip (VOL/PAN/ST/CT).
    Strip(ParamId),
    /// The `← CHANNEL` chip of a focus editor.
    Back,
    /// A chip in the hero's top-right row.
    HeroChip(HeroAction),
    /// The hero plot area (wave drawing).
    Plot,
    /// A draggable handle in a hero view.
    Handle(Handle),
    /// A noise period bar (0..16).
    NoisePeriod(u8),
    /// A tile body.
    Tile(Focus),
    /// A tile's enable dot.
    TileDot(Focus),
    /// A macro box (0..4).
    Macro(u8),
    /// The ECHO button label (opens the echo editor).
    EchoButton,
    /// The ECHO button's enable dot.
    EchoDot,
    /// The ASSIGN / DONE button.
    Assign,
    /// A preset-browser control.
    Browser(BrowserTarget),
}

/// What a hero chip does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HeroAction {
    /// Select a wave drawing tool.
    Tool(WaveTool),
    /// Apply a wave operation (SMOOTH, INVERT, SIN, …).
    Wave(WaveOp),
    /// Set an enum/int parameter to a plain value.
    Set(ParamId, f32),
    /// Flip a bool parameter.
    Toggle(ParamId),
    /// Cycle a two-variant enum parameter (EXP ↔ LIN).
    Cycle(ParamId),
    /// A drag-value chip (vertical drag adjusts the parameter).
    Value(ParamId),
    /// Open a focus editor (the channel views' ENV chip).
    Open(Focus),
    /// A non-interactive indicator chip (e.g. `GB STEP`).
    Indicator(u8),
}

/// One-shot wave RAM operations offered by the WAVE view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveOp {
    /// Circular [1 2 1]/4 smoothing.
    Smooth,
    /// Upside down.
    Invert,
    /// Sine preset.
    Sine,
    /// Triangle preset.
    Triangle,
    /// Saw preset.
    Saw,
    /// Square preset.
    Square,
}

/// Draggable handles inside hero views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// Pulse duty edge.
    Duty,
    /// Sweep start dot (drag-y = amount).
    SweepStart,
    /// Sweep end dot (drag-x = time).
    SweepEnd,
    /// Vibrato delay handle (drag-x).
    VibDelay,
    /// Vibrato depth/fade handle (drag-x = fade, drag-y = depth).
    VibDepth,
    /// Glide end handle (drag-x = time).
    GlideEnd,
    /// Envelope attack peak (drag-x).
    EnvAttack,
    /// Envelope decay end (drag-x = decay, drag-y = sustain).
    EnvDecay,
    /// Envelope release end (drag-x).
    EnvRelease,
    /// First echo tap (drag-x = time, drag-y = mix).
    EchoTime,
    /// Second echo tap (drag-y = feedback).
    EchoFeedback,
    /// Crush display (drag-y = bits, drag-x = rate).
    Crush,
}

/// Preset-browser controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BrowserTarget {
    /// The search field.
    Search,
    /// A category row (`None` = all, `Some(None)`… see [`crate::browser::CategoryFilter`]).
    Category(crate::browser::CategoryFilter),
    /// A chip filter chip.
    Chip(ChipMode),
    /// The ★ favourites filter.
    Favorites,
    /// The sort toggle.
    Sort,
    /// A list row (index into the filtered list).
    Row(usize),
    /// The list area (scrolling).
    List,
    /// The scrollbar thumb.
    Scrollbar,
    /// The favourite star in the details panel.
    Star,
    /// The Load button.
    Load,
    /// The ESC button.
    Close,
    /// «Сохранить текущий…».
    SaveOpen,
    /// The save form's name field.
    SaveName,
    /// A category chip in the save form.
    SaveCategory(pixl_core::Category),
    /// The save form's Save button.
    SaveConfirm,
    /// The save form's Cancel button.
    SaveCancel,
    /// «Удалить» for user presets.
    Delete,
    /// The overlay background (swallows clicks).
    Backdrop,
}

/// An interactive rectangle registered during a paint pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    /// Hit rectangle (logical px).
    pub rect: Rect,
    /// What it is.
    pub target: Target,
    /// Cursor to show while hovering it.
    pub cursor: Cursor,
}

enum Surface<'c, 'a> {
    Paint(&'c mut Canvas<'a>),
    Measure(&'c mut TextSystem),
}

/// Painting + hit registration context (see the module docs).
pub struct Ui<'c, 'a> {
    surface: Surface<'c, 'a>,
    hits: Vec<Hit>,
    /// The hovered target (for hover styling).
    pub hover: Option<Target>,
    /// The pressed target (for pressed styling).
    pub pressed: Option<Target>,
    /// UI language.
    pub lang: Lang,
}

impl<'c, 'a> Ui<'c, 'a> {
    /// A context that draws into `canvas`.
    pub fn paint(canvas: &'c mut Canvas<'a>, lang: Lang) -> Self {
        Self { surface: Surface::Paint(canvas), hits: Vec::with_capacity(96), hover: None, pressed: None, lang }
    }

    /// A context that only measures text and collects hits.
    pub fn measure(text: &'c mut TextSystem, lang: Lang) -> Self {
        Self { surface: Surface::Measure(text), hits: Vec::with_capacity(96), hover: None, pressed: None, lang }
    }

    /// Whether drawing calls have an effect (skip expensive geometry when `false`).
    pub fn painting(&self) -> bool {
        matches!(self.surface, Surface::Paint(_))
    }

    /// The hits registered so far, in paint order (later = on top).
    pub fn into_hits(self) -> Vec<Hit> {
        self.hits
    }

    /// Registers an interactive rectangle.
    pub fn hit(&mut self, rect: Rect, target: Target, cursor: Cursor) {
        self.hits.push(Hit { rect, target, cursor });
    }

    /// Whether `target` is hovered.
    pub fn hovered(&self, target: Target) -> bool {
        self.hover == Some(target)
    }

    /// The text system.
    pub fn text_system(&mut self) -> &mut TextSystem {
        match &mut self.surface {
            Surface::Paint(c) => c.text_system(),
            Surface::Measure(t) => t,
        }
    }

    fn canvas(&mut self) -> Option<&mut Canvas<'a>> {
        match &mut self.surface {
            Surface::Paint(c) => Some(c),
            Surface::Measure(_) => None,
        }
    }

    // ----- text -------------------------------------------------------------------------

    /// Advance width of `text` in `style`, CSS style: includes the letter spacing after the
    /// last character.
    pub fn text_width(&mut self, text: &str, style: &TextStyle) -> f64 {
        if text.is_empty() {
            return 0.0;
        }
        self.text_system().measure(text, style).width
    }

    /// Draws `text` with its CSS line box top at `top`; returns the advance width.
    pub fn text(&mut self, x: f64, top: f64, text: &str, style: &TextStyle) -> f64 {
        self.text_baseline(x, top + theme::ascent(style.size), text, style)
    }

    /// Draws `text` with its baseline at `baseline`; returns the advance width.
    pub fn text_baseline(&mut self, x: f64, baseline: f64, text: &str, style: &TextStyle) -> f64 {
        if text.is_empty() {
            return 0.0;
        }
        match &mut self.surface {
            Surface::Paint(c) => c.text_baseline(Point::new(x, baseline), text, style).width,
            Surface::Measure(t) => t.measure(text, style).width,
        }
    }

    /// Draws `text` vertically centred in a line box of `line_h` starting at `top` (the way a
    /// flex item with `align-items: center` places a single line); returns the width.
    pub fn text_centered_in(&mut self, x: f64, top: f64, line_h: f64, text: &str, style: &TextStyle) -> f64 {
        let lh = theme::line_height_of(style);
        self.text(x, top + (line_h - lh) * 0.5, text, style)
    }

    // ----- shapes -----------------------------------------------------------------------

    /// Fills a rectangle.
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        if let Some(c) = self.canvas() {
            c.fill_rect(rect, color);
        }
    }

    /// Fills a rounded rectangle.
    pub fn rrect(&mut self, rect: Rect, radius: f64, color: Color) {
        if let Some(c) = self.canvas() {
            c.fill_rounded_rect(rect, radius, color);
        }
    }

    /// Fills a rounded rectangle with per-corner radii.
    pub fn rrect_radii(&mut self, rect: Rect, radii: RoundedRectRadii, color: Color) {
        if let Some(c) = self.canvas() {
            c.fill_rounded_rect_radii(rect, radii, color);
        }
    }

    /// A CSS border of `width` drawn inside `rect` with outer corner `radius`.
    pub fn border(&mut self, rect: Rect, radius: f64, width: f64, color: Color) {
        if let Some(c) = self.canvas() {
            c.border(rect, radius, width, color);
        }
    }

    /// A CSS box: background fill plus an optional inside border.
    pub fn css_box(&mut self, rect: Rect, radius: f64, bg: Option<Color>, border: Option<(f64, Color)>) {
        if let Some(bg) = bg {
            self.rrect(rect, radius, bg);
        }
        if let Some((w, color)) = border {
            self.border(rect, radius, w, color);
        }
    }

    /// A CSS `dashed` border (WebKit pattern: dashes and gaps of 3 × width, corners solid).
    pub fn dashed_border(&mut self, rect: Rect, radius: f64, width: f64, color: Color) {
        if !self.painting() {
            return;
        }
        let inner = rect.inset(-width * 0.5);
        let r = (radius - width * 0.5).max(0.0);
        let path = RoundedRect::from_rect(inner, r).to_path(0.05);
        let dash = 3.0 * width;
        let stroke = Stroke::new(width).with_dashes(dash * 0.5, [dash, dash]);
        if let Some(c) = self.canvas() {
            c.stroke(&path, &stroke, color);
        }
    }

    /// Strokes any shape.
    pub fn stroke(&mut self, shape: &impl Shape, stroke: &Stroke, color: Color) {
        if let Some(c) = self.canvas() {
            c.stroke(shape, stroke, color);
        }
    }

    /// Fills any shape.
    pub fn fill(&mut self, shape: &impl Shape, color: Color) {
        if let Some(c) = self.canvas() {
            c.fill(shape, color);
        }
    }

    /// Fills a circle.
    pub fn circle(&mut self, center: Point, radius: f64, color: Color) {
        if let Some(c) = self.canvas() {
            c.fill_circle(center, radius, color);
        }
    }

    /// A soft rounded-rect shadow.
    pub fn shadow(&mut self, rect: Rect, radius: f64, blur: f64, color: Color) {
        if let Some(c) = self.canvas() {
            c.shadow(rect, radius, blur, color);
        }
    }

    /// Draws `icon` centred at `center`, sized like text of `size` px at CSS `weight`.
    pub fn icon(&mut self, icon: Icon, center: Point, size: f64, weight: f32, color: Color) {
        if let Some(c) = self.canvas() {
            c.icon_weighted(icon, center, size, weight, color);
        }
    }

    /// Fills `path` (local coordinates) translated by `offset`.
    pub fn fill_path_at(&mut self, path: &BezPath, offset: Point, color: Color) {
        if path.elements().is_empty() {
            return;
        }
        if let Some(c) = self.canvas() {
            c.with_transform(Affine::translate(offset.to_vec2()), |c| c.fill_path(path, color));
        }
    }

    /// Strokes `path` (local coordinates) translated by `offset`.
    pub fn stroke_path_at(&mut self, path: &BezPath, offset: Point, stroke: &Stroke, color: Color) {
        if path.elements().is_empty() {
            return;
        }
        if let Some(c) = self.canvas() {
            c.with_transform(Affine::translate(offset.to_vec2()), |c| c.stroke_path(path, stroke, color));
        }
    }

    /// Clips subsequent drawing to `shape` until [`Ui::pop`].
    pub fn push_clip(&mut self, shape: &impl Shape) {
        if let Some(c) = self.canvas() {
            c.push_clip(shape);
        }
    }

    /// Composites subsequent drawing at `alpha` until [`Ui::pop`].
    pub fn push_opacity(&mut self, alpha: f32) {
        if let Some(c) = self.canvas() {
            c.push_opacity(alpha);
        }
    }

    /// Pops the last clip / opacity layer.
    pub fn pop(&mut self) {
        if let Some(c) = self.canvas() {
            c.pop_layer();
        }
    }
}

/// Shorthand for a rectangle from origin and size.
pub fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect::new(x, y, x + w, y + h)
}
