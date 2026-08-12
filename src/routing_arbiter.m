// Audio routing arbitration, the one app-level audio call WebKit makes that cubeb does not.
//
// WebKit's AudioSessionMac::setCategory does two things: it records a category and mode through
// AudioSessionCocoa, and then, under ENABLE(ROUTING_ARBITRATION), calls
// beginRoutingArbitrationWithCategory. Only the first is visible in the log as
//
//     MediaSessionManagerCocoa::updateSessionState(0) setting category = PlayAndRecord,
//         mode = VideoChat, policy = Default
//
// but the mode is discarded on macOS -- the macOS path reaches UNUSED_PARAM(mode) -- and
// AVAudioSession itself is API_UNAVAILABLE(macos). So arbitration is the part that actually reaches
// the system, and this is it.
//
// Its documented purpose is route coordination: WebKit re-arbitrates when the default device's
// Bluetooth-ness changes and only inspects whether the default route changed. Any effect on input
// processing or on which mic modes the system offers would be incidental, which is what this is for
// measuring.

#import <AVFAudio/AVFAudio.h>
#import <Foundation/Foundation.h>
#import <dispatch/dispatch.h>

// Begins arbitration for the "play and record" category and waits for the completion handler, so a
// caller that measures immediately afterwards is not racing it. Returns 0 on success, a negative
// value if the API is unavailable or times out, or the NSError code arbitration failed with.
int routing_arbiter_begin_play_and_record(void)
{
    @autoreleasepool {
        if (![AVAudioRoutingArbiter class]) {
            return -1;
        }

        __block int result = -2;
        dispatch_semaphore_t done = dispatch_semaphore_create(0);
        [[AVAudioRoutingArbiter sharedRoutingArbiter]
            beginArbitrationWithCategory:AVAudioRoutingArbitrationCategoryPlayAndRecord
                       completionHandler:^(BOOL defaultDeviceChanged, NSError *_Nullable error) {
                           (void)defaultDeviceChanged;
                           result = error ? (int)error.code : 0;
                           dispatch_semaphore_signal(done);
                       }];

        if (dispatch_semaphore_wait(done,
                                    dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC)) != 0) {
            return -3;
        }
        return result;
    }
}

void routing_arbiter_leave(void)
{
    @autoreleasepool {
        if ([AVAudioRoutingArbiter class]) {
            [[AVAudioRoutingArbiter sharedRoutingArbiter] leaveArbitration];
        }
    }
}
