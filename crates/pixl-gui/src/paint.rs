use crate::{editor::{PLOT,TILES},Action,EditorApp};
use pixl_core::*;
use pixl_gfx::{rgb,rgba,Canvas,TextStyle,TextAlign,kurbo::{BezPath,Rect,Stroke}};
const BG:u32=0xF7F6F3;const INK:u32=0x161616;const A:u32=0xFF5A1F;const MUT:u32=0x6E6A63;const LINE:u32=0xE2DFD8;
pub(crate) fn text(c:&mut Canvas,r:Rect,s:&str,size:f32,color:u32,mono:bool){let style=if mono{TextStyle::mono(size).weight(500.).letter_spacing(0.06)}else{TextStyle::sans(size).weight(600.)};c.text_in(r,s,&style.color(rgb(color)).ellipsis(r.width() as f32));}
pub(crate) fn center(c:&mut Canvas,r:Rect,s:&str,size:f32,color:u32){c.text_in(r,s,&TextStyle::mono(size).weight(500.).color(rgb(color)).align(TextAlign::Center).ellipsis(r.width()as f32));}
fn label(c:&mut Canvas,x:f64,y:f64,s:&str){text(c,Rect::new(x,y,880.,y+18.),s,10.,MUT,true);}
pub(crate) fn path(c:&mut Canvas,pts:&[(f64,f64)],color:u32,dashed:bool){let mut p=BezPath::new();for(i,pt)in pts.iter().enumerate(){if i==0{p.move_to(*pt)}else{p.line_to(*pt)}}let stroke=if dashed{Stroke::new(1.3).with_dashes(0.,[4.,4.])}else{Stroke::new(2.5)};c.stroke_path(&p,&stroke,rgb(color));}
fn dot(c:&mut Canvas,p:(f64,f64),solid:bool){c.fill_circle(p,5.,rgb(if solid{A}else{0xFFFFFF}));if !solid{c.stroke_circle(p,5.,1.8,rgb(INK));}}
pub(crate) fn paint(app:&EditorApp,c:&mut Canvas){
    c.fill_rect(c.bounds(),rgb(BG));c.line((0.,52.),(900.,52.),1.,rgb(LINE));
    c.fill_rounded_rect(Rect::new(654.,10.,804.,42.),8.,rgb(0xECE9E3));
    c.fill_rounded_rect(Rect::new(20.,106.,880.,356.),10.,rgb(0xFFFFFF));c.stroke_rounded_rect(Rect::new(20.,106.,880.,356.),10.,1.,rgb(LINE));
    if !app.browser.open{c.push_opacity_clipped((1.-0.35*(app.transition/0.16).clamp(0.,1.).powi(3))as f32,&Rect::new(21.,106.,879.,350.));hero(app,c);c.pop_layer();}else{c.fill_rect(Rect::new(0.,52.,900.,560.),rgb(BG));}
    for control in app.base_controls(){let r=control.rect;let enabled=app.control_enabled(&control);let hovered=enabled&&r.contains(app.hover);match &control.action {
        Action::Undo|Action::Redo=>{c.fill_rounded_rect(r,5.,rgb(if hovered&&control.active{0xE7E4DE}else{0xF1EFEA}));center(c,r,&control.label,9.,if control.active{INK}else{0xC9C5BC});},
        Action::Settings=>{text(c,r,"PIXL",18.,INK,false);c.fill_rect(Rect::new(76.,23.,83.,30.),rgb(A));},
        Action::Toggle(id)=>{let p=if control.label.is_empty(){r.center()}else{c.fill_rounded_rect(r,5.,rgb(0xF1EFEA));text(c,Rect::new(r.x0+7.,r.y0,r.x1-14.,r.y1),&control.label,9.,INK,true);pixl_gfx::kurbo::Point::new(r.x1-8.,r.center().y)};if app.bridge.param(*id)>0.{c.fill_circle(p,3.5,rgb(A));}else{c.stroke_circle(p,3.5,1.5,rgb(0xC9C5BC));}},
        Action::Focus(Some(f)) if TILES.iter().any(|t|t.0==*f)&&r.y0>360.=>tile(app,c,control.rect,*f,&control.label,hovered),
        Action::Macro(i)=>{c.fill_rounded_rect(r,8.,rgb(if control.active{0xFFF1E9}else{0xFFFFFF}));c.stroke_rounded_rect(r,8.,1.,rgb(if control.active{A}else{LINE}));text(c,Rect::new(r.x0+9.,r.y0,r.x0+22.,r.y1),&format!("{}",i+1),10.,A,true);text(c,Rect::new(r.x0+28.,r.y0,r.x0+91.,r.y1),&control.label,11.,INK,false);let v=app.bridge.param(ParamId::macro_value(*i));let bar=Rect::new(r.x0+94.,r.y0+16.,r.x1-29.,r.y0+20.);c.fill_rounded_rect(bar,2.,rgb(0xE7E4DE));c.fill_rounded_rect(Rect::new(bar.x0,bar.y0,bar.x0+bar.width()*v as f64/100.,bar.y1),2.,rgb(if control.active{A}else{INK}));text(c,Rect::new(r.x1-25.,r.y0,r.x1-5.,r.y1),&format!("{v:.0}"),10.,INK,true);},
        Action::Param(ParamId::OutGain)=>{let t=app.bridge.telemetry();let clipping=t.peak_hold_db>=0.;for i in 0..2{let y=15.+i as f64*6.;c.fill_rounded_rect(Rect::new(r.x0,y,r.x1,y+3.),2.,rgb(LINE));let v=((t.rms_db[i]+60.)/60.).clamp(0.,1.);c.fill_rounded_rect(Rect::new(r.x0,y,r.x0+r.width()*v as f64,y+3.),2.,rgb(if clipping{A}else{INK}));}text(c,Rect::new(r.x0,27.,r.x1,42.),&if t.peak_hold_db< -99.{"−∞ DB".into()}else{format!("{:.1} DB",t.peak_hold_db)},9.,if clipping{A}else{MUT},true);},
        Action::Browser if r.y0<52.=>{c.fill_rounded_rect(r,16.,rgb(if control.active{0xFFFFFF}else{BG}));c.stroke_rounded_rect(r,16.,1.,rgb(LINE));let p=app.bridge.current_preset();text(c,Rect::new(r.x0+14.,r.y0,r.x0+105.,r.y1),&format!("{} / {}",p.category.tag(),if p.number==0{"USER".into()}else{format!("{:03}",p.number)}),10.,MUT,true);text(c,Rect::new(r.x0+116.,r.y0,r.x1-54.,r.y1),&p.name,13.,INK,false);if p.modified{c.fill_circle((r.x1-59.,r.center().y),2.5,rgb(A));}},
        Action::Channel(ch)=>{if control.active||hovered{c.fill_rounded_rect(r,8.,rgb(if control.active{INK}else{0xECE9E3}));}let on=app.bridge.param(ParamId::channel(*ch,ChannelField::On))>0.;text(c,Rect::new(r.x0+22.,r.y0,r.x1-3.,r.y1),&control.label,10.,if control.active{0xFFFFFF}else if on{INK}else{MUT},true);},
        Action::Select(id)=>{if control.active||hovered{c.fill_rounded_rect(r,7.,rgb(if control.active{INK}else{0xECE9E3}));}let fg=if control.active{0xFFFFFF}else{INK};if let Some(p)=app.bridge.preset_list().iter().find(|p|p.id==*id){text(c,Rect::new(r.x0+10.,r.y0,r.x0+52.,r.y1),&if p.is_factory{format!("{:03}",p.number)}else{"USER".into()},9.,if control.active{0xC9C5BC}else{MUT},true);text(c,Rect::new(r.x0+54.,r.y0,r.x1-62.,r.y1),&p.name,12.,fg,false);center(c,Rect::new(r.x1-54.,r.y0+8.,r.x1-8.,r.y1-8.),p.chip.label(),9.,if control.active{0xC9C5BC}else{MUT});}},
        _=>{let dark=enabled&&control.active&&!matches!(control.action,Action::Set(ParamId::Chip,_));let color=if dark {INK}else if matches!(control.action,Action::Set(ParamId::Chip,_))&&control.active{0xFFFFFF}else if hovered{0xE7E4DE}else{0xF1EFEA};c.fill_rounded_rect(r,if r.y0<52.{6.}else{5.},rgb(color));center(c,r.inflate(-6.,0.),&control.label,10.,if !enabled{0xA9A49A}else if dark{0xFFFFFF}else{INK});
            if let Action::Param(id)=control.action{assignment(app,c,r,id);}
        }
    }}
    if app.browser.open{browser_details(app,c);field(app,c);}else{
        label(c,20.,497.,"MACRO");
        c.line((718.,70.),(718.,88.),1.,rgb(LINE));text(c,Rect::new(728.,66.,880.,92.),match app.bridge.param(ParamId::Chip)as u8{0=>"RICOH 2A03 · 4 CH",1=>"DMG-CPU · 4 CH",_=>"FREE · 4 CH"},9.,MUT,true);
        text(c,Rect::new(20.,535.,620.,553.),app.text("SHIFT / ⌘ — ТОЧНО · ⌥ / ДВОЙНОЙ КЛИК — СБРОС","SHIFT / ⌘ — FINE · ⌥ / DOUBLE CLICK — RESET"),9.,MUT,true);
    }
    if app.value_entry.is_some(){value_entry(app,c);}
    if let Some(r)=app.press_rect {if app.press_opacity>0.{c.fill_rounded_rect(r,6.,rgba(A,(app.press_opacity*0.10)as f32));c.stroke_rounded_rect(r,6.,1.5,rgba(A,(app.press_opacity*0.7)as f32));}}
    if let Some(r)=app.hover_rect {if app.hover_opacity>0. {c.stroke_rounded_rect(r,6.,1.,rgba(A,(app.hover_opacity*0.35)as f32));}}

    if app.tip.is_empty()&&!app.browser.open&&app.value_entry.is_none(){if let Some(control)=app.base_controls().iter().rev().find(|c|c.rect.contains(app.hover)){if let Action::Param(id)=control.action{let base=app.bridge.param(id);let effective=app.bridge.param_modulated(id);if (base-effective).abs()>0.001{c.fill_rect(Rect::new(20.,535.,620.,553.),rgb(BG));text(c,Rect::new(20.,535.,620.,553.),&format!("BASE {} → MACRO {}",id.format(base),id.format(effective)),10.,A,true);}}}}
    if !app.tip.is_empty(){let x=app.hover.x.clamp(10.,690.);let y=app.hover.y.clamp(35.,500.)-30.;c.fill_rounded_rect(Rect::new(x,y,x+190.,y+25.),5.,rgb(INK));text(c,Rect::new(x+7.,y,x+183.,y+25.),&app.tip,10.,0xFFFFFF,true);}
}
fn assignment(app:&EditorApp,c:&mut Canvas,r:Rect,id:ParamId){if app.assign{if let Some(d)=app.bridge.macro_assignments().depth(app.ui.macro_selected as usize,id){c.stroke(&r.to_rounded_rect(5.),&Stroke::new(1.).with_dashes(0.,[3.,2.]),rgb(A));let badge=Rect::new(r.x1-72.,r.y0-10.,r.x1,r.y0+4.);c.fill_rounded_rect(badge,4.,rgb(A));text(c,badge,&format!("M{} {:+.0}%",app.ui.macro_selected+1,d*100.),9.,INK,true);}}}
fn tile(app:&EditorApp,c:&mut Canvas,r:Rect,f:Focus,name:&str,hovered:bool){let t=TILES.iter().find(|t|t.0==f).unwrap();let active=app.ui.focus==Some(f);let enabled=app.bridge.param(t.2)>0.;let fg=if active{0xFFFFFF}else{INK};c.fill_rounded_rect(r,10.,rgb(if active{INK}else if hovered{0xF1EFEA}else{0xFFFFFF}));c.stroke_rounded_rect(r,10.,1.,rgb(LINE));text(c,Rect::new(r.x0+12.,r.y0+6.,r.x1-22.,r.y0+24.),name,10.,if active{0xA9A49A}else{MUT},true);
    let value=app.bridge.param_modulated(t.3);let (big,unit)=match f {Focus::Sweep=>(format!("{value:.0}"),"ST".into()),Focus::Vibrato=>(format!("{:.1}",app.bridge.param_modulated(ParamId::VibRate)),"HZ".into()),Focus::Arp if app.bridge.param(ParamId::ArpSync)>0.=>(ParamId::ArpDiv.format(app.bridge.param(ParamId::ArpDiv)),ParamId::ArpMode.format(app.bridge.param(ParamId::ArpMode))),Focus::Arp=>(format!("{value:.0}"),"HZ".into()),Focus::Crush=>(format!("{value:.0}"),"BIT".into()),Focus::Glide=>(format!("{value:.0}"),"MS".into()),_=>(format!("{:.0}",app.bridge.param_modulated(ParamId::UniVoices)),"VOICES".into())};
    let color=if enabled{fg}else if active{0xA9A49A}else{0xC9C5BC};text(c,Rect::new(r.x0+12.,r.y1-34.,r.x0+78.,r.y1-5.),&big,21.,color,false);text(c,Rect::new(r.x0+68.,r.y1-28.,r.x1-5.,r.y1-6.),&unit,9.,if active{0xA9A49A}else{MUT},true);
    if f==Focus::Unison{let n=app.bridge.param_modulated(ParamId::UniVoices)as usize;let spread=app.bridge.param_modulated(ParamId::UniDetune)as f64/50.;for j in 0..n{let lane=if n<=1{0.5}else{j as f64/(n-1)as f64};let y=r.y0+39.+lane*28.;c.line((r.x0+12.,y),(r.x1-12.,y+(lane-0.5)*spread*10.),1.5,rgb(color));}assignment(app,c,r,t.3);return;}let curve=crate::curves::Curve::new(&*app.bridge,f,app.ui.channel);let mut pts=Vec::new();for i in 0..96{let x=i as f64/95.;let y=if let Some(curve)=&curve{curve.sample(x*curve.duration_ms)}else{((x*8.).floor()%4.)/4.};pts.push((r.x0+12.+x*(r.width()-24.),r.y0+68.-y*32.));}path(c,&pts,color,false);assignment(app,c,r,t.3);if(app.bridge.param(t.3)-value).abs()>0.001{c.fill_rect(Rect::new(r.x0+10.,r.y1-8.,r.x0+14.,r.y1-3.),rgb(A));}}
fn hero(app:&EditorApp,c:&mut Canvas){
    let chip=ChipMode::from_plain(app.bridge.param(ParamId::Chip));let wave_view=app.ui.focus.is_none()&&app.ui.channel==Channel::Wave;
    if !matches!(app.ui.focus,Some(Focus::Arp|Focus::Echo)){let rows=if wave_view{15}else{4};for j in 0..=rows{let y=PLOT.y0+j as f64/rows as f64*PLOT.height();c.line((21.,y),(879.,y),1.,rgb(0xEFEDE8));}for i in 1..32{let x=21.+i as f64*858./32.;c.line((x,PLOT.y0-8.),(x,PLOT.y1+8.),1.,rgb(0xEFEDE8));}}
    let get=|id|app.bridge.param_modulated(id)as f64; let mut pts=Vec::new();let mut ghost=Vec::new();
    let title=match app.ui.focus{Some(Focus::Env)=>format!("ENVELOPE · {}",app.ui.channel.label()),Some(Focus::Sweep)=>"PITCH SWEEP".into(),Some(Focus::Vibrato)=>"VIBRATO".into(),Some(Focus::Arp)=>"ARPEGGIATOR".into(),Some(Focus::Crush)=>"BITCRUSH".into(),Some(Focus::Glide)=>"GLIDE / PORTAMENTO".into(),Some(Focus::Unison)=>"UNISON".into(),Some(Focus::Echo)=>"STEREO ECHO".into(),None=>match app.ui.channel{Channel::Wave if chip==ChipMode::Nes=>"NES 2A03 · TRIANGLE · LOCKED".into(),Channel::Wave=>"32 × 4-BIT WAVE RAM".into(),Channel::P1|Channel::P2=>format!("{} · DUTY {}",app.ui.channel.label(),ParamId::duty(app.ui.channel).unwrap().format(app.bridge.param(ParamId::duty(app.ui.channel).unwrap()))),Channel::Noise=>format!("NOISE · LFSR {}-BIT",if get(ParamId::NsMode)>0.{7}else{15})}};
    let title_x=if app.ui.focus.is_some(){137.}else{34.};let title_end=app.base_controls().iter().filter(|c|c.rect.y0==116.&&c.rect.x0>title_x).map(|c|c.rect.x0).fold(850.,f64::min);text(c,Rect::new(title_x,116.,title_end-8.,138.),&title,10.,INK,true);
    if app.ui.focus.is_none()&&app.bridge.param(ParamId::channel(app.ui.channel,ChannelField::On))<=0.{text(c,Rect::new(34.,140.,600.,155.),"CHANNEL OFF · PREVIEW",9.,MUT,true);}
    if wave_view {
        let w=if chip==ChipMode::Nes{wave::NES_TRIANGLE}else{app.bridge.wave()};let step=858./32.;let telemetry=app.bridge.telemetry();let playing=if telemetry.active_voices>0{telemetry.wave_step}else{None};
        for(i,v)in w.iter().enumerate(){let x=21.+i as f64*step;let y=PLOT.y1-*v as f64/15.*PLOT.height();c.fill_rect(Rect::new(x+2.,y,x+step-2.,PLOT.y1+8.),rgba(A,0.13));if playing==Some(i as u8){c.fill_rect(Rect::new(x,PLOT.y0-6.,x+step,PLOT.y1+8.),rgba(A,0.08));}pts.push((x,y));pts.push((x+step,y));}
        let hovered=app.wave_editable()&&crate::editor::WAVE_PLOT.contains(app.hover);
        let index=((app.hover.x-21.)/step).floor().clamp(0.,31.)as usize;
        if hovered {
            let x=21.+index as f64*step;let y=PLOT.y1-w[index]as f64/15.*PLOT.height();
            c.fill_rect(Rect::new(x,PLOT.y0,x+step,PLOT.y1),rgba(A,0.07));
            c.line((x+step*0.5,PLOT.y0),(x+step*0.5,PLOT.y1),1.,rgba(A,0.35));
            dot(c,(x+step*0.5,y),true);
        }
        path(c,&pts,if chip==ChipMode::Nes{INK}else{A},false);
        let caption=if chip==ChipMode::Nes{app.text("ФИКСИРОВАННАЯ ФОРМА · БЕЗ ГРОМКОСТИ","FIXED WAVEFORM · NO VOLUME").into()}
            else if hovered{format!("{} {:02}/32 · {} {:02}/15",app.text("ШАГ","STEP"),index+1,app.text("УРОВЕНЬ","LEVEL"),w[index])}
            else{app.text("32 ШАГА · ПКМ — ИНСТРУМЕНТЫ","32 STEPS · RIGHT CLICK FOR TOOLS").into()};
        text(c,Rect::new(34.,329.,546.,347.),&caption,10.,MUT,true);return;
    }
    if app.ui.focus.is_none()&&matches!(app.ui.channel,Channel::P1|Channel::P2){let duty=get(ParamId::duty(app.ui.channel).unwrap())/100.;for k in 0..4{let x=PLOT.x0+k as f64*PLOT.width()/4.;pts.extend([(x,280.),(x,182.),(x+duty*PLOT.width()/4.,182.),(x+duty*PLOT.width()/4.,280.),(x+PLOT.width()/4.,280.)]);}for n in 1..=32{let h=((n as f64*std::f64::consts::PI*duty).sin()/n as f64).abs()*120.;let x=22.+(n-1)as f64*26.8;c.fill_rect(Rect::new(x+3.,328.-h,x+23.,328.),rgba(A,0.13));}dot(c,(PLOT.x0+duty*PLOT.width()/4.,231.),false);path(c,&pts,A,false);label(c,34.,329.,app.text("ГАРМОНИКИ","HARMONICS"));return;}
    if app.ui.focus.is_none()&&app.ui.channel==Channel::Noise{let register=app.bridge.telemetry().lfsr.max(1);let mut reg=register;for i in 0..160{let bit=(reg^(reg>>1))&1;reg=(reg>>1)|(bit<<14);if get(ParamId::NsMode)>0.{reg=(reg& !(1<<6))|(bit<<6);}let y=if reg&1==0{180.}else{265.};let x=21.+i as f64*858./160.;pts.push((x,y));pts.push((x+858./160.,y));}path(c,&pts,A,false);for i in 0..15{let x=34.+i as f64*16.;c.fill_rounded_rect(Rect::new(x,323.,x+14.,337.),3.,rgb(if register&(1<<i)>0{INK}else{0xFFFFFF}));c.stroke_rounded_rect(Rect::new(x,323.,x+14.,337.),3.,1.,rgb(if i<2{A}else{LINE}));}for i in 0..16{let x=620.+i as f64*14.;let h=6.+i as f64*1.6;c.fill_rounded_rect(Rect::new(x,337.-h,x+12.,337.),2.,rgb(if i==get(ParamId::NsPeriod)as usize{A}else{0xC9C5BC}));}return;}
    if app.ui.focus==Some(Focus::Echo){crate::plots::echo(app,c);return;}
    if app.ui.focus==Some(Focus::Arp){crate::plots::arp(app,c);return;}
    if app.ui.focus==Some(Focus::Unison){let voices=get(ParamId::UniVoices)as usize;for k in 0..voices {let det=if voices==1{0.}else{k as f64/(voices-1)as f64*2.-1.};let lane=PLOT.height()/voices as f64;let fr=6.*(1.+det*get(ParamId::UniDetune)*0.0025);let mut line=Vec::new();for i in 0..300{let x=i as f64/299.;let y=PLOT.y0+k as f64*lane+if(x*fr).fract()<0.5{lane*0.22}else{lane*0.78};line.push((PLOT.x0+x*PLOT.width(),y));}path(c,&line,if det.abs()<0.01{A}else{MUT},false);let badge=Rect::new(24.,PLOT.y0+k as f64*lane,82.,PLOT.y0+k as f64*lane+18.);c.fill_rounded_rect(badge,3.,rgba(0xFFFFFF,0.94));text(c,badge.inflate(-4.,0.),&format!("{:+.0} CT",det*get(ParamId::UniDetune)),9.,MUT,true);}return;}
    let Some(focus)=app.ui.focus else{return;};
    let Some(curve)=crate::curves::Curve::new(&*app.bridge,focus,app.ui.channel) else{return;};
    let duration=curve.duration_ms;
    let frame_hz=if chip==ChipMode::Gb{59.7275}else{FrameRate::from_plain(get(ParamId::FrameRate)as f32).hz()};
    for guide in &curve.guides{let points:Vec<_>=guide.iter().map(|(t,y)|(PLOT.x0+t/duration*PLOT.width(),PLOT.y1-y*PLOT.height())).collect();path(c,&points,0xC9C5BC,true);}
    for i in 0..=600{let t=i as f64/600.*duration;let x=PLOT.x0+i as f64/600.*PLOT.width();ghost.push((x,PLOT.y1-curve.reference(t)*PLOT.height()));let step=if chip==ChipMode::Free||!curve.frame_clocked{t}else{(t/(1000./frame_hz)).floor()*(1000./frame_hz)};let mut y=curve.sample(step);if focus==Focus::Env&&chip!=ChipMode::Free{y=(y*15.).round()/15.;}let y=PLOT.y1-y*PLOT.height();if let Some((_,last_y))=pts.last().copied(){if chip!=ChipMode::Free&&curve.frame_clocked{pts.push((x,last_y));}}pts.push((x,y));}
    path(c,&ghost,MUT,true);path(c,&pts,A,false);for(i,(t,y))in curve.handles.iter().enumerate(){dot(c,(PLOT.x0+t/duration*PLOT.width(),PLOT.y1-y*PLOT.height()),i==0);}
    let enabled=if focus==Focus::Env{app.bridge.param(ParamId::channel(app.ui.channel,ChannelField::On))>0.}else{TILES.iter().find(|t|t.0==focus).is_none_or(|t|app.bridge.param(t.2)>0.)};
    let modulated=app.focus_params().iter().any(|id|(app.bridge.param(*id)-app.bridge.param_modulated(*id)).abs()>0.001);
    let status=if !enabled{"BYPASSED · PARAMETER PREVIEW"}else if modulated{"BASE → MACRO · PARAMETER PREVIEW"}else{"PARAMETER PREVIEW"};
    text(c,Rect::new(34.,140.,590.,155.),status,9.,MUT,true);
    if focus==Focus::Env&&app.bridge.telemetry().active_voices>0&&enabled{let level=app.bridge.telemetry().env_level[app.ui.channel as usize]as f64;let y=PLOT.y1-level.clamp(0.,1.)*PLOT.height();c.line((PLOT.x0,y),(PLOT.x1,y),1.,rgba(A,0.25));text(c,Rect::new(720.,140.,874.,155.),&format!("LIVE LEVEL {:.0}%",level*100.),9.,A,true);}
    text(c,Rect::new(34.,329.,100.,347.),"0 MS",10.,MUT,true);
    center(c,Rect::new(780.,329.,874.,347.),&if duration<10.{format!("{duration:.2} MS")}else{format!("{duration:.0} MS")},10.,MUT);
    center(c,Rect::new(350.,329.,550.,347.),&if focus==Focus::Crush{"440 HZ · IDEAL S/H PREVIEW".into()}else if chip==ChipMode::Free{"CONTINUOUS".into()}else{format!("{frame_hz:.2} HZ FRAME")},9.,MUT);
}

fn browser_details(app:&EditorApp,c:&mut Canvas){
    // Fill only non-control regions so interactive rows and fields remain visible.
    c.line((170.,52.),(170.,560.),1.,rgb(LINE));c.line((640.,52.),(640.,560.),1.,rgb(LINE));
    text(c,Rect::new(182.,66.,530.,94.),&format!("{} · {}",app.text("ПРЕСЕТЫ","PRESETS"),app.filtered_presets().len()),10.,MUT,true);
    if let Some(p)=app.filtered_presets().iter().find(|p|Some(&p.id)==app.browser.selected.as_ref()){
        text(c,Rect::new(660.,108.,880.,144.),&p.name,20.,INK,false);label(c,660.,153.,&format!("{} / {} · {}",p.category.tag(),if p.number==0{"USER".into()}else{format!("{:03}",p.number)},p.chip.label()));
        text(c,Rect::new(660.,184.,880.,208.),&p.tags.join(" · "),10.,MUT,true);
        c.text((660.,228.),p.description.get(app.ui.lang),&TextStyle::sans(12.).color(rgb(MUT)).wrap(215.).line_height(1.5));label(c,660.,416.,&p.author);
    }else{label(c,200.,220.,app.text("Ничего не найдено","No matching presets"));}
    let n=app.filtered_presets().len();if n>10{let h=380.*10./n as f64;let y=108.+app.browser.scroll/(n as f64*38.)*380.;c.fill_rounded_rect(Rect::new(631.,y,634.,y+h),2.,rgb(0xC9C5BC));}
    if !app.browser.saving&&!app.browser.error.is_empty(){text(c,Rect::new(180.,488.,630.,509.),&app.browser.error,10.,A,false);}
    if app.browser.saving {c.fill_rect(Rect::new(0.,52.,900.,560.),rgba(INK,0.24));c.fill_rounded_rect(Rect::new(215.,155.,685.,397.),12.,rgb(BG));text(c,Rect::new(240.,165.,660.,194.),app.text("Сохранить пресет","Save preset"),18.,INK,false);
        for control in app.base_controls().iter().filter(|c|c.rect.y0>52.){c.fill_rounded_rect(control.rect,6.,rgb(if control.active{INK}else{0xFFFFFF}));c.stroke_rounded_rect(control.rect,6.,1.,rgb(LINE));center(c,control.rect.inflate(-8.,0.),&control.label,12.,if control.active{0xFFFFFF}else{INK});}
        text(c,Rect::new(240.,304.,660.,327.),&app.browser.error,10.,A,false);
    }
}

fn field(app:&EditorApp,c:&mut Canvas){let r=if app.browser.saving{Rect::new(240.,207.,660.,246.)}else{Rect::new(12.,68.,158.,102.)};let value=app.field();let style=TextStyle::sans(12.).color(rgb(INK));c.fill_rounded_rect(r,6.,rgb(0xFFFFFF));c.stroke_rounded_rect(r,6.,1.,rgb(LINE));c.push_clip(&r);let x=r.x0+8.;let y=r.center().y-8.;let layout=c.layout(value,&style);let mut caret=app.caret.min(value.len());while !value.is_char_boundary(caret){caret-=1;}let mut anchor=app.anchor.min(value.len());while !value.is_char_boundary(anchor){anchor-=1;}for rect in layout.selection_rects(anchor.min(caret)..anchor.max(caret)){c.fill_rect(rect+pixl_gfx::kurbo::Vec2::new(x,y),rgba(A,0.2));}c.text((x,y),if value.is_empty(){if app.browser.saving{app.text("Название пресета","Preset name")}else{app.text("Поиск…","Search…")}}else{value},&style);let cursor=layout.caret_rect(caret)+pixl_gfx::kurbo::Vec2::new(x,y);if app.text_editing{c.fill_rect(Rect::new(cursor.x0,cursor.y0,cursor.x0+1.,cursor.y1),rgb(A));}if !app.preedit.is_empty(){c.text((cursor.x0,y),&app.preedit,&style);let w=c.measure(&app.preedit,&style).width;c.line((cursor.x0,y+17.),(cursor.x0+w,y+17.),1.,rgb(A));}c.pop_clip();}

fn value_entry(app:&EditorApp,c:&mut Canvas){
    let Some(entry)=&app.value_entry else{return;};
    c.fill_rect(c.bounds(),rgba(INK,0.25));
    c.fill_rounded_rect(Rect::new(294.,181.,606.,376.),12.,rgb(BG));
    c.stroke_rounded_rect(Rect::new(294.,181.,606.,376.),12.,1.,rgb(LINE));
    text(c,Rect::new(320.,193.,580.,223.),entry.id.spec().name,17.,INK,false);
    let field=Rect::new(320.,235.,580.,275.);
    c.fill_rounded_rect(field,6.,rgb(0xFFFFFF));c.stroke_rounded_rect(field,6.,1.5,rgb(A));
    let style=TextStyle::sans(16.).color(rgb(INK));let layout=c.layout(&entry.text,&style);
    let mut caret=app.caret.min(entry.text.len());while !entry.text.is_char_boundary(caret){caret-=1;}
    let mut anchor=app.anchor.min(entry.text.len());while !entry.text.is_char_boundary(anchor){anchor-=1;}
    c.push_clip(&field.inflate(-8.,-2.));
    let offset=pixl_gfx::kurbo::Vec2::new(330.,245.);
    for rect in layout.selection_rects(anchor.min(caret)..anchor.max(caret)){c.fill_rect(rect+offset,rgba(A,0.2));}
    c.text((330.,245.),&entry.text,&style);
    let cursor=layout.caret_rect(caret)+offset;c.fill_rect(Rect::new(cursor.x0,cursor.y0,cursor.x0+1.,cursor.y1),rgb(A));
    if !app.preedit.is_empty(){c.text((cursor.x0,245.),&app.preedit,&style);}
    c.pop_clip();
    text(c,Rect::new(320.,281.,580.,307.),if entry.error.is_empty(){app.text("ENTER — ПРИМЕНИТЬ · ESC — ОТМЕНА","ENTER TO APPLY · ESC TO CANCEL")}else{&entry.error},9.,if entry.error.is_empty(){MUT}else{A},true);
    for control in app.controls().iter().filter(|c|!matches!(c.action,Action::ValueField)){
        c.fill_rounded_rect(control.rect,6.,rgb(if control.active{INK}else{0xECE9E3}));
        center(c,control.rect,&control.label,11.,if control.active{0xFFFFFF}else{INK});
    }
}
