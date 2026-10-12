use pixl_gfx::kurbo::{Rect, Size};
use pixl_gfx::{rgb, App, Canvas, Event, EventCtx, Headless, MouseEvent, TextStyle};

#[derive(Default)]
struct Probe {
    observed_focus: Vec<bool>,
    observed_drag: Vec<bool>,
}
impl App for Probe {
    fn size(&self) -> Size {
        Size::new(120.0, 80.0)
    }
    fn frame(&mut self, c: &mut Canvas, _: f64) {
        c.fill_rect(c.bounds(), rgb(0xF7F6F3));
        c.with_clip(&Rect::new(10.0, 10.0, 40.0, 40.0), |c| {
            c.fill_rect(Rect::new(0.0, 0.0, 70.0, 60.0), rgb(0xFF5A1F));
        });
        c.text((10.0, 50.0), "PIXL · Привет", &TextStyle::sans(10.0));
    }
    fn event(&mut self, e: &Event, ctx: &mut EventCtx) {
        self.observed_focus.push(ctx.has_text_focus());
        self.observed_drag.push(ctx.is_infinite_drag());
        if matches!(e, Event::MouseDown(_)) {
            ctx.focus_text_input();
            ctx.begin_infinite_drag();
        }
    }
}

#[test]
fn offscreen_clip_retina_and_event_state_match_a_view() {
    let mut h = Headless::new().expect("Metal device");
    let mut app = Probe::default();
    let image = h.render(&mut app, 2.0).expect("render/readback");
    assert_eq!((image.width, image.height), (240, 160));
    assert_eq!(image.pixel(40, 40), Some([255, 90, 31, 255]));
    assert_eq!(image.pixel(4, 4), Some([247, 246, 243, 255]));
    assert_eq!(image.pixel(100, 40), Some([247, 246, 243, 255]));
    let m = MouseEvent::at((20.0, 20.0));
    h.event(&mut app, &Event::MouseDown(m));
    h.event(&mut app, &Event::MouseDrag(m));
    h.event(&mut app, &Event::MouseUp(m));
    h.event(&mut app, &Event::MouseMove(m));
    h.event(&mut app, &Event::TextFocusLost);
    assert_eq!(app.observed_focus, [false, true, true, true, false]);
    assert_eq!(app.observed_drag, [false, true, true, false, false]);
}
