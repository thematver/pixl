// Standalone AUv2 boundary checks. No audio device or Logic process is used.
#include <AudioToolbox/AudioToolbox.h>
#include <CoreFoundation/CoreFoundation.h>
#include <algorithm>
#include <cmath>
#include <cstdio>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <map>
#include <stdexcept>
#include <string>
#include <vector>

static void require(bool ok, const std::string &why) {
    if (!ok) throw std::runtime_error(why);
}
static void check(OSStatus s, const char *op) {
    if (s) throw std::runtime_error(std::string(op) + " OSStatus=" + std::to_string(s));
}
static std::string utf8(CFStringRef s) {
    if (!s) return {};
    std::vector<char> b(CFStringGetMaximumSizeForEncoding(CFStringGetLength(s), kCFStringEncodingUTF8)+1);
    require(CFStringGetCString(s,b.data(),b.size(),kCFStringEncodingUTF8),"UTF-8 conversion");
    return b.data();
}
struct Param { AudioUnitParameterID id; std::string name; float lo, hi, def; UInt32 flags; };
struct State {
    CFPropertyListRef value = nullptr;
    ~State() { if(value) CFRelease(value); }
};
struct Host {
    AudioUnit au = nullptr;
    bool initialized = false;
    double sample = 0;
    std::vector<Param> params;
    Host(AudioComponent component) {
        check(AudioComponentInstanceNew(component,&au),"instantiate");
    }
    ~Host() { if(au) { if(initialized) AudioUnitUninitialize(au); AudioComponentInstanceDispose(au); } }
    Host(const Host &) = delete;
    void enumerate() {
        UInt32 bytes=0; Boolean writable=false;
        check(AudioUnitGetPropertyInfo(au,kAudioUnitProperty_ParameterList,kAudioUnitScope_Global,0,&bytes,&writable),"parameter list size");
        require(bytes%sizeof(AudioUnitParameterID)==0,"invalid parameter list size");
        std::vector<AudioUnitParameterID> ids(bytes/sizeof(AudioUnitParameterID));
        check(AudioUnitGetProperty(au,kAudioUnitProperty_ParameterList,kAudioUnitScope_Global,0,ids.data(),&bytes),"parameter list");
        std::sort(ids.begin(),ids.end());
        require(std::adjacent_find(ids.begin(),ids.end())==ids.end(),"duplicate parameter IDs");
        for(auto id:ids) {
            AudioUnitParameterInfo i{}; bytes=sizeof(i);
            check(AudioUnitGetProperty(au,kAudioUnitProperty_ParameterInfo,kAudioUnitScope_Global,id,&i,&bytes),"parameter info");
            std::string name=i.cfNameString?utf8(i.cfNameString):std::string(i.name,strnlen(i.name,sizeof(i.name)));
            if(i.cfNameString && (i.flags & kAudioUnitParameterFlag_CFNameRelease)) CFRelease(i.cfNameString);
            require(!name.empty() && std::isfinite(i.minValue) && std::isfinite(i.maxValue) && i.maxValue>=i.minValue,"invalid parameter metadata");
            params.push_back({id,name,i.minValue,i.maxValue,i.defaultValue,i.flags});
        }
        require(params.size()==91,"expected 91 PIXL parameters; got "+std::to_string(params.size()));
    }
    const Param &param(const std::string &name) const {
        const Param *found=nullptr;
        for(const auto &p:params) if(p.name==name) { require(!found,"ambiguous parameter "+name); found=&p; }
        require(found,"missing parameter "+name); return *found;
    }
    float get(const Param &p) {
        float v=0; check(AudioUnitGetParameter(au,p.id,kAudioUnitScope_Global,0,&v),"get parameter");
        require(std::isfinite(v),"nonfinite parameter "+p.name); return v;
    }
    void set(const Param &p,float v) { check(AudioUnitSetParameter(au,p.id,kAudioUnitScope_Global,0,v,0),"set parameter"); }
    // CLAP/this AU exposes continuous controls normalized, stepped controls in step units.
    void fraction(const char *name,float f) { const auto &p=param(name); set(p,p.lo+f*(p.hi-p.lo)); }
    void prepare() {
        UInt32 maxFrames=256;
        check(AudioUnitSetProperty(au,kAudioUnitProperty_MaximumFramesPerSlice,kAudioUnitScope_Global,0,&maxFrames,sizeof(maxFrames)),"max frames");
        AudioStreamBasicDescription fmt{};
        fmt.mSampleRate=48000; fmt.mFormatID=kAudioFormatLinearPCM;
        fmt.mFormatFlags=kAudioFormatFlagsNativeFloatPacked|kAudioFormatFlagIsNonInterleaved;
        fmt.mBytesPerPacket=fmt.mBytesPerFrame=sizeof(float); fmt.mFramesPerPacket=1;
        fmt.mChannelsPerFrame=2; fmt.mBitsPerChannel=32;
        check(AudioUnitSetProperty(au,kAudioUnitProperty_StreamFormat,kAudioUnitScope_Output,0,&fmt,sizeof(fmt)),"stereo float output");
        check(AudioUnitInitialize(au),"initialize"); initialized=true;
    }
    void midi(UInt32 status,UInt32 a,UInt32 b,UInt32 offset=0) {
        check(MusicDeviceMIDIEvent(au,status,a,b,offset),"MIDI event");
    }
    std::vector<float> render(unsigned blocks) {
        std::vector<float> out; out.reserve(blocks*256);
        float left[256],right[256];
        struct Stereo { UInt32 count; AudioBuffer buffers[2]; } list{};
        list.count=2;
        for(unsigned n=0;n<blocks;++n) {
            std::fill_n(left,256,0); std::fill_n(right,256,0);
            list.buffers[0]={1,sizeof(left),left}; list.buffers[1]={1,sizeof(right),right};
            AudioTimeStamp t{}; t.mSampleTime=sample; t.mFlags=kAudioTimeStampSampleTimeValid;
            AudioUnitRenderActionFlags flags=0;
            check(AudioUnitRender(au,&flags,&t,0,256,reinterpret_cast<AudioBufferList *>(&list)),"render");
            sample+=256;
            for(unsigned j=0;j<256;++j) {
                require(std::isfinite(left[j]) && std::isfinite(right[j]),"nonfinite audio");
                out.push_back((left[j]+right[j])*0.5f);
            }
        }
        return out;
    }
    void save(State &s) {
        UInt32 bytes=sizeof(s.value);
        check(AudioUnitGetProperty(au,kAudioUnitProperty_ClassInfo,kAudioUnitScope_Global,0,&s.value,&bytes),"save class info");
        require(s.value && CFGetTypeID(s.value)==CFDictionaryGetTypeID(),"class info is not a dictionary");
    }
    void restore(const State &s) {
        check(AudioUnitSetProperty(au,kAudioUnitProperty_ClassInfo,kAudioUnitScope_Global,0,&s.value,sizeof(s.value)),"restore class info");
    }
};
static double rms(const std::vector<float> &v) {
    double sum=0; for(auto x:v) sum+=x*x; return std::sqrt(sum/v.size());
}
static double frequency(const std::vector<float> &v) {
    // Positive zero crossings after a settling block: sufficient for a clean A4 pulse.
    std::vector<size_t> crossings;
    for(size_t i=257;i<v.size();++i) if(v[i-1]<=0 && v[i]>0) crossings.push_back(i);
    require(crossings.size()>20,"too few pitch crossings");
    return 48000.0*(crossings.size()-1)/(crossings.back()-crossings.front());
}
static void closeValue(float a,float b,const Param &p) {
    require(std::abs(a-b)<=1e-5f*std::max(1.0f,p.hi-p.lo),"value mismatch: "+p.name+" expected="+std::to_string(a)+" actual="+std::to_string(b));
}
static void nativePresets(Host &host) {
    CFArrayRef presets=nullptr; UInt32 bytes=sizeof(presets);
    check(AudioUnitGetProperty(host.au,kAudioUnitProperty_FactoryPresets,kAudioUnitScope_Global,0,&presets,&bytes),"factory presets");
    require(presets && CFArrayGetCount(presets)>0,"empty native factory library");
    std::vector<SInt32> numbers;
    for(CFIndex i=0;i<CFArrayGetCount(presets);++i) {
        auto preset=static_cast<const AUPreset *>(CFArrayGetValueAtIndex(presets,i));
        require(preset && preset->presetNumber>=0 && !utf8(preset->presetName).empty(),"invalid factory entry");
        numbers.push_back(preset->presetNumber);
        check(AudioUnitSetProperty(host.au,kAudioUnitProperty_PresentPreset,kAudioUnitScope_Global,0,preset,sizeof(*preset)),"select factory preset");
        host.render(1);
        AUPreset current{}; bytes=sizeof(current);
        check(AudioUnitGetProperty(host.au,kAudioUnitProperty_PresentPreset,kAudioUnitScope_Global,0,&current,&bytes),"current preset");
        require(current.presetNumber==preset->presetNumber && utf8(current.presetName)==utf8(preset->presetName),"native current preset mismatch");
        CFRelease(current.presetName);
        for(const auto &p:host.params) (void)host.get(p);
        State saved; host.save(saved);
        require(CFEqual(CFDictionaryGetValue(static_cast<CFDictionaryRef>(saved.value),CFSTR("name")),preset->presetName),"class-info factory name mismatch");
    }
    std::sort(numbers.begin(),numbers.end());
    require(std::adjacent_find(numbers.begin(),numbers.end())==numbers.end(),"duplicate factory numbers");
    CFRelease(presets);
    AUPreset invalid{999999,CFSTR("invalid")};
    require(AudioUnitSetProperty(host.au,kAudioUnitProperty_PresentPreset,kAudioUnitScope_Global,0,&invalid,sizeof(invalid))!=noErr,"invalid factory accepted");
    AUPreset custom{-1,CFSTR("Native Logic Setting")};
    check(AudioUnitSetProperty(host.au,kAudioUnitProperty_PresentPreset,kAudioUnitScope_Global,0,&custom,sizeof(custom)),"custom preset name");
    State saved; host.save(saved);
    auto dict=static_cast<CFDictionaryRef>(saved.value);
    require(CFEqual(CFDictionaryGetValue(dict,CFSTR("name")),custom.presetName),"custom save name mismatch");
    // Invalid plist types, foreign component IDs and corrupted chunks must fail cleanly.
    CFPropertyListRef bad=CFSTR("not a dictionary");
    require(AudioUnitSetProperty(host.au,kAudioUnitProperty_ClassInfo,kAudioUnitScope_Global,0,&bad,sizeof(bad))!=noErr,"string class info accepted");
    auto damaged=CFDictionaryCreateMutableCopy(nullptr,0,dict);
    SInt32 foreign='BAD!'; auto number=CFNumberCreate(nullptr,kCFNumberSInt32Type,&foreign);
    CFDictionarySetValue(damaged,CFSTR("subtype"),number); CFRelease(number);
    bad=damaged;
    require(AudioUnitSetProperty(host.au,kAudioUnitProperty_ClassInfo,kAudioUnitScope_Global,0,&bad,sizeof(bad))!=noErr,"foreign preset accepted");
    CFRelease(damaged);
    damaged=CFDictionaryCreateMutableCopy(nullptr,0,dict);
    const UInt8 junk[]={1,2,3}; auto data=CFDataCreate(nullptr,junk,sizeof(junk));
    CFDictionarySetValue(damaged,CFSTR("data"),data); CFRelease(data); bad=damaged;
    require(AudioUnitSetProperty(host.au,kAudioUnitProperty_ClassInfo,kAudioUnitScope_Global,0,&bad,sizeof(bad))!=noErr,"corrupt state accepted");
    CFRelease(damaged);
    host.restore(saved);
    std::cout<<"PASS native factory selection: "<<numbers.size()<<" presets, current names, custom name and invalid-state rejection\n";
}

int main(int argc,char **argv) {
    try {
        require(argc<=2,"usage: au_host [artifact-directory] (directory must exist)");
        AudioComponentDescription desc{}; desc.componentType='aumu'; desc.componentSubType='PIXL'; desc.componentManufacturer='PXLa';
        auto component=AudioComponentFindNext(nullptr,&desc);
        require(component,"registered aumu/PIXL/PXLa not found; install/register the AU separately");
        UInt32 version=0; check(AudioComponentGetVersion(component,&version),"component version");
        std::cout<<"PIXL AU component version="<<version<<"\n";
        Host source(component), restored(component);
        source.enumerate(); restored.enumerate();
        for(size_t i=0;i<source.params.size();++i) {
            const auto &p=source.params[i], &q=restored.params[i];
            require(p.id==q.id && p.name==q.name && p.lo==q.lo && p.hi==q.hi && p.flags==q.flags,"parameter identity differs across instances");
            std::cout<<p.id<<"\t"<<p.name<<"\t"<<p.lo<<".."<<p.hi<<"\n";
        }
        source.prepare(); restored.prepare();
        nativePresets(source);
        const auto &output=source.param("Output");
        require((output.flags & kAudioUnitParameterFlag_IsWritable)!=0,"Output is not writable");
        for(float f:{0.25f,0.75f}) {
            float value=output.lo+f*(output.hi-output.lo);
            source.set(output,value); source.render(1); closeValue(value,source.get(output),output);
        }
        AudioUnitParameterEvent event{};
        event.scope=kAudioUnitScope_Global; event.element=0; event.parameter=output.id;
        event.eventType=kParameterEvent_Immediate;
        event.eventValues.immediate.bufferOffset=64;
        event.eventValues.immediate.value=output.lo+0.5f*(output.hi-output.lo);
        check(AudioUnitScheduleParameters(source.au,&event,1),"schedule automation at frame 64");
        source.render(1); closeValue(event.eventValues.immediate.value,source.get(output),output);
        std::cout<<"PASS automation set/get and scheduled immediate event\n";
        // Clean single pulse, FREE mode, no modulation/effects, constant sustain.
        source.fraction("Chip",1); source.fraction("Pulse 1 On",1);
        for(const char *name:{"Pulse 2 On","Wave On","Noise On","Sweep On","Vibrato On","Arp On","Glide On","Unison On","Echo On","Analog Output"}) source.fraction(name,0);
        source.fraction("Pulse 1 Attack",0); source.fraction("Pulse 1 Release",0);
        source.fraction("Pulse 1 Sustain",1); source.fraction("Pulse 1 Duty",0.5f);
        source.fraction("Pitch Bend Range",0.5f); // 12 semitones (24 steps).
        source.render(2);
        source.midi(0x90,69,100,32);
        auto center=source.render(100);
        require(rms(center)>1e-4,"note-on is silent");
        double f0=frequency(center);
        source.midi(0xe0,127,127);
        auto bent=source.render(100); double f1=frequency(bent);
        require(std::abs(f0-440)<5 && std::abs(f1/f0-2)<0.03,"pitchbend does not produce expected octave");
        source.midi(0xe0,0,64); auto recentered=source.render(100);
        require(std::abs(frequency(recentered)/f0-1)<0.02,"pitchbend center not restored");
        source.midi(0xb0,64,127); source.midi(0x80,69,0,96);
        auto held=source.render(80); require(rms(held)>1e-4,"sustain did not hold released note");
        source.midi(0xb0,64,0); source.render(100);
        auto tail=source.render(40); require(rms(tail)<1e-5,"sustain-off did not silence note");
        std::cout<<"PASS MIDI note/pitchbend/sustain: Hz="<<f0<<" -> "<<f1<<" held RMS="<<rms(held)<<" tail RMS="<<rms(tail)<<"\n";
        State saved; source.save(saved);
        CFErrorRef error=nullptr;
        CFDataRef data=CFPropertyListCreateData(kCFAllocatorDefault,saved.value,kCFPropertyListBinaryFormat_v1_0,0,&error);
        if(error) CFRelease(error);
        require(data,"serialize class info");
        // Restore from serialized bytes, rather than sharing a mutable dictionary.
        State decoded;
        decoded.value=CFPropertyListCreateWithData(kCFAllocatorDefault,data,kCFPropertyListImmutable,nullptr,nullptr);
        if(argc==2) {
            std::ofstream file(std::string(argv[1])+"/state.aupreset",std::ios::binary);
            file.write(reinterpret_cast<const char *>(CFDataGetBytePtr(data)),CFDataGetLength(data));
            require(file.good(),"write state.aupreset");
        }
        CFRelease(data); require(decoded.value,"decode class info");
        std::vector<float> expected;
        for(const auto &p:source.params) expected.push_back(source.get(p));
        restored.fraction("Chip",0); restored.fraction("Output",0.9f); restored.render(1);
        restored.restore(decoded); restored.render(2);
        for(size_t i=0;i<source.params.size();++i) closeValue(expected[i],restored.get(restored.params[i]),source.params[i]);
        restored.midi(0x90,69,100); auto recall=restored.render(100);
        require(std::abs(frequency(recall)/f0-1)<0.02,"restored patch pitch differs");
        restored.midi(0xb0,120,0); restored.render(2);
        std::cout<<"PASS serialized class-info restore: all 91 values and audible patch pitch\n";
        if(argc==2) {
            std::ofstream file(std::string(argv[1])+"/parameters.tsv");
            file<<"id\tname\tmin\tmax\tdefault\tvalue\n"<<std::setprecision(9);
            for(size_t i=0;i<source.params.size();++i) {const auto &p=source.params[i]; file<<p.id<<'\t'<<p.name<<'\t'<<p.lo<<'\t'<<p.hi<<'\t'<<p.def<<'\t'<<expected[i]<<'\n';}
            require(file.good(),"write parameters.tsv");
        }
        std::cout<<"PASS standalone AU integration (Logic UI/project behavior still requires Logic)\n";
        return 0;
    } catch(const std::exception &e) { std::cerr<<"FAIL: "<<e.what()<<'\n'; return 1; }
}
