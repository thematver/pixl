//! Renders the eleven reference states plus all new focus/browser/chip variants at 2×.
use pixl_core::{Bridge,Channel,ChipMode,Focus,Lang,ParamId};
use pixl_gui::{EditorApp,MockBridge};
use std::sync::Arc;
fn main()->Result<(),Box<dyn std::error::Error>>{
    let dir=std::env::args().nth(1).unwrap_or_else(||"target/gui-snapshots".into());std::fs::create_dir_all(&dir)?;
    let states=[
        ("01-wave",Channel::Wave,None,ChipMode::Gb),
        ("02-pulse1",Channel::P1,None,ChipMode::Gb),
        ("03-noise",Channel::Noise,None,ChipMode::Gb),
        ("04-nes-triangle",Channel::Wave,None,ChipMode::Nes),
        ("05-sweep",Channel::Wave,Some(Focus::Sweep),ChipMode::Gb),
        ("06-vibrato",Channel::Wave,Some(Focus::Vibrato),ChipMode::Gb),
        ("07-crush",Channel::Wave,Some(Focus::Crush),ChipMode::Gb),
        ("08-glide",Channel::P1,Some(Focus::Glide),ChipMode::Gb),
        ("09-unison",Channel::P2,Some(Focus::Unison),ChipMode::Gb),
        ("10-browser",Channel::Wave,None,ChipMode::Gb),
        ("11-macro-assign",Channel::Wave,None,ChipMode::Gb),
        ("12-envelope",Channel::Wave,Some(Focus::Env),ChipMode::Gb),
        ("13-arp",Channel::Wave,Some(Focus::Arp),ChipMode::Gb),
        ("14-echo",Channel::Wave,Some(Focus::Echo),ChipMode::Gb),
        ("15-browser-save",Channel::Wave,None,ChipMode::Gb),
        ("16-free-wave",Channel::Wave,None,ChipMode::Free),
        ("17-free-envelope",Channel::P1,Some(Focus::Env),ChipMode::Free),
        ("18-en-browser",Channel::Wave,None,ChipMode::Gb),
        ("19-wave-tools",Channel::Wave,None,ChipMode::Gb),
    ];
    let mut harness=pixl_gfx::Headless::new()?;
    for(name,channel,focus,chip)in states{let bridge=Arc::new(MockBridge::new());bridge.set_param_once(ParamId::Chip,chip.index()as f32);bridge.animate(0.7);let mut app=EditorApp::new(bridge);app.ui.channel=channel;app.ui.focus=focus;
        if name.contains("browser"){app.open_browser();}if name=="18-en-browser"{app.ui.lang=Lang::En;}
        if name=="15-browser-save"{app.browser.saving=true;app.browser.save_name="My Hyrule Variation".into();}
        if name=="11-macro-assign"{app.assign=true;app.ui.macro_selected=1;}
        if name=="19-wave-tools" {
            harness.event(&mut app,&pixl_gfx::Event::MouseMove(pixl_gfx::MouseEvent {
                pos:pixl_gfx::kurbo::Point::new(432.,225.),delta:pixl_gfx::kurbo::Vec2::ZERO,
                button:None,buttons:pixl_gfx::MouseButtons::NONE,mods:pixl_gfx::Modifiers::default(),click_count:0,
            }));
        }
        let path=format!("{dir}/{name}.png");harness.render(&mut app,2.)?.save_png(&path)?;println!("{path}");
    }Ok(())
}
