// Audio routing arbitration, the one app-level audio call WebKit makes that cubeb does not.
//
// WebKit's AudioSessionMac::setCategory does two things: it calls AudioSessionCocoa::setCategory,
// which reaches AVAudioSession (see src/audio_session.m), and then, under
// ENABLE(ROUTING_ARBITRATION), calls beginRoutingArbitrationWithCategory. This file is the second
// half. The mode is only logged here -- beginRoutingArbitrationWithCategory takes a category and
// nothing else -- so arbitration is category-only by construction.
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
