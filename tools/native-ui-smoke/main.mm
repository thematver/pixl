// Real AUv2 host: exercise Cocoa editor creation, Retina resizing and repeated teardown.
// Build: clang++ -std=c++17 -fobjc-arc main.mm -framework Cocoa -framework AudioToolbox -o target/au-ui-smoke
#import <Cocoa/Cocoa.h>
#import <AudioToolbox/AudioToolbox.h>
#import <AudioToolbox/AUCocoaUIView.h>
#include <cstdio>
#include <vector>

static bool hasMetalView(NSView *view) {
    if ([NSStringFromClass([view class]) containsString:@"PixlView"] &&
        [NSStringFromClass([view.layer class]) containsString:@"CAMetalLayer"]) return true;
    for (NSView *child in view.subviews) if (hasMetalView(child)) return true;
    return false;
}
static void pump(double seconds) {
    NSDate *until = [NSDate dateWithTimeIntervalSinceNow:seconds];
    while (until.timeIntervalSinceNow > 0) {
        @autoreleasepool {
            NSEvent *event = [NSApp nextEventMatchingMask:NSEventMaskAny
                untilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]
                inMode:NSDefaultRunLoopMode dequeue:YES];
            if (event) [NSApp sendEvent:event];
            [NSApp updateWindows];
        }
    }
}
int main() {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
        [NSApp finishLaunching];
        AudioComponentDescription desc = {kAudioUnitType_MusicDevice, 'PIXL', 'PXLa', 0, 0};
        AudioComponent component = AudioComponentFindNext(nullptr, &desc);
        if (!component) { std::fprintf(stderr, "PIXL AU not registered\n"); return 1; }
        AudioUnit unit = nullptr;
        OSStatus status = AudioComponentInstanceNew(component, &unit);
        if (status) { std::fprintf(stderr, "AU open: %d\n", status); return 1; }
        UInt32 size = 0;
        status = AudioUnitGetPropertyInfo(unit, kAudioUnitProperty_CocoaUI, kAudioUnitScope_Global, 0, &size, nullptr);
        if (status || size < sizeof(AudioUnitCocoaViewInfo)) {
            std::fprintf(stderr, "AU has no custom editor: %d\n", status);
            AudioComponentInstanceDispose(unit); return 1;
        }
        std::vector<unsigned char> bytes(size);
        auto *info = reinterpret_cast<AudioUnitCocoaViewInfo *>(bytes.data());
        status = AudioUnitGetProperty(unit, kAudioUnitProperty_CocoaUI, kAudioUnitScope_Global, 0, info, &size);
        if (status) { AudioComponentInstanceDispose(unit); return 1; }
        NSBundle *bundle = [NSBundle bundleWithURL:(__bridge NSURL *)info->mCocoaAUViewBundleLocation];
        [bundle load];
        Class cls = [bundle classNamed:(__bridge NSString *)info->mCocoaAUViewClass[0]];
        id<AUCocoaUIBase> factory = [[cls alloc] init];
        for (UInt32 i = 0; i < (size - offsetof(AudioUnitCocoaViewInfo, mCocoaAUViewClass)) / sizeof(CFStringRef); ++i)
            if (info->mCocoaAUViewClass[i]) CFRelease(info->mCocoaAUViewClass[i]);
        if (info->mCocoaAUViewBundleLocation) CFRelease(info->mCocoaAUViewBundleLocation);
        if (!factory) { std::fprintf(stderr, "AU Cocoa factory missing\n"); AudioComponentInstanceDispose(unit); return 1; }
        bool valid = true;
        for (int cycle = 0; cycle < 3; ++cycle) {
            @autoreleasepool {
                NSView *view = [factory uiViewForAudioUnit:unit withSize:NSMakeSize(900,560)];
                if (!view) { valid = false; break; }
                NSWindow *window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0,0,900,560)
                    styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskResizable
                    backing:NSBackingStoreBuffered defer:NO];
                window.releasedWhenClosed = NO;
                window.title = @"PIXL AU editor verification";
                window.contentView = view;
                [window center]; [window makeKeyAndOrderFront:nil];
                pump(0.8);
                valid &= hasMetalView(view);
                [window setContentSize:NSMakeSize(1125,700)]; pump(0.2);
                [window setContentSize:NSMakeSize(675,420)]; pump(0.2);
                [window close];
                window.contentView = [[NSView alloc] initWithFrame:NSZeroRect];
            }
            pump(0.1);
        }
        AudioComponentInstanceDispose(unit);
        pump(0.1);
        std::fprintf(stderr, "AU native editor: 3 open/resize/close cycles, Metal child %s\n", valid ? "PASS" : "FAIL");
        return valid ? 0 : 1;
    }
}
