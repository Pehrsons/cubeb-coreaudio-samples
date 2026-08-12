// AVAudioSession on macOS: the category and mode WebKit sets, which cubeb never touches.
//
// WebKit logs
//
//     MediaSessionManagerCocoa::updateSessionState(0) setting category = PlayAndRecord,
//         mode = VideoChat, policy = Default
//
// and the implementation behind it is AudioSessionCocoa::setCategory, which AudioSessionMac calls
// first before doing its own routing arbitration:
//
//     void AudioSessionCocoa::setCategory(CategoryType newCategory, Mode, RouteSharingPolicy)
//     {
//         setEligibleForSmartRouting(isActive() && newCategory != AudioSessionCategory::None);
//     }
//
// The interesting part is where that leads: setEligibleForSmartRoutingInternal calls
// -setEligibleForBTSmartRoutingConsideration:error: on [AVAudioSession sharedInstance], and
// tryToSetActiveInternal calls -setActive:withOptions:error: on it. HAVE_AVAUDIOSESSION_SMARTROUTING
// is defined for PLATFORM(MAC), so this is live on macOS.
//
// The public SDK marks AVAudioSession API_UNAVAILABLE(macos) and does not even declare the class, so
// it looks unreachable -- but WebKit gets at it through pal/spi/cocoa/AVFoundationSPI.h and
// AVFoundationSoftLink.h, and the class is fully functional at runtime: on macOS 26.6 the category
// and mode both stick and setActive:YES succeeds. Hence the local declaration below, and the
// constants by dlsym rather than by symbol reference.
//
// Two caveats on interpreting a measurement made with this. The session is per process and its
// category starts at SoloAmbient, so leaving it set changes the process for every later step. And
// WebKit sets the category from MediaSessionManagerCocoa, whose calls are gated on
// AudioSession::shouldManageAudioSessionCategory(), so whether shipping Safari has it set for a
// getUserMedia capture is not established here.

#import <Foundation/Foundation.h>
#import <dlfcn.h>

@interface AVAudioSession : NSObject
+ (instancetype)sharedInstance;
- (NSString *)category;
- (NSString *)mode;
- (BOOL)setCategory:(NSString *)category
               mode:(NSString *)mode
            options:(unsigned long)options
              error:(NSError **)error;
- (BOOL)setActive:(BOOL)active withOptions:(unsigned long)options error:(NSError **)error;
- (BOOL)setEligibleForBTSmartRoutingConsideration:(BOOL)eligible error:(NSError **)error;
@end

// The category and mode constants are not exported to the macOS SDK either. Their values are their
// own names, but read the real symbol when it is there rather than relying on that.
static NSString *audio_session_constant(const char *name)
{
    NSString *__unsafe_unretained *symbol = (NSString *__unsafe_unretained *)dlsym(RTLD_DEFAULT, name);
    return symbol ? *symbol : [NSString stringWithUTF8String:name];
}

static AVAudioSession *audio_session_shared(void)
{
    Class class = NSClassFromString(@"AVAudioSession");
    return class ? [class sharedInstance] : nil;
}

// Reports the session's current category and mode, with the "AVAudioSessionCategory"/"Mode" prefixes
// stripped, so a caller can confirm what actually took effect.
void audio_session_describe(char *category_out, size_t category_len, char *mode_out, size_t mode_len)
{
    @autoreleasepool {
        AVAudioSession *session = audio_session_shared();
        NSString *category = session ? [session category] : @"(unavailable)";
        NSString *mode = session ? [session mode] : @"(unavailable)";
        for (NSString *prefix in @[ @"AVAudioSessionCategory", @"AVAudioSessionMode" ]) {
            if ([category hasPrefix:prefix]) {
                category = [category substringFromIndex:prefix.length];
            }
            if ([mode hasPrefix:prefix]) {
                mode = [mode substringFromIndex:prefix.length];
            }
        }
        strlcpy(category_out, category.UTF8String, category_len);
        strlcpy(mode_out, mode.UTF8String, mode_len);
    }
}

// Sets the play-and-record category with `mode` ("VideoChat", "VoiceChat", "Default", or any other
// AVAudioSessionMode suffix). With `activate`, also activates the session and declares it eligible
// for Bluetooth smart routing, which is the rest of what WebKit does. Returns 0 on success, -1 if
// AVAudioSession is unavailable, or the NSError code of whichever call failed first.
int audio_session_configure(const char *mode, int activate)
{
    @autoreleasepool {
        AVAudioSession *session = audio_session_shared();
        if (!session) {
            return -1;
        }

        NSString *category = audio_session_constant("AVAudioSessionCategoryPlayAndRecord");
        NSString *mode_constant = audio_session_constant(
            [[NSString stringWithFormat:@"AVAudioSessionMode%s", mode] UTF8String]);

        NSError *error = nil;
        if (![session setCategory:category mode:mode_constant options:0 error:&error]) {
            return error ? (int)error.code : -1;
        }
        if (!activate) {
            return 0;
        }

        error = nil;
        if (![session setActive:YES withOptions:0 error:&error]) {
            return error ? (int)error.code : -1;
        }
        error = nil;
        if (![session setEligibleForBTSmartRoutingConsideration:YES error:&error]) {
            return error ? (int)error.code : -1;
        }
        return 0;
    }
}

// Deactivates the session, as AudioSessionCocoa::tryToSetActiveInternal(false) does. The category
// stays where it was set: AVAudioSession has no way to put it back to the initial SoloAmbient.
int audio_session_deactivate(void)
{
    @autoreleasepool {
        AVAudioSession *session = audio_session_shared();
        if (!session) {
            return -1;
        }
        NSError *error = nil;
        if (![session setActive:NO withOptions:0 error:&error]) {
            return error ? (int)error.code : -1;
        }
        [session setEligibleForBTSmartRoutingConsideration:NO error:NULL];
        return 0;
    }
}
