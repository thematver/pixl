use pixl_core::*;
use crate::WaveEdit;
use pixl_gfx::{kurbo::{Point,Rect,Size}, App, Canvas, Cursor, Event, EventCtx, Key, Menu, MouseButton, TextCommand};
use std::sync::Arc;

pub(crate) const TILES: [(Focus, &str, ParamId, ParamId);6] = [
    (Focus::Sweep,"SWEEP",ParamId::SweepOn,ParamId::SweepAmt),
    (Focus::Vibrato,"VIBRATO",ParamId::VibOn,ParamId::VibDepth),
    (Focus::Arp,"ARP",ParamId::ArpOn,ParamId::ArpRate),
    (Focus::Crush,"CRUSH",ParamId::CrushOn,ParamId::CrushBits),
    (Focus::Glide,"GLIDE",ParamId::GlideOn,ParamId::GlideTime),
    (Focus::Unison,"UNISON",ParamId::UniOn,ParamId::UniDetune),
];
pub(crate) const WAVE_PLOT: Rect=Rect::new(21.0,158.0,879.0,320.0);
pub(crate) const PLOT: Rect=Rect::new(48.0,158.0,852.0,320.0);
/// Browser filters and inline save state, also useful to configure snapshots.
#[derive(Clone,Debug,Default)]
pub struct BrowserState {
    pub open:bool, pub search:String, pub category:Option<Category>, pub chip:Option<ChipMode>,
    pub favorites:bool, pub user_only:bool, pub alphabetical:bool, pub selected:Option<String>,
    pub scroll:f64, pub saving:bool, pub save_name:String, pub save_category:Category, pub error:String,
}
/// Semantic actions attached to native editor controls.
#[derive(Clone,Debug,PartialEq)]
pub enum Action {
    Compare, Param(ParamId), Set(ParamId,f32), Toggle(ParamId), Channel(Channel), Focus(Option<Focus>),
    WaveTool(WaveTool), WaveEdit(WaveEdit), Shape(u8), Browser, Previous(i32), Settings, Assign, Macro(usize),
    Search, Category(Option<Category>), ChipFilter(Option<ChipMode>), Favorites, User, Sort,
    Select(String), Load, Favorite(String), SaveForm, Save, Cancel, SaveName, SaveCategory(Category), Delete(String),
}
/// A single source of geometry for painting and hit testing.
#[derive(Clone,Debug)]
pub struct Control {pub rect:Rect,pub label:String,pub action:Action,pub active:bool}
#[derive(Clone)]
struct Drag { action:Action, origin:Point, last:Point, distance:f64, begun:bool, assignment:bool, ids:Vec<ParamId>, values:Vec<f32>, wave:WaveRam }
/// The complete editor state; sound and persisted UI state remain owned by Bridge.
pub struct EditorApp {
    comparison:Option<crate::history::Snapshot>, comparison_b:bool, history:crate::history::History, edit_before:Option<crate::history::Snapshot>,
    pub bridge:Arc<dyn Bridge>, pub ui:UiState, pub browser:BrowserState, pub assign:bool,
    pub(crate) hover:Point, drag:Option<Drag>, menu_param:Option<ParamId>, pub(crate) tip:String,
    last_values:Vec<f32>, last_telemetry:TelemetrySnapshot, last_preset:CurrentPreset, last_generation:u64,
    pub(crate) transition:f64,
    pub(crate) hover_rect:Option<Rect>, pub(crate) hover_opacity:f64, haptics:bool, haptic_cooldown:f64,
    text_editing:bool, pub(crate) caret:usize, pub(crate) anchor:usize, pub(crate) preedit:String, clipboard:String, wave_clipboard:String,
}
impl EditorApp {
    pub fn new(bridge:Arc<dyn Bridge>)->Self { let ui=bridge.ui_state(); let last_generation=bridge.state_generation(); Self {last_generation,comparison:None,comparison_b:false,history:crate::history::History::default(),edit_before:None,bridge,ui,browser:BrowserState::default(),assign:false,hover:Point::new(-1.0,-1.0),drag:None,menu_param:None,tip:String::new(),last_values:Vec::new(),last_telemetry:TelemetrySnapshot::default(),last_preset:CurrentPreset::default(),transition:0.0,hover_rect:None,hover_opacity:0.0,haptics:true,haptic_cooldown:0.0,text_editing:false,caret:0,anchor:0,preedit:String::new(),clipboard:String::new(),wave_clipboard:String::new()} }
    pub fn text<'a>(&self,ru:&'a str,en:&'a str)->&'a str {if self.ui.lang==Lang::Ru {ru}else{en}}
    pub fn persist(&self) {self.bridge.set_ui_state(self.ui.clone());}
    pub fn filtered_presets(&self)->Vec<PresetInfo> {
        let q=self.browser.search.to_lowercase(); let b=&self.browser;
        let mut p:Vec<_>=self.bridge.preset_list().into_iter().filter(|p| {
            b.category.is_none_or(|c|p.category==c) && b.chip.is_none_or(|c|p.chip==c) && (!b.favorites||p.favorite) && (!b.user_only||!p.is_factory) &&
            (q.is_empty() || format!("{} {} {} {}",p.name,p.tags.join(" "),p.description.ru,p.description.en).to_lowercase().contains(&q))
        }).collect();
        if b.alphabetical {p.sort_by_key(|p|p.name.to_lowercase());} p
    }
    pub fn open_browser(&mut self) { self.browser.open=true;self.browser.selected=self.bridge.current_preset().id;let list=self.filtered_presets();let i=list.iter().position(|p|Some(&p.id)==self.browser.selected.as_ref()).unwrap_or(0);self.browser.scroll=(i as f64*38.-190.).max(0.).min((list.len()as f64*38.-380.).max(0.));self.transition=0.16; }
    pub fn focus(&mut self,focus:Option<Focus>) {self.finish_drag(); self.ui.focus=focus;self.transition=0.16;self.persist();}
    pub fn controls(&self)->Vec<Control> {
        let mut out=Vec::new();
        let mut add=|r:Rect,label:String,action:Action,active:bool|out.push(Control{rect:r,label,action,active});
        add(Rect::new(20.,10.,89.,43.),"PIXL".into(),Action::Settings,false);
        let p=self.bridge.current_preset();
        add(Rect::new(256.,10.,592.,42.),format!("{} / {:03}   {}{}",p.category.tag(),p.number,p.name,if p.modified{" •"}else{""}),Action::Browser,self.browser.open);
        add(Rect::new(542.,14.,564.,38.),"‹".into(),Action::Previous(-1),false);
        add(Rect::new(566.,14.,588.,38.),"›".into(),Action::Previous(1),false);
        for (i,ch) in ChipMode::ALL.iter().enumerate() {add(Rect::new(657.+i as f64*48.,13.,703.+i as f64*48.,39.),ch.label().into(),Action::Set(ParamId::Chip,i as f32),self.bridge.param(ParamId::Chip)==i as f32);}
        add(Rect::new(833.,10.,880.,44.),String::new(),Action::Param(ParamId::OutGain),false);
        for (i,ch) in Channel::ALL.iter().enumerate() {
            let x=20.+i as f64*94.; let name=match ch {Channel::P1=>"PULSE 1",Channel::P2=>"PULSE 2",Channel::Wave if self.bridge.param(ParamId::Chip)==0.=>"TRIANGLE",Channel::Wave=>"WAVE",Channel::Noise=>"NOISE"};
            add(Rect::new(x,64.,x+90.,92.),name.into(),Action::Channel(*ch),self.ui.channel==*ch);
            add(Rect::new(x+3.,70.,x+19.,86.),String::new(),Action::Toggle(ParamId::channel(*ch,ChannelField::On)),false);
        }
        for (i,(f,l)) in [(ChannelField::Level,"VOL"),(ChannelField::Pan,"PAN"),(ChannelField::Coarse,"ST"),(ChannelField::Fine,"CT")].iter().enumerate() {
            let id=ParamId::channel(self.ui.channel,*f); if *f==ChannelField::Level && self.ui.channel==Channel::Wave && self.bridge.param(ParamId::Chip)==0. {continue;}
            add(Rect::new(410.+i as f64*75.,67.,481.+i as f64*75.,90.),format!("{l} {}",id.format(self.bridge.param(id))),Action::Param(id),false);
        }
        if self.browser.open { out.retain(|c|c.rect.y0<52.); self.browser_controls(&mut out); return out; }
        if self.ui.focus.is_some() {add(Rect::new(34.,116.,126.,138.),self.text("← КАНАЛ","← CHANNEL").into(),Action::Focus(None),false);}
        let ids=self.focus_params(); let mut x=868.;
        let widths: Vec<f64> = ids.iter().map(|id| { let label=format!("{} {}",id.spec().short,id.format(self.bridge.param(*id))); (label.chars().count() as f64*6.3+16.).clamp(42.,120.) }).collect();
        let fit = (570.0 / widths.iter().sum::<f64>().max(1.0)).min(1.0);
        for (id, width) in ids.iter().zip(&widths).rev() {let label=format!("{} {}",id.spec().short,id.format(self.bridge.param(*id)));let w=width*fit; x-=w;
            add(Rect::new(x,116.,x+w-4.,138.),label,Action::Param(*id),false);
        }
        if self.ui.focus.is_none() {
            add(Rect::new(808.,116.,865.,138.),"ENV".into(),Action::Focus(Some(Focus::Env)),false);
            match self.ui.channel {
                Channel::Wave if self.bridge.param(ParamId::Chip)!=0. => { for (i,l) in ["DRAW","LINE","SMOOTH","INVERT","SIN","TRI","SAW","SQR"].iter().enumerate() { let a=match i {0=>Action::WaveTool(WaveTool::Draw),1=>Action::WaveTool(WaveTool::Line),_=>Action::Shape(i as u8-2)}; add(Rect::new(336.+i as f64*58.,116.,390.+i as f64*58.,138.),(*l).into(),a,(i==0&&self.ui.wave_tool==WaveTool::Draw)||(i==1&&self.ui.wave_tool==WaveTool::Line));} }
                Channel::P1|Channel::P2=> {let id=ParamId::duty(self.ui.channel).unwrap(); for (i,v) in [12.5,25.,50.,75.].iter().enumerate(){add(Rect::new(550.+i as f64*62.,116.,607.+i as f64*62.,138.),format!("{v}%"),Action::Set(id,*v),self.bridge.param(id)==*v);} }
                Channel::Noise=>{for (i,l) in ["LONG","SHORT"].iter().enumerate(){add(Rect::new(580.+i as f64*72.,116.,647.+i as f64*72.,138.),(*l).into(),Action::Set(ParamId::NsMode,i as f32),self.bridge.param(ParamId::NsMode)==i as f32);}add(Rect::new(726.,116.,797.,138.),"TRACK".into(),Action::Toggle(ParamId::NsTrack),self.bridge.param(ParamId::NsTrack)>0.);}
                _=>{}
            }
        }
        if self.wave_editable() {
            for (i,(label,operation)) in [("−1",WaveEdit::ShiftLeft),("+1",WaveEdit::ShiftRight),("REV",WaveEdit::Reverse),("NORM",WaveEdit::Normalize),("COPY",WaveEdit::Copy),("PASTE",WaveEdit::Paste)].iter().enumerate() {
                let x=554.+i as f64*52.;
                add(Rect::new(x,329.,x+48.,349.),(*label).into(),Action::WaveEdit(*operation),false);
            }
        }
        for (i,(focus,name,on,id)) in TILES.iter().enumerate() {
            let x=20.+i as f64*145.; add(Rect::new(x,368.,x+135.,478.),(*name).into(),Action::Focus(Some(*focus)),self.ui.focus==Some(*focus));
            add(Rect::new(x+113.,375.,x+129.,391.),String::new(),Action::Toggle(*on),false);
            // The tile body is both a click target and a drag-value target.
            let _=id;
        }
        for i in 0..4 {add(Rect::new(76.+i as f64*157.,490.,225.+i as f64*157.,525.),pixl_core::macros::MACRO_NAMES[i].into(),Action::Macro(i),self.assign&&self.ui.macro_selected as usize==i);}
        add(Rect::new(710.,490.,782.,525.),"ECHO".into(),Action::Focus(Some(Focus::Echo)),self.ui.focus==Some(Focus::Echo));
        add(Rect::new(765.,499.,781.,515.),String::new(),Action::Toggle(ParamId::EchoOn),false);
        add(Rect::new(790.,490.,880.,525.),if self.assign {self.text("ГОТОВО","DONE")}else{"ASSIGN"}.into(),Action::Assign,self.assign);
        add(Rect::new(774.,535.,880.,554.),if self.comparison.is_none(){"A → B"}else if self.comparison_b{"B → A"}else{"A → B"}.into(),Action::Compare,self.comparison_b);
        out
    }
    fn browser_controls(&self,out:&mut Vec<Control>) {
        let mut add=|r:Rect,label:String,action:Action,active:bool|out.push(Control{rect:r,label,action,active});
        add(Rect::new(12.,68.,158.,102.),if self.browser.search.is_empty(){self.text("Поиск…","Search…").into()}else{self.browser.search.clone()},Action::Search,false);
        let all=self.bridge.preset_list();
        add(Rect::new(12.,112.,158.,141.),format!("{}    {}",self.text("Все","All"),all.len()),Action::Category(None),self.browser.category.is_none()&&!self.browser.user_only);
        for (i,c) in Category::ALL.iter().enumerate(){add(Rect::new(12.,145.+i as f64*32.,158.,174.+i as f64*32.),format!("{}    {}",c.title(),all.iter().filter(|p|p.category==*c).count()),Action::Category(Some(*c)),self.browser.category==Some(*c));}
        if all.iter().any(|p|!p.is_factory){add(Rect::new(12.,342.,158.,371.),self.text("Мои","User").into(),Action::User,self.browser.user_only);}
        for (i,c) in ChipMode::ALL.iter().enumerate(){add(Rect::new(12.+i as f64*49.,468.,57.+i as f64*49.,493.),c.label().into(),Action::ChipFilter(Some(*c)),self.browser.chip==Some(*c));}
        add(Rect::new(12.,504.,158.,532.),self.text("★ Избранное","★ Favorites").into(),Action::Favorites,self.browser.favorites);
        add(Rect::new(537.,66.,622.,94.),if self.browser.alphabetical{"A–Z ↓"}else{"№ ↓"}.into(),Action::Sort,false);
        for (i,p) in self.filtered_presets().iter().enumerate(){let y=108.+i as f64*38.-self.browser.scroll;if (108.0..484.).contains(&y){add(Rect::new(180.,y,628.,y+36.),format!("{:03}   {}   {}",p.number,p.name,p.chip.label()),Action::Select(p.id.clone()),self.browser.selected.as_deref()==Some(&p.id));}}
        add(Rect::new(660.,486.,802.,524.),self.text("Загрузить","Load").into(),Action::Load,true);
        add(Rect::new(812.,486.,880.,524.),"ESC".into(),Action::Browser,false);
        add(Rect::new(180.,510.,421.,540.),self.text("Сохранить текущий…","Save current…").into(),Action::SaveForm,false);
        if let Some(p)=all.iter().find(|p|Some(&p.id)==self.browser.selected.as_ref()){
            add(Rect::new(844.,68.,882.,100.),if p.favorite{"★"}else{"☆"}.into(),Action::Favorite(p.id.clone()),p.favorite);
            if !p.is_factory {add(Rect::new(455.,510.,625.,540.),self.text("Удалить","Delete").into(),Action::Delete(p.id.clone()),false);}
        }
        if self.browser.saving {
            // Appended last: the form owns all hits in its opaque panel.
            out.retain(|c|c.rect.y0<52.);
            add_form(out,self);
        }
    }
    pub(crate) fn focus_params(&self)->Vec<ParamId> {use ParamId::*;match self.ui.focus {
        Some(Focus::Env)=>[ChannelField::Attack,ChannelField::Decay,ChannelField::Sustain,ChannelField::Release].iter().map(|f|ParamId::channel(self.ui.channel,*f)).collect(),
        Some(Focus::Sweep)=>vec![SweepAmt,SweepTime,SweepDir,SweepCurve],Some(Focus::Vibrato)=>vec![VibRate,VibDepth,VibDelay,VibFade,VibShape],
        Some(Focus::Arp)=>vec![ArpMode,ArpChord,ArpOct,if self.bridge.param(ArpSync)>0.{ArpDiv}else{ArpRate},ArpSync],
        Some(Focus::Crush)=>vec![CrushBits,CrushRate],Some(Focus::Glide)=>vec![GlideTime,GlideMode,GlideCurve],
        Some(Focus::Unison)=>vec![UniVoices,UniDetune,UniWidth,VoiceMode,Polyphony],
        Some(Focus::Echo)=>vec![if self.bridge.param(EchoSync)>0.{EchoDiv}else{EchoTime},EchoFb,EchoMix,EchoFir,EchoWidth,EchoSync],None=>vec![]}}
    fn param_for(&self,a:&Action)->Option<ParamId>{match a {Action::Param(id)|Action::Set(id,_)|Action::Toggle(id)=>Some(*id),Action::Macro(i)=>Some(ParamId::macro_value(*i)),Action::Focus(Some(f))=>TILES.iter().find(|t|t.0==*f).map(|t|t.3),_=>None}}
    fn hit(&self,p:Point)->Option<Control>{self.controls().into_iter().rev().find(|c|c.rect.contains(p))}
    fn finish_drag(&mut self){if let Some(d)=self.drag.take(){if d.begun&&!d.assignment {for id in d.ids {self.bridge.end_edit(id);}}} self.tip.clear();}
    fn activate(&mut self,a:Action,ctx:&mut EventCtx){match a {
        Action::Compare=>{let current=crate::history::Snapshot::capture(&*self.bridge);if let Some(other)=self.comparison.take(){other.restore_changes(&current,&*self.bridge);self.comparison_b=!self.comparison_b;}else{self.comparison_b=true;}self.comparison=Some(current);self.history.clear();self.edit_before=None;if self.haptics{ctx.haptic_alignment();}},
        Action::Set(id,v)=>self.bridge.set_param_once(id,v), Action::Toggle(id)=>self.bridge.set_param_once(id,if self.bridge.param(id)>0.{0.}else{1.}),
        Action::Param(id)=>{match id.spec().kind{ParamKind::Bool=>self.bridge.set_param_once(id,1.-self.bridge.param(id)),ParamKind::Enum{variants}=>self.bridge.set_param_once(id,(self.bridge.param(id)+1.)%variants.len() as f32),_=>{}}}
        Action::Channel(c)=>{self.ui.channel=c;self.ui.focus=None;self.transition=0.16;self.persist();},
        Action::Focus(f)=>self.focus(if self.ui.focus==f{None}else{f}),Action::WaveTool(t)=>{self.ui.wave_tool=t;self.persist();},
        Action::WaveEdit(operation)=>self.edit_wave(operation),
        Action::Shape(s)=>{let w=self.bridge.wave();self.bridge.set_wave(match s{0=>wave::smooth(&w),1=>wave::invert(&w),2=>wave::sine(),3=>wave::triangle(),4=>wave::saw(),_=>wave::square()});},
        Action::Browser=>{if self.browser.open{self.browser.open=false;self.text_editing=false;ctx.blur_text_input();}else{self.open_browser();self.text_editing=false;ctx.blur_text_input();}},
        Action::Previous(delta)=>{let p=self.filtered_presets();if !p.is_empty(){let id=self.bridge.current_preset().id;let i=p.iter().position(|p|Some(&p.id)==id.as_ref()).unwrap_or(0);let i=(i as i32+delta).rem_euclid(p.len() as i32) as usize;self.result(self.bridge.load_preset(&p[i].id));}},
        Action::Settings=>ctx.show_context_menu(self.settings_menu(),Point::new(20.,44.)),
        Action::Assign=>{self.assign=!self.assign;self.persist();},Action::Macro(i)=>{self.ui.macro_selected=i as u8;self.persist();},
        Action::Search=>{let layout=ctx.layout(&self.browser.search,&pixl_gfx::TextStyle::sans(12.));self.caret=layout.hit_test(Point::new((self.hover.x-20.).max(0.),8.));self.anchor=self.caret;self.text_editing=true;ctx.focus_text_input();ctx.set_text_input_area(Rect::new(24.,73.,146.,95.));},
        Action::Category(c)=>{self.browser.category=c;self.browser.user_only=false;self.filter_changed();},Action::ChipFilter(c)=>{self.browser.chip=if self.browser.chip==c{None}else{c};self.filter_changed();},
        Action::Favorites=>{self.browser.favorites=!self.browser.favorites;self.filter_changed();},Action::User=>{self.browser.user_only=!self.browser.user_only;self.browser.category=None;self.filter_changed();},Action::Sort=>{self.browser.alphabetical=!self.browser.alphabetical;self.filter_changed();},
        Action::Select(id)=>self.browser.selected=Some(id),Action::Load=>self.load_selected(ctx),Action::Favorite(id)=>{if let Some(p)=self.bridge.preset_list().iter().find(|p|p.id==id){self.bridge.set_favorite(&id,!p.favorite);}},
        Action::SaveForm=>{self.browser.saving=true;self.browser.save_name=self.bridge.current_preset().name;self.browser.error.clear();self.caret=self.browser.save_name.len();self.anchor=0;self.text_editing=true;ctx.focus_text_input();},Action::SaveName=>{let layout=ctx.layout(&self.browser.save_name,&pixl_gfx::TextStyle::sans(12.));self.caret=layout.hit_test(Point::new((self.hover.x-248.).max(0.),8.));self.anchor=self.caret;self.text_editing=true;ctx.focus_text_input();},Action::SaveCategory(c)=>self.browser.save_category=c,
        Action::Save=>{match self.bridge.save_user_preset(&self.browser.save_name,self.browser.save_category){Ok(id)=>{self.browser.saving=false;self.browser.selected=Some(id);self.browser.search.clear();self.browser.category=None;self.browser.chip=None;self.browser.favorites=false;self.browser.user_only=true;self.browser.scroll=0.;},Err(e)=>self.browser.error=e}},Action::Cancel=>self.browser.saving=false,
        Action::Delete(id)=>{self.result(self.bridge.delete_user_preset(&id));self.filter_changed();},
    }ctx.request_redraw();}
    fn result(&mut self,r:Result<(),String>){if let Err(e)=r{self.browser.error=e;}}
    fn load_selected(&mut self,ctx:&mut EventCtx){if let Some(id)=self.browser.selected.clone(){match self.bridge.load_preset(&id){Ok(())=>{self.browser.open=false;self.text_editing=false;ctx.blur_text_input();},Err(e)=>self.browser.error=e}}}
    fn filter_changed(&mut self){self.browser.scroll=0.;self.browser.selected=self.filtered_presets().first().map(|p|p.id.clone());}
    fn settings_menu(&self)->Menu{let mut m=Menu::new();m=if self.history.can_undo(){m.item(170,self.text("Отменить изменение","Undo edit"))}else{m.disabled_item(170,self.text("Отменить изменение","Undo edit"))};m=if self.history.can_redo(){m.item(171,self.text("Повторить изменение","Redo edit"))}else{m.disabled_item(171,self.text("Повторить изменение","Redo edit"))};m=m.separator();for (i,s) in bridge::UI_SCALES.iter().enumerate(){m=m.checked_item(100+i as u32,format!("UI {}%",(s*100.) as u32),self.ui.scale==*s);}m=m.checked_item(110,"Русский",self.ui.lang==Lang::Ru).checked_item(111,"English",self.ui.lang==Lang::En).separator().item(120,"NTSC 60 Hz").item(121,"PAL 50 Hz");for (i,v) in [1,2,3,5,7,12,24].iter().enumerate(){m=m.item(130+i as u32,format!("Pitch bend ±{v}"));}for (i,v) in [0,25,50,75,100].iter().enumerate(){m=m.item(140+i as u32,format!("Velocity {v}%"));}m.checked_item(151,self.text("Тактильный отклик","Haptic feedback"),self.haptics).item(150,if self.bridge.param(ParamId::Analog)>0.{"✓ Analog output"}else{"Analog output"}).separator().disabled_item(160,"PIXL v1.0.0 · PIXL Audio")}
    fn menu_selected(&mut self,n:u32){let before=crate::history::Snapshot::capture(&*self.bridge);match n {200..=206=>self.edit_wave([WaveEdit::ShiftLeft,WaveEdit::ShiftRight,WaveEdit::Reverse,WaveEdit::Normalize,WaveEdit::Copy,WaveEdit::Paste,WaveEdit::Reset][(n-200)as usize]),170=>self.history.undo(&*self.bridge),171=>self.history.redo(&*self.bridge),100..=105=>{let s=bridge::UI_SCALES[(n-100) as usize];if self.bridge.request_resize(s){self.ui.scale=s;self.persist();}},110|111=>{self.ui.lang=if n==110{Lang::Ru}else{Lang::En};self.persist();},120|121=>self.bridge.set_param_once(ParamId::FrameRate,(n-120) as f32),130..=136=>self.bridge.set_param_once(ParamId::PbRange,[1.,2.,3.,5.,7.,12.,24.][(n-130)as usize]),140..=144=>self.bridge.set_param_once(ParamId::VelSens,(n-140)as f32*25.),151=>self.haptics=!self.haptics,150=>self.bridge.set_param_once(ParamId::Analog,1.-self.bridge.param(ParamId::Analog)),1=>{if let Some(id)=self.menu_param{self.bridge.reset_param(id);}},10..=13=>{if let Some(id)=self.menu_param{self.bridge.set_macro_assignment((n-10)as usize,id,0.25);}},20=>{if let Some(id)=self.menu_param{for i in 0..4{self.bridge.set_macro_assignment(i,id,0.);}}},_=>{}}if n!=170&&n!=171{self.history.record(before,crate::history::Snapshot::capture(&*self.bridge));}}
    pub(crate) fn wave_editable(&self)->bool {
        !self.browser.open && self.ui.focus.is_none() && self.ui.channel==Channel::Wave && self.bridge.param(ParamId::Chip)!=0.
    }
    fn read_wave_clipboard(&self)->String {
        #[cfg(target_os="macos")]
        if let Some(text)=pixl_gfx::clipboard::read(){return text;}
        self.wave_clipboard.clone()
    }
    fn edit_wave(&mut self,operation:WaveEdit) {
        if !self.wave_editable(){return;}
        match operation {
            WaveEdit::Copy=>{
                self.wave_clipboard=crate::wave_edit::encode(&self.bridge.wave());
                #[cfg(target_os="macos")]
                if pixl_gfx::clipboard::write(&self.wave_clipboard).is_err(){self.tip=self.text("Не удалось скопировать форму","Could not copy waveform").into();}
            },
            WaveEdit::Paste=>match crate::wave_edit::decode(&self.read_wave_clipboard()) {
                Some(wave)=>self.bridge.set_wave(wave),
                None=>self.tip=self.text("Нужны 32 hex-шага (0–F)","Expected 32 hex steps (0–F)").into(),
            },
            _=>self.bridge.set_wave(crate::wave_edit::transform(self.bridge.wave(),operation)),
        }
    }
    fn wave_menu(&self)->Menu {
        let menu=Menu::new().item(200,self.text("Сдвинуть на шаг влево","Shift one step left"))
            .item(201,self.text("Сдвинуть на шаг вправо","Shift one step right"))
            .item(202,self.text("Развернуть","Reverse"))
            .item(203,self.text("Нормализовать 0–15","Normalize to 0–15")).separator()
            .item(204,self.text("Копировать форму","Copy waveform"));
        let menu=if crate::wave_edit::decode(&self.read_wave_clipboard()).is_some(){menu.item(205,self.text("Вставить форму","Paste waveform"))}else{menu.disabled_item(205,self.text("Вставить форму","Paste waveform"))};
        menu.separator().item(206,self.text("Исходная форма","Default waveform"))
    }
    fn wave_point(p:Point)->(usize,u8){let x=((p.x-WAVE_PLOT.x0)/WAVE_PLOT.width()*32.).floor().clamp(0.,31.)as usize;let y=((PLOT.y1-p.y)/PLOT.height()*15.).round().clamp(0.,15.)as u8;(x,y)}
    fn draw_wave(&self,d:&Drag,p:Point){let (a,av)=Self::wave_point(if self.ui.wave_tool==WaveTool::Line{d.origin}else{d.last});let (b,bv)=Self::wave_point(p);let mut w=if self.ui.wave_tool==WaveTool::Line{d.wave}else{self.bridge.wave()};for (i,v) in w.iter_mut().enumerate().take(a.max(b)+1).skip(a.min(b)){let t=if a==b{1.}else{(i as f64-a as f64)/(b as f64-a as f64)};*v=(av as f64+(bv as f64-av as f64)*t).round()as u8;}self.bridge.set_wave(w);}
}
fn add_form(out:&mut Vec<Control>,app:&EditorApp){let mut add=|r,label,action|out.push(Control{rect:r,label,action,active:false});add(Rect::new(240.,207.,660.,246.),app.browser.save_name.clone(),Action::SaveName);for(i,c)in Category::ALL.iter().enumerate(){add(Rect::new(240.+i as f64*70.,264.,305.+i as f64*70.,294.),c.title().into(),Action::SaveCategory(*c));}add(Rect::new(440.,334.,540.,369.),app.text("Сохранить","Save").into(),Action::Save);add(Rect::new(552.,334.,660.,369.),app.text("Отмена","Cancel").into(),Action::Cancel);}
impl App for EditorApp {
    fn size(&self)->Size{Size::new(900.,560.)}
    fn event(&mut self,event:&Event,ctx:&mut EventCtx){match event {
        Event::MouseDown(e)=>{
            self.finish_drag();self.edit_before=Some(crate::history::Snapshot::capture(&*self.bridge));self.hover=e.pos;
            if e.button==Some(MouseButton::Right)&&self.wave_editable()&&WAVE_PLOT.contains(e.pos){
                self.edit_before=None;ctx.show_context_menu(self.wave_menu(),e.pos);return;
            }
            if let Some(c)=self.hit(e.pos){if !matches!(c.action,Action::Search|Action::SaveName){self.text_editing=false;ctx.blur_text_input();}let id=self.param_for(&c.action);
                if e.button==Some(MouseButton::Right){if let Some(id)=id {self.menu_param=Some(id);let mut m=Menu::new().item(1,self.text("Сбросить","Reset"));if id.spec().modulatable{for i in 0..4{m=m.item(10+i,format!("{} {}",self.text("Назначить на макро","Assign to macro"),i+1));}m=m.item(20,self.text("Убрать из макро","Remove from macros"));}ctx.show_context_menu(m,e.pos);}return;}
                if e.button!=Some(MouseButton::Left){return;}
                if let Some(id)=id{if e.mods.alt||e.click_count>=2 {if self.assign&&id.spec().modulatable{self.bridge.set_macro_assignment(self.ui.macro_selected as usize,id,0.);}else{self.bridge.reset_param(id);}ctx.request_redraw();return;}}
                if e.click_count>=2{if let Action::Select(id)=c.action{self.browser.selected=Some(id);self.load_selected(ctx);ctx.request_redraw();return;}}
                self.drag=Some(Drag{action:c.action.clone(),origin:e.pos,last:e.pos,distance:0.,begun:false,assignment:self.assign&&id.is_some_and(|id|id.spec().modulatable),ids:id.filter(|_|matches!(c.action,Action::Param(_)|Action::Macro(_)|Action::Focus(_))).into_iter().collect(),values:id.into_iter().map(|id|if self.assign&&id.spec().modulatable{self.bridge.macro_assignments().depth(self.ui.macro_selected as usize,id).unwrap_or(0.)}else{id.normalize(self.bridge.param(id))}).collect(),wave:self.bridge.wave()});
            }else if e.button==Some(MouseButton::Left)&&!self.browser.open&&(if self.ui.focus.is_none()&&self.ui.channel==Channel::Wave{WAVE_PLOT}else{PLOT}).contains(e.pos){
                let ids=match self.ui.focus {
                    Some(Focus::Env)=>{let ch=self.ui.channel; let f=if e.pos.x<280.{ChannelField::Attack}else if e.pos.x<600.{ChannelField::Decay}else{ChannelField::Release};let mut ids=vec![ParamId::channel(ch,f)];if f==ChannelField::Decay{ids.push(ParamId::channel(ch,ChannelField::Sustain));}ids},
                    Some(Focus::Sweep)=>if e.pos.x<450.{vec![ParamId::SweepAmt]}else{vec![ParamId::SweepTime]},
                    Some(Focus::Vibrato)=>if e.pos.x<320.{vec![ParamId::VibDelay]}else{vec![ParamId::VibFade,ParamId::VibDepth]},
                    Some(Focus::Echo)=>if e.pos.x<500.{vec![if self.bridge.param(ParamId::EchoSync)>0.{ParamId::EchoDiv}else{ParamId::EchoTime},ParamId::EchoMix]}else{vec![ParamId::EchoFb]},
                    Some(Focus::Crush)=>vec![ParamId::CrushRate,ParamId::CrushBits],Some(Focus::Glide)=>vec![ParamId::GlideTime],
                    Some(Focus::Unison)=>vec![ParamId::UniDetune,ParamId::UniWidth],Some(Focus::Arp)=>vec![if self.bridge.param(ParamId::ArpSync)>0.{ParamId::ArpDiv}else{ParamId::ArpRate}],
                    None=>match self.ui.channel{Channel::P1|Channel::P2=>vec![ParamId::duty(self.ui.channel).unwrap()],Channel::Noise=>vec![ParamId::NsPeriod],_=>vec![]},
                };
                let d=Drag{action:Action::Focus(self.ui.focus),origin:e.pos,last:e.pos,distance:0.,begun:!ids.is_empty(),assignment:false,values:ids.iter().map(|id|id.normalize(self.bridge.param(*id))).collect(),ids,wave:self.bridge.wave()};
                if self.ui.focus.is_none()&&self.ui.channel==Channel::Wave&&self.bridge.param(ParamId::Chip)!=0.{self.draw_wave(&d,e.pos);}else{for id in &d.ids{self.bridge.begin_edit(*id);}if !d.ids.is_empty(){ctx.begin_infinite_drag();}}
                self.drag=Some(d);
            }ctx.request_redraw();
        }
        Event::MouseDrag(e)=>{if let Some(mut d)=self.drag.take(){
            d.distance+=(e.pos-d.last).hypot().max(e.delta.hypot());
            if d.ids.is_empty(){if matches!(d.action,Action::Focus(None))&&self.ui.focus.is_none()&&self.ui.channel==Channel::Wave&&!self.browser.open&&self.bridge.param(ParamId::Chip)!=0.{self.draw_wave(&d,e.pos);}}
            else if d.distance>=3.||d.begun{
                if !d.begun{if !d.assignment {for id in &d.ids {self.bridge.begin_edit(*id);}}d.begun=true;ctx.begin_infinite_drag();}
                let fine=if e.mods.fine(){0.1}else{1.};
                for (i,id) in d.ids.iter().enumerate(){let horizontal=matches!(d.action,Action::Macro(_)) || (PLOT.contains(d.origin)&&((d.ids.len()>1&&i==0)||matches!(id,ParamId::SweepTime|ParamId::GlideTime|ParamId::VibDelay|ParamId::VibFade|ParamId::P1Attack|ParamId::P2Attack|ParamId::WvAttack|ParamId::NsAttack|ParamId::P1Release|ParamId::P2Release|ParamId::WvRelease|ParamId::NsRelease|ParamId::P1Duty|ParamId::P2Duty|ParamId::NsPeriod|ParamId::UniWidth)));
                    let delta=(if horizontal{e.delta.x}else{-e.delta.y})as f32*0.005*fine;
                    if d.assignment {let m=self.ui.macro_selected as usize;d.values[i]=(d.values[i]+delta).clamp(-1.,1.);let depth=d.values[i];if self.bridge.set_macro_assignment(m,*id,depth){self.tip=format!("M{} {:+.0}%",m+1,depth*100.);}else{self.tip=self.text("Нет свободных слотов","No free routing slots").into();}}
                    else{d.values[i]=(d.values[i]+delta).clamp(0.,1.);let mut v=id.denormalize(d.values[i]);if matches!(id,ParamId::P1Duty|ParamId::P2Duty)&&self.bridge.param(ParamId::Chip)!=2.{v=[12.5f32,25.,50.,75.].into_iter().min_by(|a,b|(a-v).abs().total_cmp(&(b-v).abs())).unwrap();}let previous=self.bridge.param(*id);let detent=matches!(id.spec().kind,ParamKind::Enum{..}|ParamKind::Int{..})&&previous!=v || previous!=v&&((previous<0.&&v>=0.)||(previous>0.&&v<=0.));if detent&&self.haptics&&self.haptic_cooldown<=0.{ctx.haptic_alignment();self.haptic_cooldown=0.065;}self.bridge.set_param(*id,v);self.tip=format!("{} {}",id.spec().short,id.format(v));}
                }
            }d.last=e.pos;self.drag=Some(d);ctx.request_redraw();}}
        Event::MouseUp(_)=>{if let Some(d)=self.drag.clone(){let click=!d.begun&&d.distance<3.;self.finish_drag();ctx.end_infinite_drag();if click{self.activate(d.action,ctx);}}if let Some(before)=self.edit_before.take(){self.history.record(before,crate::history::Snapshot::capture(&*self.bridge));}ctx.request_redraw();}
        Event::MouseMove(e)=>{self.hover=e.pos;ctx.request_redraw();},Event::MouseLeave=>{self.hover=Point::new(-1.,-1.);ctx.request_redraw();},
        Event::Scroll(e)=>{if self.browser.open{let max=(self.filtered_presets().len()as f64*38.-380.).max(0.);self.browser.scroll=(self.browser.scroll-e.delta.y).clamp(0.,max);}else if !e.momentum{if let Some(id)=self.hit(e.pos).filter(|c|matches!(c.action,Action::Param(_)|Action::Macro(_)|Action::Focus(Some(_)))).and_then(|c|self.param_for(&c.action)){if e.delta.y.abs()>0.01 {let current=self.bridge.param(id);let v=match id.spec().kind{ParamKind::Enum{..}|ParamKind::Int{..}=>id.denormalize(id.normalize(current+e.delta.y.signum() as f32)),ParamKind::Bool=>current,_=>id.denormalize((id.normalize(current)+e.delta.y as f32*0.0005*if e.mods.fine(){0.1}else{1.}).clamp(0.,1.))};self.bridge.set_param_once(id,v);}}}ctx.request_redraw();},
        Event::KeyDown(e) if e.mods.cmd&&matches!(e.key,Key::Character('f')|Key::Character('F'))=>{if !self.browser.open{self.open_browser();}self.text_editing=true;ctx.focus_text_input();self.caret=self.browser.search.len();self.anchor=0;ctx.set_handled();ctx.request_redraw();},
        Event::KeyDown(e) if self.browser.open=>{if e.mods.cmd&&self.text_editing {if let Key::Character(ch)=e.key {let name=match ch.to_ascii_lowercase(){'a'=>"selectAll:",'c'=>"copy:",'v'=>"paste:",'x'=>"cut:",_=>""};if !name.is_empty(){self.edit_text(&TextCommand::Other(name.into()),ctx);ctx.set_handled();ctx.request_redraw();return;}}}match e.key{Key::Escape=>{if self.browser.saving{self.browser.saving=false;}else{self.browser.open=false;self.text_editing=false;ctx.blur_text_input();}},Key::Enter=>{if self.browser.saving{self.activate(Action::Save,ctx);}else{self.load_selected(ctx);}},Key::ArrowUp|Key::ArrowDown=>{let p=self.filtered_presets();if !p.is_empty(){let i=p.iter().position(|p|Some(&p.id)==self.browser.selected.as_ref()).unwrap_or(0) as i32;let i=(i+if e.key==Key::ArrowUp{-1}else{1}).clamp(0,p.len()as i32-1)as usize;self.browser.selected=Some(p[i].id.clone());self.browser.scroll=(i as f64*38.-190.).max(0.).min((p.len()as f64*38.-380.).max(0.));}},_=>return}ctx.set_handled();ctx.request_redraw();},
        Event::TextInput(s) if self.browser.open&&self.text_editing=>{self.insert_text(s);ctx.request_redraw();},
        Event::TextPreedit(s) if self.browser.open&&self.text_editing=>{self.preedit=s.clone();ctx.request_redraw();},
        Event::TextCommand(command) if self.browser.open&&self.text_editing=>{self.edit_text(command,ctx);ctx.request_redraw();},
        Event::MenuItemSelected(n)=>{self.menu_selected(*n);ctx.request_redraw();},Event::TextFocusLost=>{self.text_editing=false;self.finish_drag();ctx.end_infinite_drag();ctx.request_redraw();},_=>{}
    }}
    fn frame(&mut self,c:&mut Canvas,dt:f64){self.transition=(self.transition-dt).max(0.);self.haptic_cooldown=(self.haptic_cooldown-dt).max(0.);let target=self.hit(self.hover).map(|c|c.rect);if target.is_some(){self.hover_rect=target;}let goal=if target.is_some(){1.0}else{0.0};let k=1.0-(-dt/0.065).exp();self.hover_opacity+=(goal-self.hover_opacity)*k;if (goal-self.hover_opacity).abs()<0.005{self.hover_opacity=goal;}crate::paint::paint(self,c);}
    fn needs_redraw(&mut self)->bool{let generation=self.bridge.state_generation();let recalled=generation!=self.last_generation;if recalled{self.finish_drag();self.history.clear();self.edit_before=None;self.comparison=None;self.comparison_b=false;self.ui=self.bridge.ui_state();self.browser.selected=self.bridge.current_preset().id;self.last_generation=generation;}let values:Vec<_>=PARAMS.iter().map(|p|self.bridge.param_modulated(p.id)).collect();let mut t=self.bridge.telemetry();t.frame=0;if t.active_voices==0 {t.lfsr=0;t.wave_step=None;t.arp_step=None;t.pitch_offset_st=0.;t.env_level=[0.;4];}let p=self.bridge.current_preset();if p.id!=self.last_preset.id{self.history.clear();self.edit_before=None;self.comparison=None;self.comparison_b=false;}let changed=recalled||values!=self.last_values||t!=self.last_telemetry||p!=self.last_preset;self.last_values=values;self.last_telemetry=t;self.last_preset=p;changed||self.transition>0.||self.haptic_cooldown>0.||(self.hover_opacity-if self.hit(self.hover).is_some(){1.0}else{0.0}).abs()>0.005}
    fn cursor(&self)->Cursor{if let Some(c)=self.hit(self.hover){if matches!(c.action,Action::Macro(_)){Cursor::ResizeHorizontal}else if matches!(c.action,Action::Param(_)){Cursor::ResizeVertical}else{Cursor::Hand}}else if WAVE_PLOT.contains(self.hover)&&self.wave_editable(){Cursor::Crosshair}else{Cursor::Arrow}}
}
impl Drop for EditorApp {fn drop(&mut self){self.finish_drag();}}
impl EditorApp {
    pub(crate) fn field(&self)->&str{if self.browser.saving{&self.browser.save_name}else{&self.browser.search}}
    fn field_mut(&mut self)->&mut String{if self.browser.saving{&mut self.browser.save_name}else{&mut self.browser.search}}
    fn clamp_caret(&mut self){let len=self.field().len();self.caret=self.caret.min(len);self.anchor=self.anchor.min(len);while !self.field().is_char_boundary(self.caret){self.caret-=1;}while !self.field().is_char_boundary(self.anchor){self.anchor-=1;}}
    fn insert_text(&mut self,s:&str){self.clamp_caret();let a=self.caret.min(self.anchor);let b=self.caret.max(self.anchor);let s=s.replace(['\n','\r']," ");if self.browser.saving&&self.field()[..a].chars().count()+s.chars().count()+self.field()[b..].chars().count()>80{return;}self.field_mut().replace_range(a..b,&s);self.caret=a+s.len();self.anchor=self.caret;self.preedit.clear();if !self.browser.saving{self.filter_changed();}}
    fn edit_text(&mut self,command:&TextCommand,ctx:&mut EventCtx){
        self.clamp_caret();let value=self.field().to_owned();let prev=|at:usize|value[..at].char_indices().last().map(|(i,_)|i).unwrap_or(0);let next=|at:usize|value[at..].char_indices().nth(1).map(|(i,_)|at+i).unwrap_or(value.len());
        let word_left=|at:usize|{let mut i=at;while i>0&&value[..i].chars().last().is_some_and(char::is_whitespace){i=prev(i);}while i>0&&value[..i].chars().last().is_some_and(|c|!c.is_whitespace()){i=prev(i);}i};
        let word_right=|at:usize|{let mut i=at;while i<value.len()&&value[i..].chars().next().is_some_and(|c|!c.is_whitespace()){i=next(i);}while i<value.len()&&value[i..].chars().next().is_some_and(char::is_whitespace){i=next(i);}i};
        let movement=match command{TextCommand::MoveLeft{select}=>Some((if !select&&self.anchor!=self.caret{self.anchor.min(self.caret)}else{prev(self.caret)},*select)),TextCommand::MoveRight{select}=>Some((if !select&&self.anchor!=self.caret{self.anchor.max(self.caret)}else{next(self.caret)},*select)),TextCommand::MoveWordLeft{select}=>Some((word_left(self.caret),*select)),TextCommand::MoveWordRight{select}=>Some((word_right(self.caret),*select)),TextCommand::MoveToLineStart{select}|TextCommand::MoveToDocumentStart{select}=>Some((0,*select)),TextCommand::MoveToLineEnd{select}|TextCommand::MoveToDocumentEnd{select}=>Some((value.len(),*select)),_=>None};
        if let Some((caret,select))=movement{self.caret=caret;if !select{self.anchor=caret;}return;}
        match command {
            TextCommand::SelectAll=>{self.anchor=0;self.caret=value.len();},
            TextCommand::DeleteBackward|TextCommand::DeleteForward|TextCommand::DeleteWordBackward|TextCommand::DeleteWordForward|TextCommand::DeleteToLineStart|TextCommand::DeleteToLineEnd=>{if self.caret==self.anchor{self.anchor=match command{TextCommand::DeleteBackward=>prev(self.caret),TextCommand::DeleteForward=>next(self.caret),TextCommand::DeleteWordBackward=>word_left(self.caret),TextCommand::DeleteWordForward=>word_right(self.caret),TextCommand::DeleteToLineStart=>0,_=>value.len()};}self.insert_text("");},
            TextCommand::Cancel=>{if self.browser.saving{self.browser.saving=false;}else{self.browser.open=false;self.text_editing=false;ctx.blur_text_input();}},
            TextCommand::InsertNewline=>{if self.browser.saving{self.activate(Action::Save,ctx);}else{self.load_selected(ctx);}},
            TextCommand::Other(name)=>match name.as_str(){"selectAll:"=>{self.anchor=0;self.caret=value.len();},"copy:"|"cut:"=>{self.clipboard=value[self.anchor.min(self.caret)..self.anchor.max(self.caret)].into();#[cfg(target_os="macos")]let _=pixl_gfx::clipboard::write(&self.clipboard);if name=="cut:"{self.insert_text("");}},"paste:"=>{#[cfg(target_os="macos")]if let Some(s)=pixl_gfx::clipboard::read(){self.clipboard=s;}let s=self.clipboard.clone();self.insert_text(&s);},_=>{}},
            _=>{}
        }
        let x=if self.browser.saving{248.}else{24.};let y=if self.browser.saving{215.}else{76.};ctx.set_text_input_area(Rect::new(x,y,x+1.,y+18.));
    }
}


#[cfg(test)]
mod host_recall_tests {
    use super::*;
    #[test]
    fn same_preset_recall_updates_ui_and_discards_stale_history() {
        let bridge=Arc::new(crate::MockBridge::new());
        let mut editor=EditorApp::new(bridge.clone());
        editor.needs_redraw();
        let before=crate::history::Snapshot::capture(&*bridge);
        bridge.set_param_once(ParamId::OutGain,-9.);
        editor.history.record(before,crate::history::Snapshot::capture(&*bridge));
        assert!(editor.history.can_undo());
        let mut ui=bridge.ui_state();ui.lang=Lang::En;ui.channel=Channel::Noise;
        let id=bridge.current_preset().id;
        bridge.host_recall(ui);
        assert!(editor.needs_redraw());
        assert_eq!(bridge.current_preset().id,id);
        assert_eq!(editor.ui.lang,Lang::En);
        assert_eq!(editor.ui.channel,Channel::Noise);
        assert!(!editor.history.can_undo());
        assert!(editor.comparison.is_none());
    }
}

#[cfg(test)]
mod wave_editor_tests {
    use super::*;
    #[test]
    fn wave_menu_edits_can_be_undone_without_changing_host_automation() {
        let bridge=Arc::new(crate::MockBridge::new());
        bridge.set_param_once(ParamId::Chip,1.);
        let mut editor=EditorApp::new(bridge.clone());editor.ui.channel=Channel::Wave;
        let original=bridge.wave();
        editor.menu_selected(202);
        let mut reversed=original;reversed.reverse();
        assert_eq!(bridge.wave(),reversed);
        bridge.set_param_once(ParamId::OutGain,-12.);
        editor.menu_selected(170);
        assert_eq!(bridge.wave(),original);
        assert_eq!(bridge.param(ParamId::OutGain),-12.);
        editor.menu_selected(171);
        assert_eq!(bridge.wave(),reversed);
    }
    #[test]
    fn nes_triangle_cannot_be_edited_by_wave_tools() {
        let bridge=Arc::new(crate::MockBridge::new());
        bridge.set_param_once(ParamId::Chip,0.);
        let mut editor=EditorApp::new(bridge.clone());editor.ui.channel=Channel::Wave;
        let original=bridge.wave();
        editor.menu_selected(202);
        assert_eq!(bridge.wave(),original);
        assert!(!editor.controls().iter().any(|c|matches!(c.action,Action::WaveEdit(_))));
        assert!(!editor.history.can_undo());
    }
}
