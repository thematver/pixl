//! Frame building (App → vello scene) and GPU rendering shared by views and offscreen
//! rendering.

use crate::app::App;
use crate::canvas::Canvas;
use crate::gpu::GpuContext;
use crate::text::TextSystem;
use crate::GfxError;
use vello::kurbo::{Affine, Size, Vec2};
use vello::peniko::Color;
use vello::{wgpu, Scene};

/// Largest render target edge accepted (pixels); bigger requests are clamped.
pub(crate) const MAX_TARGET_EDGE: u32 = 8192;

/// How logical content maps onto a target of a given size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    /// Logical content size (sanitized `App::size`).
    pub content: Size,
    /// Uniform zoom from logical points to view points.
    pub zoom: f64,
    /// Offset of the content's top-left inside the view (view points, letterboxing).
    pub offset: Vec2,
    /// Device pixels per view point (backing scale).
    pub backing: f64,
}

impl Placement {
    /// Fits `content` into a view of `view` points with `backing` pixels per point.
    pub fn fit(content: Size, view: Size, backing: f64) -> Placement {
        let content = sanitize_size(content);
        let view = sanitize_size(view);
        let backing = if backing.is_finite() && backing > 0.0 { backing.clamp(0.25, 8.0) } else { 1.0 };
        let zoom = (view.width / content.width).min(view.height / content.height);
        let zoom = if zoom.is_finite() && zoom > 0.0 { zoom } else { 1.0 };
        let offset =
            Vec2::new((view.width - content.width * zoom) * 0.5, (view.height - content.height * zoom) * 0.5);
        Placement { content, zoom, offset, backing }
    }

    /// Logical → device pixel transform.
    pub fn to_device(&self) -> Affine {
        Affine::scale(self.backing) * Affine::translate(self.offset) * Affine::scale(self.zoom)
    }

    /// Device pixels per logical point.
    pub fn scale_factor(&self) -> f64 {
        self.backing * self.zoom
    }

    /// Converts a point in view coordinates (points, flipped) to logical coordinates.
    pub fn view_to_logical(&self, p: vello::kurbo::Point) -> vello::kurbo::Point {
        ((p.to_vec2() - self.offset) / self.zoom).to_point()
    }
}

fn sanitize_size(s: Size) -> Size {
    let f = |v: f64| if v.is_finite() && v >= 1.0 { v.min(100_000.0) } else { 1.0 };
    Size::new(f(s.width), f(s.height))
}

/// Reusable per-target frame state: the scene and canvas scratch buffers.
#[derive(Default)]
pub(crate) struct FrameBuilder {
    scene: Scene,
    stack: Vec<vello::kurbo::Affine>,
}

impl FrameBuilder {
    /// Runs `app.frame` into the internal scene. Returns the clear colour.
    pub fn build(&mut self, app: &mut dyn App, text: &mut TextSystem, placement: &Placement, dt: f64) -> Color {
        self.scene.reset();
        let dt = if dt.is_finite() { dt.clamp(0.0, 0.1) } else { 0.0 };
        {
            let mut canvas = Canvas::new(
                &mut self.scene,
                text,
                &mut self.stack,
                placement.to_device(),
                placement.content,
                placement.scale_factor(),
            );
            app.frame(&mut canvas, dt);
            canvas.finish();
        }
        text.end_frame();
        app.background()
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }
}

/// An `Rgba8Unorm` storage texture vello renders into.
pub(crate) struct RenderTarget {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    generation: u64,
}

impl RenderTarget {
    pub fn new(gpu: &GpuContext, width: u32, height: u32, extra_usage: wgpu::TextureUsages) -> RenderTarget {
        let (width, height) = (width.clamp(1, MAX_TARGET_EDGE), height.clamp(1, MAX_TARGET_EDGE));
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pixl-gfx target"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | extra_usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        RenderTarget { texture, view, width, height, generation: gpu.generation() }
    }

    /// Whether this target can be reused for `width × height` on `gpu`.
    pub fn matches(&self, gpu: &GpuContext, width: u32, height: u32) -> bool {
        self.generation == gpu.generation()
            && self.width == width.clamp(1, MAX_TARGET_EDGE)
            && self.height == height.clamp(1, MAX_TARGET_EDGE)
    }
}

/// Renders `scene` into `target` with `background` as the clear colour.
pub(crate) fn render_scene(
    gpu: &GpuContext,
    scene: &Scene,
    target: &RenderTarget,
    background: Color,
) -> Result<(), GfxError> {
    let mut renderer = gpu.renderer();
    renderer
        .render_to_texture(
            &gpu.device,
            &gpu.queue,
            scene,
            &target.view,
            &vello::RenderParams {
                base_color: background,
                width: target.width,
                height: target.height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
        .map_err(|e| GfxError::Render(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Point;

    #[test]
    fn placement_fits_and_letterboxes() {
        let p = Placement::fit(Size::new(900.0, 560.0), Size::new(1350.0, 840.0), 2.0);
        assert!((p.zoom - 1.5).abs() < 1e-12);
        assert_eq!(p.offset, Vec2::ZERO);
        assert!((p.scale_factor() - 3.0).abs() < 1e-12);
        let q = Placement::fit(Size::new(900.0, 560.0), Size::new(1000.0, 560.0), 1.0);
        assert!((q.zoom - 1.0).abs() < 1e-12);
        assert!((q.offset.x - 50.0).abs() < 1e-12);
        assert_eq!(q.view_to_logical(Point::new(50.0, 10.0)), Point::new(0.0, 10.0));
        let d = q.to_device() * Point::new(0.0, 0.0);
        assert!((d.x - 50.0).abs() < 1e-12);
    }

    #[test]
    fn placement_survives_garbage() {
        let p = Placement::fit(Size::new(f64::NAN, 0.0), Size::new(-5.0, f64::INFINITY), f64::NAN);
        assert!(p.zoom.is_finite() && p.zoom > 0.0);
        assert!(p.offset.x.is_finite() && p.offset.y.is_finite());
        assert_eq!(p.backing, 1.0);
    }
}
