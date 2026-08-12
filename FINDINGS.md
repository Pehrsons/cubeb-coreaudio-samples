# Built-in microphone levels on macOS, and VoiceProcessingIO

Notes from investigating [bug 1896938](https://bugzilla.mozilla.org/show_bug.cgi?id=1896938) ("the
built-in mic is barely audible in Firefox") and
[bug 2054983](https://bugzilla.mozilla.org/show_bug.cgi?id=2054983), measured with the `vpio-levels`
binary in this repository. Machine unless stated otherwise: Mac14,9 (M2 Pro), macOS 26.6 (25G72),
built-in mic and speakers. Numbers from other machines are attributed.

## What the device does when VPIO attaches

Any VoiceProcessingIO unit attached to the built-in mic switches the device's input stream from
`96000 Hz, 1 ch` to `96000 Hz, 3 ch` — its raw mic array — **for every client on the system**:

| state | plain non-cubeb client |
| --- | --- |
| no VPIO anywhere | -53.3 dBFS |
| VPIO attached, probe reopened at 3 ch | -81.0 dBFS, per channel -84.2 / -83.8 / -81.0 |
| after the cubeb stream is destroyed | -81.7 dBFS |
| after `VPIO_IDLE_TIMEOUT` disposes the unit | -57.3 dBFS, format back to 1 ch |

All three channels are quiet, so this is not a 3→1 downmix artifact. The device recovers only when
the pooled unit is *disposed*, ten seconds after the last stream closes, which is what bug 1896938
comment 60 describes as the degraded state persisting. On a USB mic, VPIO changes no format and
costs nothing: this is built-in-mic specific.

A cubeb stream on the non-VPIO path sees the same thing — -60.0 dBFS alone, -85.8 dBFS once a VPIO
stream attaches — which is the ~26 dB the VPIO forcelist exists to avoid.

## The bypass cliff is Apple's, not ours

Expressed against a plain client measured in the same window, so room level cancels:

| | bypass vs plain client | processed vs plain client |
| --- | --- | --- |
| M2 Pro (this machine) | +40.8 dB | +38.6 dB |
| M3 Pro (bug 1896938 comment 63) | **+0.0 dB** | +39.6 dB |
| M4 (padenot) | **+0.0 dB** | +38.1 dB |

From M3 onwards a bypassed unit passes the raw array through untouched — bit-identical to a client
with no voice processing at all. The processed path is healthy everywhere.

`--scenario native-vpio` sets up a VoiceProcessingIO unit directly, with no cubeb involved, and
measures it beside cubeb's. The two agree to a tenth of a dB, on this machine and on the M4. So
none of what cubeb does to the unit — the rate it adopts, the format it imposes, the channel clamp,
the buffer size, disabling output-scope IO — causes the cliff.

## Gain knobs, and which of them work

- **The device's input volume** (`kAudioDevicePropertyVolumeScalar`, input scope) has a -12..+12 dB
  range and works — but only while no VPIO unit is attached. Driving it min to max, a 24 dB
  nominal swing: no VPIO, plain client -52.5 → -28.7 (+23.8 dB); VPIO attached, the VPIO stream
  -27.8 → -27.8 (0.0 dB) and the plain client -67.9 → -70.5; VPIO disposed again, -51.9 → -27.9
  (+24.0 dB). So attaching a VPIO unit freezes the system input gain for everyone, which matches
  comment 60 reporting the System Settings slider going unresponsive.
- **The unit's own `kHALOutputParam_Volume`** is advertised on both buses of both unit types but
  does not affect capture: 1.0 → 0.5 on bus 1 moved the level from -45.1 to -45.5, and on bus 0 to
  -46.3. It is the output-side volume, inert on a capture-only unit.
- **There is no per-app input gain.** Per process macOS offers only mute
  (`kAudioHardwarePropertyProcessInputMute`, `kAudioDevicePropertyProcessMute`); the `AudioProcess`
  objects expose PID, bundle ID, devices and running flags, no gain.

So on M3 and later there is no API-level way to compensate for the bypass path.

## AGC normalises to a target

VPIO's AGC contributes ±5 dB depending on how loud the room is, and takes 10 to 15 seconds to get
there. Measuring AGC on and off as two simultaneous units, so acoustics are identical:

| run | early (on − off) | settled |
| --- | --- | --- |
| 1 | 4.6 dB | 5.6 dB |
| 2 | 5.1 dB | 5.5 dB |
| 3 | 1.4 dB | 5.6 dB |

On a machine with a ~6 dB hotter room (an M4) the sign inverts: AGC-off measured 5.9 dB *louder*.
Settled processed levels land at -28 to -31 dBFS on both machines regardless of source level, i.e.
the AGC target. Consequences: comparing processed levels across machines mostly measures the target,
not the mic path, and two browsers both running non-bypassed VPIO with AGC on cannot differ by a
stable few dB.

## How cubeb differs from WebKit, and what each difference costs

Read from `Source/WebCore/platform/mediastream/cocoa/CoreAudioCaptureUnit.{cpp,mm}` and
`platform/audio/mac/AudioSessionMac.mm`.

| difference | measured effect |
| --- | --- |
| WebKit has no bypass concept: `shouldUseVPIO = enableEchoCancellation()`, and when false it creates a `kAudioUnitSubType_HALOutput` unit instead, so no VPIO unit exists at all | the difference that matters; ours keeps the unit and bypasses it, which on M3+ is the raw path |
| WebKit never sets `kAUVoiceIOProperty_VoiceProcessingEnableAGC`, so it keeps the unit default (on after `AudioUnitInitialize`); cubeb sets it from client params, so AEC+NS without AGC turns it off | ±5 dB, sign depending on room level; symmetric between browsers when both request AGC |
| WebKit never touches `EnableIO` for VPIO; cubeb disables the output scope for input-only streams | none measurable |
| WebKit configures the unit at the device's nominal rate; cubeb adopts the unit's 44100 default and resamples | none measurable (44100 vs 96000 within 0.5 dB) |
| WebKit sets the public `kAUVoiceIOProperty_OtherAudioDuckingConfiguration`; cubeb calls the private `audio_device_duck()` on `get_default_device(OUTPUT)`, which is the wrong device when the stream's output is not the system default, and once per unit creation rather than per stream | none measurable on level |
| WebKit enables `kAudioDevicePropertyVoiceActivityDetectionEnable` and installs `kAUVoiceIOProperty_MutedSpeechActivityEventListener` | none measurable |
| WebKit calls `AVAudioRoutingArbiter beginArbitrationWithCategory:` (`arbitration on` in the tool) | none measurable: bypass -30.0/-29.8 without, -28.8/-29.9 with |
| WebKit sets the `AVAudioSession` category to `PlayAndRecord` with mode `VideoChat`, activates the session and declares it eligible for Bluetooth smart routing (`session videochat`, `--scenario webkit-session`) | none measurable **on M2**, where the comparison cannot come out any other way; untested where it would matter |
| WebKit runs a watchdog, `verifyIsCapturing`, that calls `captureFailed()` when the microphone proc stops being called; cubeb has nothing equivalent | not a level difference, but see below |
| WebKit retains a released VPIO unit for 3 s (`delayBeforeStoredVPIOUnitDeallocation`); cubeb for 10 s (`VPIO_IDLE_TIMEOUT`) | how long the device stays in raw-array mode after capture ends |

Nothing above the audio unit adds gain in WebKit: the only gain application is
`applyGain(volume())` with `volume()` defaulting to 1, the outgoing WebRTC path has no gain or APM,
and `LibWebRTCProvider::createPeerConnectionFactory` attaches no audio processing module.

### The audio session is reachable on macOS, and is the one difference still open

WebKit's log line `setting category = PlayAndRecord, mode = VideoChat` comes from
`MediaSessionManagerCocoa::updateSessionState`, and the implementation under it is
`AudioSessionCocoa::setCategory`, which `AudioSessionMac::setCategory` calls before doing its own
arbitration. It leads to real system calls: `setEligibleForSmartRoutingInternal` calls
`-setEligibleForBTSmartRoutingConsideration:error:` on `[AVAudioSession sharedInstance]`, and
`tryToSetActiveInternal` calls `-setActive:withOptions:error:`. `HAVE_AVAUDIOSESSION_SMARTROUTING` is
defined for `PLATFORM(MAC)`.

The public SDK marks `AVAudioSession` `API_UNAVAILABLE(macos)` and does not declare the class at all,
so this looks impossible — but WebKit reaches it through `pal/spi/cocoa/AVFoundationSPI.h` and
`AVFoundationSoftLink.h`, and it works: on macOS 26.6 the class is present, `sharedInstance` returns
an object, the category starts at `SoloAmbient`, and setting `PlayAndRecord`/`VideoChat` plus
`setActive:YES` all succeed with no error. `session videochat` in the tool does exactly that.

Measured on this M2, bypassed and processed units side by side, Δ against a plain client in the same
window: 41.1 dB with no session, 41.2 with the category and mode set but not activated, 41.3
activated, 41.4 with the units created after the session was already in place. So no effect —
**but this machine has no bypass cliff to fix**, bypass already sitting 41 dB above a plain client,
so the null result is the only result it could produce. `--scenario webkit-session` on an M3 or later
is the test that would mean something. The processed unit drifting 41.0 → 37.4 across those same
windows is the NS and AGC settle, not the session: the first window is the first seconds after
start.

One thing this does not establish: `MediaSessionManagerCocoa`'s category calls are gated on
`AudioSession::shouldManageAudioSessionCategory()`, which defaults to false, so whether shipping
Safari has the session configured this way during a getUserMedia capture is still unverified.

## How Gecko can ask cubeb for no processing

Even when a page requests `aec+ns+agc`, the shared stream can end up at
`CUBEB_INPUT_PROCESSING_PARAM_NONE`, which on the built-in mic means a *bypassed* VPIO unit:

- `DeviceInputTrack::UpdateRequestedProcessingParams()` intersects (`&=`) every listener's request,
  so one consumer wanting nothing drops the whole device.
- `AudioInputProcessing::RequestedInputProcessingParams` returns `NONE` when
  `mPlatformProcessingSetError` is set, and cubeb fails the set when AEC and NS disagree ("AEC !=
  NS"), so a page asking for echo cancellation without noise suppression latches it.
- `media.getusermedia.audio.processing.platform.enabled` is true only on macOS.

Unverified against a running Firefox; this is code reading. The `MOZ_LOG` lines to confirm it are
`MediaTrackGraph:5` for "notifying of setting requested processing params", `MediaManager:5` for
"platform processing params are now"/"failed to apply", and `cubeb:5` for "set input processing
params".

## The Control Center mic mode picker is not a symptom

Instantiating a VPIO unit is necessary and sufficient for macOS to offer mic modes. With correct
attribution — the tool wrapped in an app bundle and launched by launchd, see `make-app.sh` — a
plain AUHAL client gets no picker, matching Chrome, and every VPIO configuration gets one. Varying
processing params, bypass, output-scope IO, VAD with its listener, ducking, rate, routing
arbitration, and the `com.apple.security.device.microphone` versus `audio-input` entitlements
changed nothing.

CoreAudio logs the decision for voice isolation:

```
System_Input_Processing_Notification_Handler::is_vi_available(): Voice isolation DSP is available
for client with bundle id org.mozilla.vpio-levels (AVFoundation is available, application is not in
client deny list, application is not FaceTime variant)
```

so there is a bundle-ID deny list, but Safari passes this check too — it offers Voice Isolation and
only lacks Wide Spectrum. No wide-spectrum decision is logged anywhere. Both browsers default to
Standard, so nothing unexpected is applied to either. Treat the picker as a curiosity.

## Reliability, separate from level

- A cubeb VPIO stream on the built-in mic sometimes reports `CUBEB_STATE_STARTED`, logs "started
  successfully", and never delivers a data callback: 5/5 with another client already capturing,
  including cross-process, and 3/5 with nothing else capturing. `cubeb_stream_stop` plus
  `cubeb_stream_start` does not recover it, because the same pooled unit is restarted, while
  opening a second stream works immediately. After roughly 40 create/destroy cycles the mic failed
  6/6 until coreaudiod was restarted, while a plain client on it still read -56.6 dBFS. WebKit's
  `verifyIsCapturing` watchdog exists for exactly this.
- With the `audio-dump` feature compiled in, a failed `cubeb_stream_init` runs
  `cubeb_audio_dump_stop` on a session that never started and aborts the process
  (`cubeb_audio_dump.cpp:172`).

## Measurement cautions, each learned by getting it wrong

- Compare against a **plain client measured in the same window**, and take each run's own **pre-VPIO
  baseline**. Absolute levels across runs track the room, not the change under test.
- The attenuated plain client **floors out** around -60 to -70 dBFS regardless of source, so it is
  not a valid reference while VPIO is attached, and "N dB of attenuation" computed from it is a
  lower bound.
- Measure an **early and a settled window**: the AGC ramp takes 10 to 15 s, and a single window
  starting at first buffer hides it.
- Use **speech**, not music. Noise suppression treats steady music as noise and suppresses it
  progressively, which looks exactly like a settling loss.
- A bare CLI tool's capture is attributed to the **terminal**, so anything the system scopes per app
  (TCC grants, mic modes) needs the app bundle and `open -a` with an absolute path.
- `/usr/bin/log` refuses to run sandboxed ("Cannot run while sandboxed"), and `log` is also a zsh
  builtin. WebKit's capture logging is on the `WebRTC` channel of subsystem `com.apple.WebKit`, not
  under `com.apple.coreaudio`; shipping Safari emits no VPIO *setup* logs at all.
