//! The immediate-mode drawing API handed to [`App::frame`](crate::App::frame).

use crate::icons::Icon;
use crate::text::{TextAlign, TextLayout, TextMetrics, TextStyle, TextSystem};
use std::sync::Arc;
use vello::kurbo::{
    Affine, BezPath, Cap, Circle, Line, Point, Rect, RoundedRect, RoundedRectRadii, Shape, Size, Stroke, Vec2,
};
use vello::peniko::{BlendMode, BrushRef, Color, Fill, Mix};
use vello::Scene;

/// Immediate-mode drawing surface for one frame.
///
/// All coordinates are **logical points** (the design's CSS pixels): the canvas maps them to
/// device pixels (Retina backing scale × UI zoom) itself. Colours are sRGB with straight
/// alpha; anything that converts into a [`BrushRef`] (a [`Color`], a
/// [`Gradient`](vello::peniko::Gradient)) can be used as paint.
///
/// State is a stack: [`Canvas::save`]/[`Canvas::restore`] for transforms and
/// [`Canvas::push_clip`]/[`Canvas::push_opacity`]/[`Canvas::pop_layer`] for layers. The
/// closure helpers ([`Canvas::with_transform`], [`Canvas::with_clip`],
/// [`Canvas::with_opacity`]) keep pushes and pops balanced automatically. Anything left
/// unbalanced at the end of the frame is popped by the renderer.
pub struct Canvas<'a> {
    scene: &'a mut Scene,
    text: &'a mut TextSystem,
    stack: &'a mut Vec<Affine>,
    /// Logical → device transform of the frame (scale and letterbox offset).
    base: Affine,
    /// Current user transform (logical).
    transform: Affine,
    layers: u32,
    size: Size,
    scale: f64,
}

impl<'a> Canvas<'a> {
    pub(crate) fn new(
        scene: &'a mut Scene,
        text: &'a mut TextSystem,
        stack: &'a mut Vec<Affine>,
        base: Affine,
        size: Size,
        scale: f64,
    ) -> Self {
        stack.clear();
        Self { scene, text, stack, base, transform: Affine::IDENTITY, layers: 0, size, scale }
    }

    /// Logical size of the content (what [`App::size`](crate::App::size) returned).
    pub fn size(&self) -> Size {
        self.size
    }

    /// Logical bounds `(0, 0, width, height)`.
    pub fn bounds(&self) -> Rect {
        self.size.to_rect()
    }

    /// Device pixels per logical point (backing scale × UI zoom), e.g. `2.0` on Retina at
    /// 100 %. Useful to snap hairlines: `1.0 / canvas.scale_factor()` is one device pixel.
    pub fn scale_factor(&self) -> f64 {
        self.scale
    }

    /// The text system (measurement, cached layouts).
    pub fn text_system(&mut self) -> &mut TextSystem {
        self.text
    }

    // ----- transforms -----------------------------------------------------------------

    /// Current user transform (logical coordinates, not including the device scale).
    pub fn transform(&self) -> Affine {
        self.transform
    }

    /// Replaces the current user transform.
    pub fn set_transform(&mut self, transform: Affine) {
        if transform.is_finite() {
            self.transform = transform;
        }
    }

    /// Pre-multiplies the current transform by `transform` (applies it in local space).
    pub fn apply(&mut self, transform: Affine) {
        let t = self.transform * transform;
        self.set_transform(t);
    }

    /// Translates the local coordinate system.
    pub fn translate(&mut self, offset: impl Into<Vec2>) {
        self.apply(Affine::translate(offset.into()));
    }

    /// Scales the local coordinate system uniformly.
    pub fn scale(&mut self, factor: f64) {
        self.apply(Affine::scale(factor));
    }

    /// Pushes the current transform.
    pub fn save(&mut self) {
        if self.stack.len() < 1024 {
            self.stack.push(self.transform);
        }
    }

    /// Pops the transform pushed by [`Canvas::save`] (no-op if unbalanced).
    pub fn restore(&mut self) {
        if let Some(t) = self.stack.pop() {
            self.transform = t;
        }
    }

    /// Runs `f` with `transform` applied, restoring the previous transform afterwards.
    pub fn with_transform<R>(&mut self, transform: Affine, f: impl FnOnce(&mut Self) -> R) -> R {
        let saved = self.transform;
        self.apply(transform);
        let r = f(self);
        self.transform = saved;
        r
    }

    fn device(&self) -> Affine {
        self.base * self.transform
    }

    // ----- layers -----------------------------------------------------------------------

    /// Clips subsequent drawing to `shape` (in current local coordinates) until
    /// [`Canvas::pop_layer`].
    pub fn push_clip(&mut self, shape: &impl Shape) {
        self.scene.push_clip_layer(Fill::NonZero, self.device(), shape);
        self.layers += 1;
    }

    /// Draws subsequent content into a layer composited with `alpha` (0..1) until
    /// [`Canvas::pop_layer`].
    pub fn push_opacity(&mut self, alpha: f32) {
        let full = Rect::new(-1.0e5, -1.0e5, 1.0e5, 1.0e5);
        self.scene.push_layer(Fill::NonZero, BlendMode::from(Mix::Normal), sanitize_alpha(alpha), self.base, &full);
        self.layers += 1;
    }

    /// Like [`Canvas::push_opacity`], but the layer is also clipped to `shape` (cheaper for
    /// small elements).
    pub fn push_opacity_clipped(&mut self, alpha: f32, shape: &impl Shape) {
        self.scene.push_layer(Fill::NonZero, BlendMode::from(Mix::Normal), sanitize_alpha(alpha), self.device(), shape);
        self.layers += 1;
    }

    /// Pops the most recent clip / opacity layer (no-op if none is open).
    pub fn pop_layer(&mut self) {
        if self.layers > 0 {
            self.scene.pop_layer();
            self.layers -= 1;
        }
    }

    /// Alias of [`Canvas::pop_layer`] for readability after [`Canvas::push_clip`].
    pub fn pop_clip(&mut self) {
        self.pop_layer();
    }

    /// Runs `f` with drawing clipped to `shape`.
    pub fn with_clip<R>(&mut self, shape: &impl Shape, f: impl FnOnce(&mut Self) -> R) -> R {
        let depth = self.layers;
        self.push_clip(shape);
        let r = f(self);
        while self.layers > depth {
            self.pop_layer();
        }
        r
    }

    /// Runs `f` with everything it draws composited at `alpha`. Skips `f` entirely when
    /// `alpha` is (almost) zero and avoids the layer when it is (almost) one.
    pub fn with_opacity(&mut self, alpha: f32, f: impl FnOnce(&mut Self)) {
        let alpha = sanitize_alpha(alpha);
        if alpha <= 1.0 / 512.0 {
            return;
        }
        if alpha >= 1.0 - 1.0 / 512.0 {
            f(self);
            return;
        }
        let depth = self.layers;
        self.push_opacity(alpha);
        f(self);
        while self.layers > depth {
            self.pop_layer();
        }
    }

    /// Number of open layers (for debugging).
    pub fn layer_depth(&self) -> u32 {
        self.layers
    }

    pub(crate) fn finish(&mut self) {
        if self.layers > 0 {
            log::debug!("pixl-gfx: {} unbalanced canvas layer(s) closed at end of frame", self.layers);
        }
        while self.layers > 0 {
            self.pop_layer();
        }
    }

    // ----- shapes -------------------------------------------------------------------------

    /// Fills any kurbo shape (non-zero rule).
    pub fn fill<'b>(&mut self, shape: &impl Shape, paint: impl Into<BrushRef<'b>>) {
        self.scene.fill(Fill::NonZero, self.device(), paint, None, shape);
    }

    /// Fills any kurbo shape with the even-odd rule (shapes with holes).
    pub fn fill_even_odd<'b>(&mut self, shape: &impl Shape, paint: impl Into<BrushRef<'b>>) {
        self.scene.fill(Fill::EvenOdd, self.device(), paint, None, shape);
    }

    /// Strokes any kurbo shape. `stroke` controls width (logical px), joins, caps and dashes:
    ///
    /// ```
    /// use pixl_gfx::kurbo::{Join, Stroke};
    /// let dashed = Stroke::new(1.5).with_dashes(0.0, [4.0, 3.0]).with_join(Join::Round);
    /// assert_eq!(dashed.dash_pattern.len(), 2);
    /// ```
    pub fn stroke<'b>(&mut self, shape: &impl Shape, stroke: &Stroke, paint: impl Into<BrushRef<'b>>) {
        if stroke.width > 0.0 && stroke.width.is_finite() {
            self.scene.stroke(stroke, self.device(), paint, None, shape);
        }
    }

    /// Fills a rectangle.
    pub fn fill_rect<'b>(&mut self, rect: Rect, paint: impl Into<BrushRef<'b>>) {
        self.fill(&rect, paint);
    }

    /// Strokes a rectangle centred on its edges (half the width falls outside).
    pub fn stroke_rect<'b>(&mut self, rect: Rect, width: f64, paint: impl Into<BrushRef<'b>>) {
        self.stroke(&rect, &Stroke::new(width), paint);
    }

    /// Fills a rounded rectangle (uniform radius; clamped to half the shorter side).
    pub fn fill_rounded_rect<'b>(&mut self, rect: Rect, radius: f64, paint: impl Into<BrushRef<'b>>) {
        self.fill(&rounded(rect, radius), paint);
    }

    /// Fills a rounded rectangle with per-corner radii.
    pub fn fill_rounded_rect_radii<'b>(
        &mut self,
        rect: Rect,
        radii: RoundedRectRadii,
        paint: impl Into<BrushRef<'b>>,
    ) {
        self.fill(&RoundedRect::from_rect(rect.abs(), radii), paint);
    }

    /// Strokes a rounded rectangle centred on its outline.
    pub fn stroke_rounded_rect<'b>(&mut self, rect: Rect, radius: f64, width: f64, paint: impl Into<BrushRef<'b>>) {
        self.stroke(&rounded(rect, radius), &Stroke::new(width), paint);
    }

    /// Draws a CSS-style border: a stroke of `width` lying entirely *inside* `rect`
    /// (`box-sizing: border-box`), with outer corner `radius`. Pixel-exact for integer
    /// rectangles and widths, like the browser.
    pub fn border<'b>(&mut self, rect: Rect, radius: f64, width: f64, paint: impl Into<BrushRef<'b>>) {
        if !(width > 0.0) {
            return;
        }
        let rect = rect.abs();
        let w = width.min(rect.width() * 0.5).min(rect.height() * 0.5);
        let inner = rect.inset(-w * 0.5);
        self.stroke(&rounded(inner, (radius - w * 0.5).max(0.0)), &Stroke::new(w), paint);
    }

    /// Fills a circle.
    pub fn fill_circle<'b>(&mut self, center: impl Into<Point>, radius: f64, paint: impl Into<BrushRef<'b>>) {
        self.fill(&Circle::new(center, radius.max(0.0)), paint);
    }

    /// Strokes a circle centred on its outline.
    pub fn stroke_circle<'b>(
        &mut self,
        center: impl Into<Point>,
        radius: f64,
        width: f64,
        paint: impl Into<BrushRef<'b>>,
    ) {
        self.stroke(&Circle::new(center, radius.max(0.0)), &Stroke::new(width), paint);
    }

    /// Draws a straight line with butt caps.
    pub fn line<'b>(&mut self, p0: impl Into<Point>, p1: impl Into<Point>, width: f64, paint: impl Into<BrushRef<'b>>) {
        self.stroke(&Line::new(p0, p1), &Stroke::new(width).with_caps(Cap::Butt), paint);
    }

    /// Fills a path (non-zero rule).
    pub fn fill_path<'b>(&mut self, path: &BezPath, paint: impl Into<BrushRef<'b>>) {
        self.fill(path, paint);
    }

    /// Strokes a path with `stroke` (width, joins, caps, dashes).
    pub fn stroke_path<'b>(&mut self, path: &BezPath, stroke: &Stroke, paint: impl Into<BrushRef<'b>>) {
        self.stroke(path, stroke, paint);
    }

    /// Draws a soft drop shadow for a rounded rectangle (`blur` = Gaussian standard
    /// deviation in logical px, like CSS `box-shadow` blur / 2).
    pub fn shadow(&mut self, rect: Rect, radius: f64, blur: f64, color: Color) {
        if blur.is_finite() && blur >= 0.0 {
            self.scene.draw_blurred_rounded_rect(self.device(), rect.abs(), color, radius.max(0.0), blur.max(0.01));
        }
    }

    // ----- text -----------------------------------------------------------------------

    /// Shapes `text` (cached) without drawing it.
    pub fn layout(&mut self, text: &str, style: &TextStyle) -> Arc<TextLayout> {
        self.text.layout(text, style)
    }

    /// Measures `text`.
    pub fn measure(&mut self, text: &str, style: &TextStyle) -> TextMetrics {
        self.text.measure(text, style)
    }

    /// Draws `text` anchored at `pos`: `pos.y` is the **top of the first line box** (CSS),
    /// `pos.x` is the left edge, centre or right edge of the text box depending on
    /// `style.align` (the box is `style.max_width` wide when set, else the text width).
    pub fn text(&mut self, pos: impl Into<Point>, text: &str, style: &TextStyle) -> TextMetrics {
        let layout = self.text.layout(text, style);
        let pos = pos.into();
        let x = pos.x - anchor_offset(style.align, layout.box_width());
        self.draw_layout(&layout, Point::new(x, pos.y), style.color);
        layout.metrics()
    }

    /// Like [`Canvas::text`], but `pos.y` is the **baseline** of the first line.
    pub fn text_baseline(&mut self, pos: impl Into<Point>, text: &str, style: &TextStyle) -> TextMetrics {
        let layout = self.text.layout(text, style);
        let pos = pos.into();
        let x = pos.x - anchor_offset(style.align, layout.box_width());
        self.draw_layout(&layout, Point::new(x, pos.y - layout.metrics().baseline), style.color);
        layout.metrics()
    }

    /// Draws `text` inside `rect`: horizontally per `style.align` (relative to the rect's
    /// left edge, centre or right edge), vertically centred the way CSS
    /// `display:flex; align-items:center` centres a line box. Set `style.max_width` (e.g.
    /// with [`TextStyle::wrap`]) to wrap or truncate within the rect.
    pub fn text_in(&mut self, rect: Rect, text: &str, style: &TextStyle) -> TextMetrics {
        let rect = rect.abs();
        let layout = self.text.layout(text, style);
        let x = match style.align {
            TextAlign::Left => rect.x0,
            TextAlign::Center => rect.center().x - layout.box_width() * 0.5,
            TextAlign::Right => rect.x1 - layout.box_width(),
        };
        let y = rect.center().y - layout.height() * 0.5;
        self.draw_layout(&layout, Point::new(x, y), style.color);
        layout.metrics()
    }

    /// Draws a pre-shaped layout with its box's top-left corner at `top_left`.
    pub fn draw_layout<'b>(&mut self, layout: &TextLayout, top_left: impl Into<Point>, paint: impl Into<BrushRef<'b>>) {
        let t = self.device() * Affine::translate(top_left.into().to_vec2());
        layout.encode(self.scene, t, paint.into());
    }

    // ----- icons ----------------------------------------------------------------------

    /// Draws `icon` with its visual bounds centred on `center`, sized like text of font
    /// size `size`, stroked at weight 500.
    pub fn icon<'b>(&mut self, icon: Icon, center: impl Into<Point>, size: f64, paint: impl Into<BrushRef<'b>>) {
        self.icon_weighted(icon, center, size, 500.0, paint);
    }

    /// Like [`Canvas::icon`] with an explicit CSS weight (controls stroke thickness).
    pub fn icon_weighted<'b>(
        &mut self,
        icon: Icon,
        center: impl Into<Point>,
        size: f64,
        weight: f32,
        paint: impl Into<BrushRef<'b>>,
    ) {
        if !(size > 0.0 && size.is_finite()) {
            return;
        }
        let sw = Icon::stroke_width_em(weight);
        let b = icon.bounds_em(sw);
        let center = center.into();
        let t = self.device() * Affine::translate(center.to_vec2()) * Affine::scale(size) * Affine::translate(-b.center().to_vec2());
        icon.encode(self.scene, t, sw, paint.into());
    }

    /// Draws `icon` scaled to fit inside `rect` (aspect preserved, centred).
    pub fn icon_in<'b>(&mut self, icon: Icon, rect: Rect, paint: impl Into<BrushRef<'b>>) {
        let rect = rect.abs();
        let sw = Icon::stroke_width_em(500.0);
        let b = icon.bounds_em(sw);
        if b.width() <= 0.0 || b.height() <= 0.0 {
            return;
        }
        let size = (rect.width() / b.width()).min(rect.height() / b.height());
        self.icon(icon, rect.center(), size, paint);
    }
}

fn anchor_offset(align: TextAlign, box_width: f64) -> f64 {
    match align {
        TextAlign::Left => 0.0,
        TextAlign::Center => box_width * 0.5,
        TextAlign::Right => box_width,
    }
}

fn rounded(rect: Rect, radius: f64) -> RoundedRect {
    let rect = rect.abs();
    let r = if radius.is_finite() { radius.max(0.0) } else { 0.0 };
    let r = r.min(rect.width() * 0.5).min(rect.height() * 0.5);
    RoundedRect::from_rect(rect, r)
}

fn sanitize_alpha(a: f32) -> f32 {
    if a.is_finite() {
        a.clamp(0.0, 1.0)
    } else {
        1.0
    }
}
