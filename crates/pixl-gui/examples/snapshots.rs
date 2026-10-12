//! Deterministic atlas of representative UI state classes, using the production painter.
//!
//! cargo run -p pixl-gui --release --example snapshots -- target/design-review/2026-10-13/before
//! Run the same command with `after` after an increment. No audio host or wall clock is used.
//! This covers finite state classes; it does not claim to enumerate every parameter combination.
use pixl_core::{
    Bridge, Category, Channel, ChannelField, ChipMode, Focus, Lang, ParamId, ParamKind, TelemetrySnapshot, WaveTool,
};
use pixl_gfx::{
    kurbo::Point, App, Event, EventCtx, Headless, Menu, MenuItem, Modifiers, MouseButton, MouseEvent, TextCommand,
};
use pixl_gui::{Action, EditorApp, MockBridge};
use std::{fmt::Write as _, path::Path, sync::Arc};

#[derive(Clone)]
struct State {
    name: String,
    group: &'static str,
    note: String,
    channel: Channel,
    focus: Option<Focus>,
    chip: ChipMode,
    scenario: Scenario,
    scale: f64,
    params: Vec<(ParamId, f32)>,
}
#[derive(Clone, Copy)]
enum Scenario {
    Plain,
    Browser,
    BrowserSave,
    EnglishBrowser,
    Assign,
    WaveHover,
    ExactValue,
    InvalidValue,
    Hover,
    Pressed,
    FineDrag,
    WaveDraw,
    WaveLine,
    Search,
    EmptySearch,
    Category(Category),
    ChipFilter(ChipMode),
    Favorites,
    EmptyFavorites,
    User,
    EmptyUser,
    LongName,
    ScrollEnd,
    Sorted,
    SaveError,
    SaveCategory(Category),
    SaveSelection,
    SearchSelection,
    Preedit,
    ComparisonB,
    ComparisonA,
    HistoryUndo,
    HistoryRedo,
    Disabled,
    Minimum,
    Maximum,
    Silence,
    Clipping,
    DenseVoices,
    AssignNegative,
    AssignFull,
    FramePal,
    Tempo(f32),
    ArpLive(u8),
}
impl State {
    fn new(
        name: impl Into<String>,
        group: &'static str,
        channel: Channel,
        focus: Option<Focus>,
        chip: ChipMode,
        scenario: Scenario,
        note: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            group,
            note: note.into(),
            channel,
            focus,
            chip,
            scenario,
            scale: 1.0,
            params: Vec::new(),
        }
    }
}
const FOCI: [(Focus, &str); 8] = [
    (Focus::Env, "envelope"),
    (Focus::Sweep, "sweep"),
    (Focus::Vibrato, "vibrato"),
    (Focus::Arp, "arp"),
    (Focus::Crush, "crush"),
    (Focus::Glide, "glide"),
    (Focus::Unison, "unison"),
    (Focus::Echo, "echo"),
];
fn slug(chip: ChipMode) -> &'static str {
    match chip {
        ChipMode::Nes => "nes",
        ChipMode::Gb => "gb",
        ChipMode::Free => "free",
    }
}
fn channel_slug(channel: Channel) -> &'static str {
    match channel {
        Channel::P1 => "pulse1",
        Channel::P2 => "pulse2",
        Channel::Wave => "wave",
        Channel::Noise => "noise",
    }
}

fn states() -> Vec<State> {
    use Scenario::*;
    let old = [
        ("01-wave", Channel::Wave, None, ChipMode::Gb, Plain),
        ("02-pulse1", Channel::P1, None, ChipMode::Gb, Plain),
        ("03-noise", Channel::Noise, None, ChipMode::Gb, Plain),
        ("04-nes-triangle", Channel::Wave, None, ChipMode::Nes, Plain),
        ("05-sweep", Channel::Wave, Some(Focus::Sweep), ChipMode::Gb, Plain),
        ("06-vibrato", Channel::Wave, Some(Focus::Vibrato), ChipMode::Gb, Plain),
        ("07-crush", Channel::Wave, Some(Focus::Crush), ChipMode::Gb, Plain),
        ("08-glide", Channel::P1, Some(Focus::Glide), ChipMode::Gb, Plain),
        ("09-unison", Channel::P2, Some(Focus::Unison), ChipMode::Gb, Plain),
        ("10-browser", Channel::Wave, None, ChipMode::Gb, Browser),
        ("11-macro-assign", Channel::Wave, None, ChipMode::Gb, Assign),
        ("12-envelope", Channel::Wave, Some(Focus::Env), ChipMode::Gb, Plain),
        ("13-arp", Channel::Wave, Some(Focus::Arp), ChipMode::Gb, Plain),
        ("14-echo", Channel::Wave, Some(Focus::Echo), ChipMode::Gb, Plain),
        ("15-browser-save", Channel::Wave, None, ChipMode::Gb, BrowserSave),
        ("16-free-wave", Channel::Wave, None, ChipMode::Free, Plain),
        ("17-free-envelope", Channel::P1, Some(Focus::Env), ChipMode::Free, Plain),
        ("18-en-browser", Channel::Wave, None, ChipMode::Gb, EnglishBrowser),
        ("19-wave-tools", Channel::Wave, None, ChipMode::Gb, WaveHover),
        ("20-exact-value", Channel::Wave, None, ChipMode::Gb, ExactValue),
        ("21-invalid-value", Channel::Wave, None, ChipMode::Gb, InvalidValue),
    ];
    let mut out: Vec<_> = old
        .into_iter()
        .map(|(n, c, f, ch, s)| {
            State::new(n, "reference", c, f, ch, s, "Original reference state; names retained for comparisons.")
        })
        .collect();
    for &chip in ChipMode::ALL {
        for &channel in Channel::ALL {
            out.push(State::new(
                format!("channel-{}-{}", slug(chip), channel_slug(channel)),
                "channels",
                channel,
                None,
                chip,
                Plain,
                "Channel shape in each chip mode.",
            ));
            out.push(State::new(
                format!("disabled-{}-{}", slug(chip), channel_slug(channel)),
                "disabled",
                channel,
                None,
                chip,
                Disabled,
                "Selected channel is disabled; editing must remain intelligible.",
            ));
        }
        for &(focus, name) in &FOCI {
            out.push(State::new(
                format!("focus-{}-{name}", slug(chip)),
                "focus",
                Channel::Wave,
                Some(focus),
                chip,
                Plain,
                "Every focus editor in every chip mode.",
            ));
        }
    }
    for &(focus, name) in &FOCI {
        for (scenario, bound) in [(Minimum, "min"), (Maximum, "max"), (Disabled, "off")] {
            out.push(State::new(
                format!("limits-{name}-{bound}"),
                "limits",
                Channel::Wave,
                Some(focus),
                ChipMode::Gb,
                scenario,
                "Boundary values and disabled effect; macros zeroed to make the boundary exact.",
            ));
        }
    }
    for (name, scenario) in [
        ("hover-tile", Hover),
        ("pressed-tile", Pressed),
        ("fine-drag", FineDrag),
        ("wave-draw-active", WaveDraw),
        ("wave-line-active", WaveLine),
        ("comparison-b", ComparisonB),
        ("comparison-a", ComparisonA),
        ("undo-available", HistoryUndo),
        ("redo-available", HistoryRedo),
        ("assign-negative", AssignNegative),
        ("assign-full", AssignFull),
        ("silence", Silence),
        ("clipping", Clipping),
        ("dense-voices", DenseVoices),
        ("pal-frame", FramePal),
    ] {
        out.push(State::new(
            format!("interaction-{name}"),
            "interaction",
            Channel::Wave,
            None,
            ChipMode::Gb,
            scenario,
            "Synthetic input events or explicit audio telemetry, never a hidden clock.",
        ));
    }
    for (name, scenario) in [
        ("search", Search),
        ("empty-search", EmptySearch),
        ("favorites", Favorites),
        ("empty-favorites", EmptyFavorites),
        ("user", User),
        ("empty-user", EmptyUser),
        ("long-unicode-name", LongName),
        ("scroll-bottom", ScrollEnd),
        ("alphabetical", Sorted),
        ("save-error", SaveError),
        ("save-selection", SaveSelection),
        ("search-selection", SearchSelection),
        ("ime-composition", Preedit),
    ] {
        out.push(State::new(
            format!("browser-{name}"),
            "browser",
            Channel::Wave,
            None,
            ChipMode::Gb,
            scenario,
            "Browser filtering, selection, empty content, text input and error states.",
        ));
    }
    for category in pixl_core::Category::ALL {
        out.push(State::new(
            format!("browser-category-{}", category.tag().to_lowercase()),
            "browser",
            Channel::Wave,
            None,
            ChipMode::Gb,
            Category(category),
            "Every category selected.",
        ));
        out.push(State::new(
            format!("save-category-{}", category.tag().to_lowercase()),
            "modal",
            Channel::Wave,
            None,
            ChipMode::Gb,
            SaveCategory(category),
            "Every save category selected.",
        ));
    }
    for &chip in ChipMode::ALL {
        out.push(State::new(
            format!("browser-chip-{}", slug(chip)),
            "browser",
            Channel::Wave,
            None,
            ChipMode::Gb,
            ChipFilter(chip),
            "Every chip filter selected.",
        ));
    }
    // Every discrete graphical mode, without exploding into an unhelpful Cartesian product.
    for &(id, focus, name) in &[
        (ParamId::VibShape, Focus::Vibrato, "vibrato-shape"),
        (ParamId::SweepCurve, Focus::Sweep, "sweep-curve"),
        (ParamId::SweepDir, Focus::Sweep, "sweep-direction"),
        (ParamId::ArpMode, Focus::Arp, "arp-mode"),
        (ParamId::ArpChord, Focus::Arp, "arp-chord"),
        (ParamId::GlideCurve, Focus::Glide, "glide-curve"),
        (ParamId::GlideMode, Focus::Glide, "glide-mode"),
        (ParamId::EchoFir, Focus::Echo, "echo-filter"),
    ] {
        if let ParamKind::Enum { variants } = id.spec().kind {
            for (i, label) in variants.iter().enumerate() {
                let mut s = State::new(
                    format!("mode-{name}-{i:02}"),
                    "modes",
                    Channel::Wave,
                    Some(focus),
                    ChipMode::Gb,
                    Plain,
                    format!("{}: {label}", id.spec().name),
                );
                s.params.push((id, i as f32));
                out.push(s);
            }
        }
    }
    for &chip in ChipMode::ALL {
        for mode in 0..2 {
            let mut s = State::new(
                format!("mode-noise-{}-{mode}", slug(chip)),
                "modes",
                Channel::Noise,
                None,
                chip,
                Plain,
                "Long and short LFSR modes in every chip.",
            );
            s.params.push((ParamId::NsMode, mode as f32));
            out.push(s);
        }
    }
    for (name, channel, focus, scenario) in [
        ("wave", Channel::Wave, None, Plain),
        ("browser", Channel::Wave, None, Browser),
        ("value", Channel::Wave, None, ExactValue),
        ("vibrato", Channel::Wave, Some(Focus::Vibrato), Plain),
    ] {
        for &zoom in &pixl_core::bridge::UI_SCALES {
            let scale = zoom as f64;
            let mut s = State::new(
                format!("scale-{:03}-{name}", (scale * 100.) as u32),
                "scale",
                channel,
                focus,
                ChipMode::Gb,
                scenario,
                "Rendered at UI zoom × Retina 2; logical design stays 900×560.",
            );
            s.scale = scale;
            out.push(s);
        }
    }
    for tempo in [72., 174.] {
        for (focus, name) in [(Focus::Echo, "echo"), (Focus::Arp, "arp")] {
            out.push(State::new(
                format!("live-{name}-tempo-{}", tempo as u32),
                "live",
                Channel::Wave,
                Some(focus),
                ChipMode::Gb,
                Tempo(tempo),
                "New state: explicit mock host tempo; no corresponding baseline PNG.",
            ));
        }
    }
    for (mode, name) in [(1, "down"), (2, "updown"), (3, "single-held")] {
        out.push(State::new(
            format!("live-arp-{name}"),
            "live",
            Channel::Wave,
            Some(Focus::Arp),
            ChipMode::Gb,
            ArpLive(mode),
            "New state: held-note index telemetry, including descending and single-note patterns.",
        ));
    }
    out
}

fn mouse(pos: (f64, f64)) -> MouseEvent {
    MouseEvent::at(pos).button(MouseButton::Left).clicks(1)
}
fn hit(app: &EditorApp, predicate: impl Fn(&Action) -> bool) -> Point {
    app.controls().into_iter().find(|c| predicate(&c.action)).expect("atlas action exists").rect.center()
}
fn click(h: &mut Headless, app: &mut EditorApp, action: impl Fn(&Action) -> bool) {
    let p = hit(app, action);
    let e = mouse((p.x, p.y));
    h.event(app, &Event::MouseDown(e));
    h.event(app, &Event::MouseUp(e));
}
fn focus_params(focus: Focus) -> Vec<ParamId> {
    use ParamId::*;
    match focus {
        Focus::Env => vec![WvAttack, WvDecay, WvSustain, WvRelease],
        Focus::Sweep => vec![SweepAmt, SweepTime, SweepCurve, SweepDir],
        Focus::Vibrato => vec![VibRate, VibDepth, VibDelay, VibFade, VibShape],
        Focus::Arp => vec![ArpMode, ArpRate, ArpOct, ArpChord],
        Focus::Crush => vec![CrushBits, CrushRate],
        Focus::Glide => vec![GlideTime, GlideMode, GlideCurve],
        Focus::Unison => vec![UniVoices, UniDetune, UniWidth],
        Focus::Echo => vec![EchoTime, EchoFb, EchoMix, EchoFir, EchoWidth],
    }
}
fn on_param(focus: Focus) -> Option<ParamId> {
    Some(match focus {
        Focus::Env => return None,
        Focus::Sweep => ParamId::SweepOn,
        Focus::Vibrato => ParamId::VibOn,
        Focus::Arp => ParamId::ArpOn,
        Focus::Crush => ParamId::CrushOn,
        Focus::Glide => ParamId::GlideOn,
        Focus::Unison => ParamId::UniOn,
        Focus::Echo => ParamId::EchoOn,
    })
}
fn setup(h: &mut Headless, s: &State) -> EditorApp {
    use Scenario::*;
    let bridge = Arc::new(MockBridge::new());
    bridge.set_param_once(ParamId::Chip, s.chip.index() as f32);
    bridge.animate(0.7);
    let mut app = EditorApp::new(bridge.clone());
    app.ui.channel = s.channel;
    app.ui.focus = s.focus;
    app.ui.scale = s.scale as f32;
    for &(id, v) in &s.params {
        bridge.set_param_once(id, v);
    }
    match s.scenario {
        Plain => {}
        Browser => app.open_browser(),
        BrowserSave | SaveError | SaveCategory(_) | SaveSelection => {
            app.open_browser();
            click(h, &mut app, |a| matches!(a, Action::SaveForm));
            app.browser.save_name =
                if matches!(s.scenario, SaveError) { String::new() } else { "My Hyrule Variation".into() };
            if matches!(s.scenario, SaveError) {
                click(h, &mut app, |a| matches!(a, Action::Save));
            }
            if let SaveCategory(c) = s.scenario {
                app.browser.save_category = c;
            }
            if matches!(s.scenario, SaveSelection) {
                h.event(&mut app, &Event::TextCommand(TextCommand::SelectAll));
            }
        }
        EnglishBrowser => {
            app.open_browser();
            app.ui.lang = Lang::En;
        }
        Assign => {
            app.assign = true;
            app.ui.macro_selected = 1;
        }
        WaveHover => {
            h.event(&mut app, &Event::MouseMove(MouseEvent::at((432., 225.))));
        }
        ExactValue | InvalidValue => {
            let mut ctx = EventCtx::new(h.text(), 2., false, false);
            app.edit_value(ParamId::VibRate, &mut ctx);
            if matches!(s.scenario, InvalidValue) {
                app.event(&Event::TextInput("oops".into()), &mut ctx);
                app.event(&Event::TextCommand(TextCommand::InsertNewline), &mut ctx);
            }
        }
        Hover => {
            let p = hit(&app, |a| matches!(a, Action::Focus(Some(Focus::Vibrato))));
            h.event(&mut app, &Event::MouseMove(MouseEvent::at(p)));
            h.time_frame(&mut app, 2., 0.2).unwrap();
        }
        Pressed => {
            let p = hit(&app, |a| matches!(a, Action::Focus(Some(Focus::Vibrato))));
            h.event(&mut app, &Event::MouseDown(mouse((p.x, p.y))));
        }
        FineDrag => {
            let p = hit(&app, |a| matches!(a, Action::Param(ParamId::WvLevel)));
            let mods = Modifiers { shift: true, ..Modifiers::NONE };
            h.event(&mut app, &Event::MouseDown(mouse((p.x, p.y)).mods(mods)));
            h.event(&mut app, &Event::MouseDrag(mouse((p.x, p.y - 24.)).mods(mods).delta((0., -24.))));
        }
        WaveDraw | WaveLine => {
            app.ui.wave_tool = if matches!(s.scenario, WaveLine) { WaveTool::Line } else { WaveTool::Draw };
            h.event(&mut app, &Event::MouseDown(mouse((260., 280.))));
            h.event(&mut app, &Event::MouseDrag(mouse((610., 180.)).delta((350., -100.))));
        }
        Search | EmptySearch | SearchSelection | Preedit => {
            app.open_browser();
            click(h, &mut app, |a| matches!(a, Action::Search));
            h.event(
                &mut app,
                &Event::TextInput(if matches!(s.scenario, EmptySearch) {
                    "ZZZ NO MATCH 93821".into()
                } else {
                    "castle".into()
                }),
            );
            if matches!(s.scenario, SearchSelection) {
                h.event(&mut app, &Event::TextCommand(TextCommand::SelectAll));
            }
            if matches!(s.scenario, Preedit) {
                h.event(&mut app, &Event::TextPreedit("城の音".into()));
            }
        }
        Category(c) => {
            app.open_browser();
            app.browser.category = Some(c);
            app.browser.scroll = 0.;
            app.browser.selected = app.filtered_presets().first().map(|p| p.id.clone());
        }
        ChipFilter(c) => {
            app.open_browser();
            app.browser.chip = Some(c);
            app.browser.scroll = 0.;
            app.browser.selected = app.filtered_presets().first().map(|p| p.id.clone());
        }
        Favorites | EmptyFavorites => {
            if matches!(s.scenario, Favorites) {
                for p in bridge.preset_list().iter().take(4) {
                    bridge.set_favorite(&p.id, true);
                }
            }
            app.open_browser();
            app.browser.favorites = true;
            app.browser.scroll = 0.;
            app.browser.selected = app.filtered_presets().first().map(|p| p.id.clone());
        }
        User | EmptyUser | LongName => {
            if !matches!(s.scenario, EmptyUser) {
                let name = if matches!(s.scenario, LongName) {
                    "Ночное электричество · 東京の星 · A very long evolving chiptune landscape 0123456789"
                } else {
                    "My Hyrule Variation"
                };
                bridge.save_user_preset(name, pixl_core::Category::Pad).unwrap();
            }
            app.open_browser();
            app.browser.user_only = true;
            app.browser.scroll = 0.;
            app.browser.selected = app.filtered_presets().first().map(|p| p.id.clone());
        }
        ScrollEnd => {
            app.open_browser();
            app.browser.scroll = (app.filtered_presets().len() as f64 * 38. - 380.).max(0.);
            app.browser.selected = app.filtered_presets().last().map(|p| p.id.clone());
        }
        Sorted => {
            app.open_browser();
            app.browser.alphabetical = true;
            app.browser.scroll = 0.;
            app.browser.selected = app.filtered_presets().first().map(|p| p.id.clone());
        }
        ComparisonB | ComparisonA => {
            click(h, &mut app, |a| matches!(a, Action::Compare));
            bridge.set_param_once(ParamId::macro_value(0), 95.);
            if matches!(s.scenario, ComparisonA) {
                click(h, &mut app, |a| matches!(a, Action::Compare));
            }
        }
        HistoryUndo | HistoryRedo => {
            h.event(&mut app, &Event::MenuItemSelected(202));
            if matches!(s.scenario, HistoryRedo) {
                click(h, &mut app, |a| matches!(a, Action::Undo));
            }
        }
        Disabled => {
            bridge.set_param_once(ParamId::channel(s.channel, ChannelField::On), 0.);
            if let Some(f) = s.focus {
                if let Some(id) = on_param(f) {
                    bridge.set_param_once(id, 0.);
                }
            }
            bridge.set_telemetry(TelemetrySnapshot::default());
        }
        Minimum | Maximum => {
            for i in 0..4 {
                bridge.set_param_once(ParamId::macro_value(i), 0.);
            }
            if let Some(f) = s.focus {
                if let Some(id) = on_param(f) {
                    bridge.set_param_once(id, 1.);
                }
                for id in focus_params(f) {
                    bridge.set_param_once(id, id.denormalize(if matches!(s.scenario, Maximum) { 1. } else { 0. }));
                }
                if f == Focus::Echo {
                    bridge.set_param_once(ParamId::EchoSync, 0.);
                }
            }
        }
        Silence => bridge.set_telemetry(TelemetrySnapshot::default()),
        Clipping => {
            let mut t = bridge.telemetry();
            t.peak_db = [2.4, 1.2];
            t.peak_hold_db = 2.4;
            t.rms_db = [-0.5, -1.];
            bridge.set_telemetry(t);
        }
        DenseVoices => {
            let mut t = bridge.telemetry();
            t.active_voices = 16;
            t.held_notes = [u64::MAX, u64::MAX];
            t.env_level = [1.; 4];
            bridge.set_telemetry(t);
        }
        AssignNegative | AssignFull => {
            app.assign = true;
            app.ui.macro_selected = 2;
            for id in [
                ParamId::SweepAmt,
                ParamId::VibDepth,
                ParamId::CrushBits,
                ParamId::GlideTime,
                ParamId::UniDetune,
                ParamId::WvLevel,
            ] {
                bridge.set_macro_assignment(2, id, if matches!(s.scenario, AssignNegative) { -0.85 } else { 1. });
            }
        }
        FramePal => {
            bridge.set_param_once(ParamId::FrameRate, 1.);
            app.ui.focus = Some(Focus::Vibrato);
        }
        Tempo(bpm) => {
            let mut t = bridge.telemetry();
            t.tempo_bpm = bpm;
            t.arp_step = Some(1);
            bridge.set_telemetry(t);
            if s.focus == Some(Focus::Echo) {
                bridge.set_param_once(ParamId::EchoOn, 1.);
                bridge.set_param_once(ParamId::EchoSync, 1.);
            } else {
                bridge.set_param_once(ParamId::ArpOn, 1.);
                bridge.set_param_once(ParamId::ArpSync, 1.);
            }
        }
        ArpLive(mode) => {
            bridge.set_param_once(ParamId::ArpOn, 1.);
            bridge.set_param_once(ParamId::ArpChord, 0.);
            bridge.set_param_once(ParamId::ArpMode, if mode == 3 { 0. } else { mode as f32 });
            let mut t = bridge.telemetry();
            t.held_notes = if mode == 3 { [1u64 << 60, 0] } else { [(1u64 << 60) | (1u64 << 64), 1u64 << 3] };
            t.last_note = Some(60);
            t.arp_step = Some(if mode == 3 { 0 } else { 1 });
            bridge.set_telemetry(t);
        }
    }
    app
}

fn json(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                write!(out, "\\u{:04x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn write_manifest(dir: &Path, states: &[State]) -> std::io::Result<()> {
    let mut out=String::from("{\n  \"schema\": 1,\n  \"renderer\": \"production pixl-gui + offscreen Metal, deterministic MockBridge\",\n  \"coverage\": \"finite representative state classes; not every parameter combination or native host menu\",\n  \"native_menu_inventory\": \"menu-inventory.json\",\n  \"states\": [\n");
    for (i, s) in states.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        write!(out,"    {{\"name\":{},\"file\":{},\"group\":{},\"chip\":{},\"channel\":{},\"focus\":{},\"ui_scale\":{},\"retina_scale\":2,\"width\":{},\"height\":{},\"note\":{}}}",json(&s.name),json(&format!("{}.png",s.name)),json(s.group),json(s.chip.label()),json(s.channel.label()),json(&s.focus.map(|f|format!("{f:?}")).unwrap_or_else(||"Shape".into())),s.scale,(1800.*s.scale).round() as u32,(1120.*s.scale).round() as u32,json(&s.note)).unwrap();
    }
    out.push_str("\n  ]\n}\n");
    std::fs::write(dir.join("manifest.json"), out)
}
fn menu_json(menu: &Menu) -> String {
    let mut out = String::from("[");
    for (i, item) in menu.items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match item {
            MenuItem::Action { id, label, checked, enabled } => write!(
                out,
                "{{\"kind\":\"action\",\"id\":{id},\"label\":{},\"checked\":{checked},\"enabled\":{enabled}}}",
                json(label)
            )
            .unwrap(),
            MenuItem::Submenu { label, menu, enabled } => write!(
                out,
                "{{\"kind\":\"submenu\",\"label\":{},\"enabled\":{enabled},\"items\":{}}}",
                json(label),
                menu_json(menu)
            )
            .unwrap(),
            MenuItem::Separator => out.push_str("{\"kind\":\"separator\"}"),
        }
    }
    out.push(']');
    out
}
fn write_menu_inventory(dir: &Path, h: &mut Headless) -> std::io::Result<()> {
    let mut out=String::from("{\"capture\":\"Semantic production menu requests from Headless.event effects; native OS menu pixels require a foreground capture.\",\"menus\":[");
    let mut first = true;
    for (lang, lang_name) in [(Lang::Ru, "ru"), (Lang::En, "en")] {
        for name in [
            "settings-default",
            "settings-undo",
            "settings-redo",
            "parameter-modulatable",
            "parameter-global",
            "wave-empty-clipboard",
            "wave-filled-clipboard",
            "nes-triangle-no-wave-menu",
        ] {
            let s = State::new("menu", "menu", Channel::Wave, None, ChipMode::Gb, Scenario::Plain, "");
            let mut app = setup(h, &s);
            app.ui.lang = lang;
            if name == "settings-undo" || name == "settings-redo" {
                h.event(&mut app, &Event::MenuItemSelected(202));
            }
            if name == "settings-redo" {
                h.event(&mut app, &Event::MenuItemSelected(170));
            }
            if name == "wave-filled-clipboard" {
                h.event(&mut app, &Event::MenuItemSelected(204));
            }
            if name == "nes-triangle-no-wave-menu" {
                app.bridge.set_param_once(ParamId::Chip, 0.);
            }
            let effects = if name.starts_with("settings") {
                let p = hit(&app, |a| matches!(a, Action::Settings));
                let e = mouse((p.x, p.y));
                h.event(&mut app, &Event::MouseDown(e));
                h.event(&mut app, &Event::MouseUp(e))
            } else if name.starts_with("parameter") {
                let id = if name == "parameter-global" { ParamId::OutGain } else { ParamId::WvLevel };
                let p = hit(&app, |a| matches!(a,Action::Param(found) if *found==id));
                h.event(&mut app, &Event::MouseDown(MouseEvent::at(p).button(MouseButton::Right).clicks(1)))
            } else {
                h.event(&mut app, &Event::MouseDown(MouseEvent::at((432., 225.)).button(MouseButton::Right).clicks(1)))
            };
            if !first {
                out.push(',');
            }
            first = false;
            write!(
                out,
                "{{\"name\":{},\"language\":{},\"requested\":{},\"items\":{}}}",
                json(name),
                json(lang_name),
                effects.context_menu.is_some(),
                effects.context_menu.as_ref().map(|(m, _)| menu_json(m)).unwrap_or_else(|| "[]".into())
            )
            .unwrap();
        }
    }
    out.push_str("]}\n");
    std::fs::write(dir.join("menu-inventory.json"), out)
}
fn write_gallery(dir: &Path, states: &[State]) -> std::io::Result<()> {
    let manifest = std::fs::read_to_string(dir.join("manifest.json"))?;
    let template = include_str!("atlas-gallery.html");
    std::fs::write(dir.join("index.html"), template.replace("__MANIFEST__", &manifest.replace("</", "<\\/")))?;
    let coverage=format!("# PIXL UI state atlas\n\n{} deterministic PNGs, rendered by the actual production painter at 2× Retina unless a zoom is specified.\n\n- The original 21 reference filenames are retained.\n- Shape: four channels × three chip modes, enabled and disabled.\n- Focus: all eight focus editors × three chip modes.\n- Boundaries: minimum, maximum and disabled for all eight focus editors; macros zeroed at boundaries.\n- Discrete graphical modes: every vibrato shape, sweep curve/direction, arp mode/chord, glide curve/mode, echo filter, long/short LFSR × chip.\n- Browser: six categories, three chip filters, search, empty search, favorites/empty favorites, user/empty user, long Unicode name, bottom of list, A–Z sorting, selected text and IME composition.\n- Dialogs: save form, all six save categories, name selection, save validation error, exact value, invalid exact value.\n- Interaction: hover, held press, fine drag, active draw/line, undo/redo, A/B, macro assignment extremes, silence, overload, dense voice telemetry, PAL.\n- Zoom: all six supported scales (75%, 100%, 125%, 150%, 175%, 200%) × wave, browser, numeric dialog, vibrato (2× Retina backing).\n\n## Scope and reproducibility\n\nThis is coverage of finite UI classes, not all possible continuous values, combinations, animation frames or preset × parameter permutations. Audio telemetry is an explicit mock at t=0.7 s. Active press is a true MouseDown without release; a missing visual difference is a review finding, not an omitted state. Hover settles with a fixed 200 ms frame and every new capture settles content transitions with another fixed 250 ms frame. `menu-inventory.json` records the production Settings, parameter, editable wave and locked NES wave menu requests in both languages (16 cases), including enabled and checked flags; it is semantic evidence, not a rendered native-menu screenshot. Native macOS context menus and Logic's AU header belong to their host and require a separate foreground capture; these PNGs do not claim to cover them.\n\nRun from the repository root:\n\n```sh\ncargo run -p pixl-gui --release --example snapshots -- target/design-review/2026-10-13/before\ncargo run -p pixl-gui --release --example snapshots -- target/design-review/2026-10-13/after\n```\n\n`index.html` works from a local file without a server, dependencies, fonts or network. `manifest.json` records exact state names, dimensions, chip/channel/focus and notes. PNGs live in ignored build output; the generator and gallery template are versioned.\n",states.len());
    std::fs::write(dir.join("coverage.md"), coverage)
}
fn render_motion(dir: &Path, h: &mut Headless) -> Result<(), Box<dyn std::error::Error>> {
    for reel in ["focus", "macro", "arp"] {
        let folder = dir.join("motion").join(reel);
        std::fs::create_dir_all(&folder)?;
        let bridge = Arc::new(MockBridge::new());
        bridge.set_param_once(ParamId::Chip, 1.);
        bridge.animate(0.);
        let mut app = EditorApp::new(bridge.clone());
        app.ui.channel = Channel::Wave;
        if reel == "macro" {
            app.ui.focus = Some(Focus::Sweep);
        }
        if reel == "arp" {
            app.ui.focus = Some(Focus::Arp);
            bridge.set_param_once(ParamId::ArpOn, 1.);
            bridge.set_param_once(ParamId::ArpMode, 2.);
            bridge.set_param_once(ParamId::ArpChord, 0.);
        }
        let focus_target = hit(&app, |a| matches!(a, Action::Focus(Some(Focus::Vibrato))));
        for frame in 0..32 {
            bridge.animate(frame as f64 / 60.);
            if reel == "focus" {
                match frame {
                    0 => {
                        h.event(&mut app, &Event::MouseMove(MouseEvent::at(focus_target)));
                    }
                    6 => {
                        h.event(&mut app, &Event::MouseDown(mouse((focus_target.x, focus_target.y))));
                    }
                    10 => {
                        h.event(&mut app, &Event::MouseUp(mouse((focus_target.x, focus_target.y))));
                    }
                    24 => {
                        h.event(&mut app, &Event::MouseLeave);
                    }
                    _ => {}
                }
            }
            if reel == "macro" {
                bridge.set_param_once(ParamId::macro_value(0), frame as f32 / 31. * 100.);
            }
            if reel == "arp" {
                let mut t = bridge.telemetry();
                t.held_notes = [(1u64 << 60) | (1u64 << 64), 1u64 << 3];
                t.last_note = Some(60);
                t.arp_step = Some([0, 1, 2, 1][(frame / 4) % 4]);
                bridge.set_telemetry(t);
            }
            h.time_frame(&mut app, 2., 1. / 60.)?;
            h.render(&mut app, 2.)?.save_png(folder.join(format!("frame-{frame:03}.png")))?;
        }
        println!("Motion ready: {} (32 frames / 60 fps, explicit MockBridge input)", folder.display());
    }
    std::fs::write(dir.join("motion/manifest.json"),"{\"source\":\"Actual production renderer, synthetic MockBridge MIDI/parameter input; not live audio capture\",\"fps\":60,\"frames_per_reel\":32,\"reels\":[\"focus\",\"macro\",\"arp\"]}\n")?;
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "target/design-review/2026-10-13/before".into());
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir)?;
    let mut states = states();
    let mut harness = Headless::new()?;
    if std::env::args().any(|s| s == "--menus-only") {
        states.retain(|s| dir.join(format!("{}.png", s.name)).exists());
        write_menu_inventory(dir, &mut harness)?;
        write_manifest(dir, &states)?;
        write_gallery(dir, &states)?;
        println!("Native menu inventory ready: {}", dir.join("menu-inventory.json").display());
        return Ok(());
    }
    for (i, s) in states.iter().enumerate() {
        let mut app = setup(&mut harness, s);
        let path = dir.join(format!("{}.png", s.name));
        harness.time_frame(&mut app, s.scale * 2., 0.25)?;
        harness.render(&mut app, s.scale * 2.)?.save_png(&path)?;
        println!("{:03}/{} {}", i + 1, states.len(), path.display());
    }
    write_menu_inventory(dir, &mut harness)?;
    write_manifest(dir, &states)?;
    write_gallery(dir, &states)?;
    if std::env::args().any(|s| s == "--motion") {
        render_motion(dir, &mut harness)?;
    }
    println!("Atlas ready: {}", dir.join("index.html").display());
    Ok(())
}
