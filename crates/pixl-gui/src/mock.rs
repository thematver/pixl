use pixl_core::{*, preset::PresetState};
use std::sync::Mutex;

/// Recorded host automation, in delivery order.
#[derive(Clone, Debug, PartialEq)]
pub enum EditEvent { Begin(ParamId), Set(ParamId, f32), End(ParamId), Wave(WaveRam), Resize(f32) }
struct State {
    sound: PresetState, ui: UiState, presets: Vec<(PresetInfo, PresetState)>,
    current: CurrentPreset, events: Vec<EditEvent>, telemetry: TelemetrySnapshot,
}
/// Thread-safe in-memory Bridge, using the real factory library and preset states.
pub struct MockBridge { state: Mutex<State> }
impl Default for MockBridge { fn default() -> Self { Self::new() } }
impl MockBridge {
    pub fn new() -> Self {
        let lib = PresetLibrary::from_json(include_str!("../../../presets/factory.json")).expect("factory library");
        let presets: Vec<_> = lib.presets.iter().map(|p| (PresetInfo {
            id: format!("factory:{:03}", p.number), name:p.name.clone(), category:p.category,
            number:p.number, chip:p.chip(), icon:p.icon, tags:p.tags.clone(), description:p.description.clone(),
            author:p.author.clone(), is_factory:true, favorite:false,
        }, p.to_state().0)).collect();
        let initial = presets.iter().find(|(p,_)| p.number == 14).unwrap_or(&presets[0]);
        let sound = initial.1.clone();
        let current = CurrentPreset { id:Some(initial.0.id.clone()), name:initial.0.name.clone(), category:initial.0.category, number:initial.0.number, modified:false };
        Self { state:Mutex::new(State { sound, ui:UiState::default(), presets, current, events:Vec::new(), telemetry:TelemetrySnapshot::default() }) }
    }
    pub fn events(&self) -> Vec<EditEvent> { self.state.lock().unwrap().events.clone() }
    pub fn clear_events(&self) { self.state.lock().unwrap().events.clear(); }
    pub fn set_telemetry(&self, telemetry: TelemetrySnapshot) { self.state.lock().unwrap().telemetry = telemetry; }
    /// Explicit clock for reproducible screenshots, without a hidden wall-clock dependency.
    pub fn animate(&self, seconds:f64) {
        let mut s=self.state.lock().unwrap(); let t=&mut s.telemetry;
        t.active_voices=3; t.last_note=Some(64); t.held_notes=[1u64<<60,1];
        t.rms_db=[-18.0+(seconds.sin()*4.0) as f32,-21.0]; t.peak_db=[-6.2,-9.0]; t.peak_hold_db=-6.2;
        t.wave_step=Some(((seconds*16.0) as u8)%32); t.arp_step=Some(((seconds*8.0) as u8)%8);
        t.lfsr=((seconds*12345.0) as u16 & 0x7fff).max(1); t.env_level=[0.8;4]; t.frame=(seconds*60.0) as u64;
    }
}
impl Bridge for MockBridge {
    fn param(&self,id:ParamId)->f32 { self.state.lock().unwrap().sound.values.get(id) }
    fn param_modulated(&self,id:ParamId)->f32 { let s=self.state.lock().unwrap(); let mut out=s.sound.values; let m=std::array::from_fn(|i| s.sound.values.get(ParamId::macro_value(i))/100.0); apply_macros(&s.sound.values,m,&s.sound.macros,&mut out); out.get(id) }
    fn begin_edit(&self,id:ParamId) { self.state.lock().unwrap().events.push(EditEvent::Begin(id)); }
    fn set_param(&self,id:ParamId,v:f32) { let mut s=self.state.lock().unwrap(); let changed=s.sound.values.get(id)!=id.sanitize(v); s.sound.values.set(id,v); s.current.modified|=changed; s.events.push(EditEvent::Set(id,id.sanitize(v))); }
    fn end_edit(&self,id:ParamId) { self.state.lock().unwrap().events.push(EditEvent::End(id)); }
    fn wave(&self)->WaveRam { self.state.lock().unwrap().sound.wave }
    fn set_wave(&self,w:WaveRam) { let mut s=self.state.lock().unwrap(); let w=wave::sanitize(&w); s.sound.wave=w; s.current.modified=true; s.events.push(EditEvent::Wave(w)); }
    fn macro_assignments(&self)->MacroAssignments { self.state.lock().unwrap().sound.macros }
    fn set_macro_assignment(&self,m:usize,id:ParamId,d:f32)->bool { let mut s=self.state.lock().unwrap(); let ok=s.sound.macros.set(m,id,d); if ok { s.current.modified=true; } ok }
    fn preset_list(&self)->Vec<PresetInfo> { self.state.lock().unwrap().presets.iter().map(|(p,_)|p.clone()).collect() }
    fn current_preset(&self)->CurrentPreset { self.state.lock().unwrap().current.clone() }
    fn load_preset(&self,id:&str)->Result<(),String> { let mut s=self.state.lock().unwrap(); let (p,sound)=s.presets.iter().find(|(p,_)|p.id==id).cloned().ok_or("Preset not found")?; s.sound=sound; s.current=CurrentPreset{id:Some(p.id),name:p.name,category:p.category,number:p.number,modified:false}; Ok(()) }
    fn save_user_preset(&self,name:&str,category:Category)->Result<PresetId,String> {
        let name=name.trim(); if name.is_empty() || name.chars().count()>80 {return Err("Enter a name (1–80 characters)".into());}
        let mut s=self.state.lock().unwrap();
        let existing=s.presets.iter().find(|(p,_)|!p.is_factory && p.name.eq_ignore_ascii_case(name)).map(|(p,_)|p.id.clone());
        let id=existing.unwrap_or_else(|| format!("user:{}",s.presets.len()));
        let p=Preset::from_state(name,category,&s.sound);
        let info=PresetInfo{id:id.clone(),name:name.into(),category,number:0,chip:p.chip(),icon:p.icon,tags:p.tags,description:p.description,author:"User".into(),is_factory:false,favorite:false};
        let sound=s.sound.clone();
        if let Some(slot)=s.presets.iter_mut().find(|(p,_)|p.id==id) { let fav=slot.0.favorite; *slot=(info,sound); slot.0.favorite=fav; } else {s.presets.push((info,sound));}
        s.current=CurrentPreset{id:Some(id.clone()),name:name.into(),category,number:0,modified:false}; Ok(id)
    }
    fn delete_user_preset(&self,id:&str)->Result<(),String> { let mut s=self.state.lock().unwrap(); let i=s.presets.iter().position(|(p,_)|p.id==id).ok_or("Preset not found")?; if s.presets[i].0.is_factory {return Err("Factory presets are read-only".into());} s.presets.remove(i); if s.current.id.as_deref()==Some(id) {s.current.id=None;s.current.modified=true;} Ok(()) }
    fn set_favorite(&self,id:&str,favorite:bool) {if let Some((p,_))=self.state.lock().unwrap().presets.iter_mut().find(|(p,_)|p.id==id){p.favorite=favorite;} }
    fn telemetry(&self)->TelemetrySnapshot {self.state.lock().unwrap().telemetry}
    fn ui_state(&self)->UiState {self.state.lock().unwrap().ui.clone()}
    fn set_ui_state(&self,ui:UiState) {self.state.lock().unwrap().ui=ui;}
    fn request_resize(&self,scale:f32)->bool {let mut s=self.state.lock().unwrap(); if !(0.75..=2.0).contains(&scale){return false;} s.ui.scale=scale;s.events.push(EditEvent::Resize(scale));true}
}
