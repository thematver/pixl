use parley::{FontContext, LayoutContext, PositionedLayoutItem, StyleProperty, FontWeight, FontFamily};
use std::sync::Arc;
use vello::kurbo::{Affine, RoundedRect, Stroke, BezPath};
use vello::peniko::{Color, Fill, Blob};
use vello::wgpu;

fn main() {
    let (w, h) = (900u32 * 2, 560u32 * 2);
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).expect("adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let t0 = std::time::Instant::now();
    let mut renderer = vello::Renderer::new(&device, vello::RendererOptions {
        use_cpu: false, antialiasing_support: vello::AaSupport::area_only(), num_init_threads: std::num::NonZeroUsize::new(1), pipeline_cache: None,
    }).expect("renderer");
    eprintln!("renderer init {:?}", t0.elapsed());

    let mut fcx = FontContext::new();
    for f in ["Geist-Regular", "Geist-SemiBold", "Geist-Bold", "GeistMono-Medium"] {
        let bytes = std::fs::read(format!("{}/assets/fonts/{f}.ttf", env!("CARGO_MANIFEST_DIR"))).unwrap();
        fcx.collection.register_fonts(Blob::new(Arc::new(bytes)), None);
    }
    #[derive(Clone, Copy, PartialEq, Default, Debug)] struct B;
    let mut lcx: LayoutContext<B> = LayoutContext::new();
    let mut scene = vello::Scene::new();
    let s = Affine::scale(2.0);
    scene.fill(Fill::NonZero, s, Color::from_rgb8(0xF7, 0xF6, 0xF3), None, &RoundedRect::new(0.0, 0.0, 900.0, 560.0, 14.0));
    scene.fill(Fill::NonZero, s, Color::WHITE, None, &RoundedRect::new(20.0, 106.0, 878.0, 356.0, 10.0));
    scene.stroke(&Stroke::new(1.0), s, Color::from_rgb8(0xE2, 0xDF, 0xD8), None, &RoundedRect::new(20.5, 106.5, 877.5, 355.5, 10.0));
    let mut p = BezPath::new(); p.move_to((20.0, 300.0)); for i in 0..32 { let y = 300.0 - ((i * 7) % 15) as f64 * 12.0; p.line_to((20.0 + i as f64 * 26.8, y)); p.line_to((20.0 + (i + 1) as f64 * 26.8, y)); }
    scene.stroke(&Stroke::new(2.5), s, Color::from_rgb8(0xFF, 0x5A, 0x1F), None, &p);
    for (txt, fam, wt, size, x, y) in [("PIXL", "Geist", 700.0, 18.0, 20.0, 16.0), ("Hyrule Fanfare · рисуйте мышью", "Geist", 600.0, 13.0, 300.0, 18.0), ("32 × 4-BIT WAVE RAM", "Geist Mono", 500.0, 10.0, 34.0, 117.0)] {
        let mut b = lcx.ranged_builder(&mut fcx, txt, 2.0, true);
        b.push_default(StyleProperty::FontFamily(FontFamily::named(fam)));
        b.push_default(StyleProperty::FontWeight(FontWeight::new(wt)));
        b.push_default(StyleProperty::FontSize(size));
                let mut layout = b.build(txt);
        layout.break_all_lines(None);
        for line in layout.lines() { for item in line.items() { if let PositionedLayoutItem::GlyphRun(run) = item {
            let r = run.run(); let font = r.font(); let fs = r.font_size();
            let mut gx = run.offset(); let gy = run.baseline();
            let glyphs = run.glyphs().map(|g| { let gl = vello::Glyph { id: g.id as u32, x: gx + g.x, y: gy - g.y }; gx += g.advance; gl }).collect::<Vec<_>>();
            scene.draw_glyphs(font).font_size(fs).transform(Affine::translate((x * 2.0, y * 2.0))).brush(Color::from_rgb8(0x16,0x16,0x16)).hint(false).draw(Fill::NonZero, glyphs.into_iter());
        }}}
    }
    let tex = device.create_texture(&wgpu::TextureDescriptor { label: None, size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm, usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC, view_formats: &[] });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let t1 = std::time::Instant::now();
    renderer.render_to_texture(&device, &queue, &scene, &view, &vello::RenderParams { base_color: Color::from_rgb8(0xE7, 0xE4, 0xDE), width: w, height: h, antialiasing_method: vello::AaConfig::Area }).expect("render");
    let padded = (w * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (padded * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    enc.copy_texture_to_buffer(tex.as_image_copy(), wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None } }, wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 });
    queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    eprintln!("render+readback {:?}", t1.elapsed());
    let data = buf.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h { let st = (row * padded) as usize; out.extend_from_slice(&data[st..st + (w * 4) as usize]); }
    let path = std::env::args().nth(1).unwrap_or("spike.png".into());
    let f = std::fs::File::create(&path).unwrap();
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w, h); e.set_color(png::ColorType::Rgba); e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&out).unwrap();
    eprintln!("wrote {path}");
}
