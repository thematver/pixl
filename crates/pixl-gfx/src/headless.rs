//! Offscreen rendering and synthetic events, for golden screenshots and UI tests.
//!
//! ```no_run
//! use pixl_gfx::{App, Canvas, Event, EventCtx, Headless, kurbo::Size};
//! struct Hello;
//! impl App for Hello {
//!     fn size(&self) -> Size { Size::new(200.0, 80.0) }
//!     fn event(&mut self, _: &Event, _: &mut EventCtx) {}
//!     fn frame(&mut self, c: &mut Canvas, _: f64) {
//!         c.text((20.0, 20.0), "Привет", &pixl_gfx::TextStyle::sans(24.0));
//!     }
//! }
//! let mut headless = Headless::new()?;
//! let image = headless.render(&mut Hello, 2.0)?; // 400 × 160 pixels
//! assert_eq!(image.width, 400);
//! # Ok::<(), pixl_gfx::GfxError>(())
//! ```

use crate::app::{App, Effects, EventCtx};
use crate::event::Event;
use crate::gpu::GpuContext;
use crate::render::{render_scene, FrameBuilder, Placement, RenderTarget};
use crate::text::TextSystem;
use crate::GfxError;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vello::wgpu;

/// How long offscreen rendering waits for the shared GPU context to be created.
const GPU_TIMEOUT: Duration = Duration::from_secs(60);

/// An RGBA8 image (sRGB, straight alpha, rows top to bottom).
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes.
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image").field("width", &self.width).field("height", &self.height).finish_non_exhaustive()
    }
}

impl Image {
    /// The pixel at `(x, y)` as `[r, g, b, a]`, or `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y as usize) * (self.width as usize) + x as usize) * 4;
        self.pixels.get(i..i + 4).and_then(|p| p.try_into().ok())
    }

    /// Encodes the image as PNG.
    #[cfg(feature = "png")]
    pub fn save_png(&self, path: impl AsRef<std::path::Path>) -> Result<(), GfxError> {
        let path = path.as_ref();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| GfxError::Io(e.to_string()))?;
        }
        let file = std::fs::File::create(path).map_err(|e| GfxError::Io(format!("{}: {e}", path.display())))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header().map_err(|e| GfxError::Io(e.to_string()))?;
        writer.write_image_data(&self.pixels).map_err(|e| GfxError::Io(e.to_string()))?;
        writer.finish().map_err(|e| GfxError::Io(e.to_string()))
    }
}

/// Timing of one offscreen frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameTiming {
    /// CPU time to run [`App::frame`] and encode the scene (text shaping included).
    pub encode: Duration,
    /// Time from submitting the scene to the GPU finishing it (vello render).
    pub render: Duration,
}

impl FrameTiming {
    /// `encode + render`.
    pub fn total(&self) -> Duration {
        self.encode + self.render
    }
}

/// An offscreen renderer with its own text system. Shares the process-wide GPU context.
pub struct Headless {
    gpu: Arc<GpuContext>,
    text: TextSystem,
    builder: FrameBuilder,
    target: Option<RenderTarget>,
    text_focused: bool,
    infinite_drag: bool,
}

impl std::fmt::Debug for Headless {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Headless").field("gpu", &self.gpu).finish_non_exhaustive()
    }
}

impl Headless {
    /// Creates an offscreen renderer (blocks until the shared GPU context exists).
    pub fn new() -> Result<Headless, GfxError> {
        let gpu = GpuContext::get_blocking(GPU_TIMEOUT)?;
        Ok(Headless {
            gpu,
            text: TextSystem::new(),
            builder: FrameBuilder::default(),
            target: None,
            text_focused: false,
            infinite_drag: false,
        })
    }

    /// The GPU context in use.
    pub fn gpu(&self) -> &Arc<GpuContext> {
        &self.gpu
    }

    /// The text system used for frames and events.
    pub fn text(&mut self) -> &mut TextSystem {
        &mut self.text
    }

    /// Delivers `event` to `app` like a view would (with a 2× scale factor and no text
    /// focus initially) and returns what the handler requested. Focus and drag requests
    /// persist between events, just as they do in a native view.
    pub fn event(&mut self, app: &mut dyn App, event: &Event) -> Effects {
        if matches!(event, Event::TextFocusLost) {
            self.text_focused = false;
        }
        let mut ctx = EventCtx::new(&mut self.text, 2.0, self.text_focused, self.infinite_drag);
        app.event(event, &mut ctx);
        let effects = ctx.into_effects();
        if let Some(focus) = effects.text_focus {
            self.text_focused = focus;
        }
        if let Some(drag) = effects.infinite_drag {
            self.infinite_drag = drag;
        }
        if matches!(event, Event::MouseUp(_)) {
            self.infinite_drag = false;
        }
        effects
    }

    /// Renders one frame of `app` at `scale` device pixels per logical point (2.0 = Retina)
    /// and reads it back.
    pub fn render(&mut self, app: &mut dyn App, scale: f64) -> Result<Image, GfxError> {
        self.refresh_gpu()?;
        let (placement, w, h) = self.placement(app, scale);
        let background = self.builder.build(app, &mut self.text, &placement, 0.0);
        self.ensure_target(w, h);
        let target = self.target.as_ref().ok_or_else(|| GfxError::Render("missing render target".into()))?;
        render_scene(&self.gpu, self.builder.scene(), target, background)?;
        read_back(&self.gpu, target)
    }

    /// Renders one frame without reading it back and reports how long encoding and GPU
    /// rendering took (waits for the GPU to finish).
    pub fn time_frame(&mut self, app: &mut dyn App, scale: f64, dt: f64) -> Result<FrameTiming, GfxError> {
        self.refresh_gpu()?;
        let (placement, w, h) = self.placement(app, scale);
        let t0 = Instant::now();
        let background = self.builder.build(app, &mut self.text, &placement, dt);
        let encode = t0.elapsed();
        self.ensure_target(w, h);
        let target = self.target.as_ref().ok_or_else(|| GfxError::Render("missing render target".into()))?;
        let t1 = Instant::now();
        render_scene(&self.gpu, self.builder.scene(), target, background)?;
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GfxError::Render(format!("GPU wait failed: {e}")))?;
        Ok(FrameTiming { encode, render: t1.elapsed() })
    }

    fn refresh_gpu(&mut self) -> Result<(), GfxError> {
        if self.gpu.is_lost() {
            self.gpu = GpuContext::get_blocking(GPU_TIMEOUT)?;
            self.target = None;
        }
        Ok(())
    }

    fn placement(&self, app: &dyn App, scale: f64) -> (Placement, u32, u32) {
        let scale = if scale.is_finite() && scale > 0.0 { scale.clamp(0.25, 8.0) } else { 1.0 };
        let size = app.size();
        let placement = Placement::fit(size, size, scale);
        let w = (placement.content.width * scale).round().max(1.0) as u32;
        let h = (placement.content.height * scale).round().max(1.0) as u32;
        (placement, w, h)
    }

    fn ensure_target(&mut self, w: u32, h: u32) {
        let gpu = &self.gpu;
        if !self.target.as_ref().is_some_and(|t| t.matches(gpu, w, h)) {
            self.target = Some(RenderTarget::new(gpu, w, h, wgpu::TextureUsages::COPY_SRC));
        }
    }
}

fn read_back(gpu: &GpuContext, target: &RenderTarget) -> Result<Image, GfxError> {
    let (w, h) = (target.width, target.height);
    let unpadded = w as usize * 4;
    let padded =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pixl-gfx readback"),
        size: (padded * h as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
    encoder.copy_texture_to_buffer(
        target.texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded as u32), rows_per_image: None },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    gpu.queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GfxError::Render(format!("GPU wait failed: {e}")))?;
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(GfxError::Render(format!("readback mapping failed: {e}"))),
        Err(_) => return Err(GfxError::Render("readback mapping timed out".into())),
    }
    let mut pixels = Vec::with_capacity(unpadded * h as usize);
    {
        let data = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| GfxError::Render(format!("readback mapping failed: {e}")))?;
        for row in 0..h as usize {
            let start = row * padded;
            pixels.extend_from_slice(data.get(start..start + unpadded).unwrap_or(&[]));
        }
    }
    buffer.unmap();
    if pixels.len() != unpadded * h as usize {
        return Err(GfxError::Render("readback size mismatch".into()));
    }
    Ok(Image { width: w, height: h, pixels })
}

/// Renders one frame of `app` offscreen at `scale` and returns the pixels.
pub fn render_to_image(app: &mut dyn App, scale: f64) -> Result<Image, GfxError> {
    Headless::new()?.render(app, scale)
}

/// Renders one frame of `app` offscreen at `scale` (2.0 = Retina) and writes a PNG to
/// `path` (parent directories are created). No window or main thread needed.
#[cfg(feature = "png")]
pub fn render_to_png(app: &mut dyn App, scale: f64, path: impl AsRef<std::path::Path>) -> Result<(), GfxError> {
    render_to_image(app, scale)?.save_png(path)
}
