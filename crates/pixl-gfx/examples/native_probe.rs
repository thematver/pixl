//! Small native host used to validate rendering, mouse gestures and teardown.
use pixl_gfx::{
    kurbo::{Rect, Size},
    rgb, run_standalone, App, Canvas, Event, EventCtx, MouseButton, TextStyle,
};
#[derive(Default)]
struct Probe {
    value: f64,
}
impl App for Probe {
    fn size(&self) -> Size {
        Size::new(900.0, 560.0)
    }
    fn event(&mut self, e: &Event, ctx: &mut EventCtx) {
        match e {
            Event::MouseDown(e) if e.button == Some(MouseButton::Left) => ctx.begin_infinite_drag(),
            Event::MouseDrag(e) if ctx.is_infinite_drag() => {
                self.value = (self.value - e.delta.y * 0.005).clamp(0.0, 1.0);
                ctx.request_redraw();
            }
            _ => {}
        }
    }
    fn frame(&mut self, c: &mut Canvas, _: f64) {
        c.fill_rect(c.bounds(), rgb(0xF7F6F3));
        c.text((20.0, 16.0), "PIXL", &TextStyle::sans(18.0).weight(700.0));
        c.fill_rounded_rect(Rect::new(20.0, 106.0, 880.0, 356.0), 10.0, rgb(0xFFFFFF));
        c.text((34.0, 117.0), "METAL · RETINA · Привет", &TextStyle::mono(10.0));
        c.fill_rect(Rect::new(34.0, 200.0, 34.0 + 800.0 * self.value, 220.0), rgb(0xFF5A1F));
        c.text((34.0, 240.0), &format!("DRAG {:.0}%", self.value * 100.0), &TextStyle::sans(24.0));
    }
}
fn main() -> Result<(), pixl_gfx::GfxError> {
    run_standalone(Box::<Probe>::default())
}
