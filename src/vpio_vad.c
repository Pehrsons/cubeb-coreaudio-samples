// Voice activity detection setup for a VoiceProcessingIO unit, matching what WebKit does.
//
// This lives in C because kAUVoiceIOProperty_MutedSpeechActivityEventListener takes an Objective-C
// block, which bindgen flattens to a void pointer. Hand-rolling a block layout in Rust is easy to
// get subtly wrong, and the point of this file is to be a faithful copy of the reference behaviour:
// enable the device's voice activity detection, then install a listener on the unit, as
// CoreAudioCaptureUnit::setVoiceActivityDetection does.

#include <AudioToolbox/AudioToolbox.h>
#include <CoreAudio/CoreAudio.h>

OSStatus vpio_enable_voice_activity_detection(AudioUnit unit, AudioDeviceID device)
{
    const AudioObjectPropertyAddress address = {
        kAudioDevicePropertyVoiceActivityDetectionEnable,
        kAudioObjectPropertyScopeInput,
        kAudioObjectPropertyElementMain,
    };
    UInt32 enable = 1;
    OSStatus err = AudioObjectSetPropertyData(device, &address, 0, NULL, sizeof(enable), &enable);
    if (err) {
        return err;
    }

    // The unit copies the block, which is the same lifetime assumption WebKit makes.
    AUVoiceIOMutedSpeechActivityEventListener listener =
        ^(AUVoiceIOSpeechActivityEvent event) { (void)event; };
    return AudioUnitSetProperty(unit, kAUVoiceIOProperty_MutedSpeechActivityEventListener,
                                kAudioUnitScope_Global, 0, &listener, sizeof(listener));
}
