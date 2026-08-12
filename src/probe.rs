//! Native input capture clients, independent of cubeb.
//!
//! Two kinds. An AUHAL unit with input enabled and no voice processing is the equivalent of
//! another app (Chrome, QuickTime, the macOS input meter) capturing the same device at the same
//! time as cubeb. A VoiceProcessingIO unit set up here rather than by cubeb answers the other
//! half: whether a level behaviour belongs to Apple's voice processing or to the way cubeb
//! configures it (rate, format, channel count, buffer size, output-scope IO).

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::{mem, ptr};

use coreaudio_sys::*;

use crate::meter::Meter;

extern "C" {
    /// Enables the device's voice activity detection and installs a muted-speech listener on the
    /// unit, as WebKit's CoreAudioCaptureUnit does. In C because the listener is an ObjC block.
    fn vpio_enable_voice_activity_detection(unit: AudioUnit, device: AudioDeviceID) -> OSStatus;
}

const AU_IN_BUS: AudioUnitElement = 1;
const AU_OUT_BUS: AudioUnitElement = 0;
// Renders bigger than this are dropped rather than allocating on the audio thread. The HAL asks for
// the device buffer frame size, which is orders of magnitude below this.
const MAX_RENDER_FRAMES: usize = 16384;

/// What to instantiate, and how to configure it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ProbeKind {
    /// A plain AUHAL capture unit: no voice processing at all.
    Hal,
    /// A VoiceProcessingIO unit configured here rather than by cubeb. `params` uses the same bits
    /// as cubeb's input processing params; `None` leaves the unit's own defaults alone, which is
    /// what WebKit does.
    Vpio {
        params: Option<u32>,
        /// Enable voice activity detection and install the muted-speech listener, as WebKit does.
        voice_activity: bool,
        /// Configure other-audio ducking through the public property, as WebKit does, rather than
        /// leaving it to cubeb's private audio_device_duck call.
        ducking: bool,
        /// Set the unit's capture format to this rate, as WebKit does with the device's nominal
        /// rate. `None` leaves the format the unit advertises alone, imposing nothing.
        rate: Option<f64>,
        /// Disable IO on the output scope, as cubeb does for an input-only stream. WebKit never
        /// touches EnableIO for a VPIO unit, so the default here is to leave it alone.
        disable_output_io: bool,
    },
}

pub struct InputProbe {
    unit: AudioUnit,
    // Boxed so the address handed to the audio unit as a callback refcon stays valid on moves.
    state: Box<ProbeState>,
    device: AudioDeviceID,
    kind: ProbeKind,
    rate: f64,
    channels: usize,
    started: AtomicBool,
}

struct ProbeState {
    unit: AudioUnit,
    meter: Arc<Meter>,
    channels: usize,
    buffer: Vec<f32>,
}

impl InputProbe {
    /// Open (but don't start) a plain capture client on `device`, metering into `meter`.
    pub fn new(device: AudioDeviceID, meter: Arc<Meter>) -> Result<Self, OSStatus> {
        Self::with_kind(device, meter, ProbeKind::Hal)
    }

    /// Open (but don't start) a capture client of the given kind.
    pub fn with_kind(
        device: AudioDeviceID,
        meter: Arc<Meter>,
        kind: ProbeKind,
    ) -> Result<Self, OSStatus> {
        let desc = AudioComponentDescription {
            componentType: kAudioUnitType_Output,
            componentSubType: match kind {
                ProbeKind::Hal => kAudioUnitSubType_HALOutput,
                ProbeKind::Vpio { .. } => kAudioUnitSubType_VoiceProcessingIO,
            },
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        let comp = unsafe { AudioComponentFindNext(ptr::null_mut(), &desc) };
        if comp.is_null() {
            return Err(-1);
        }
        let mut unit: AudioUnit = ptr::null_mut();
        check(unsafe { AudioComponentInstanceNew(comp, &mut unit) })?;

        let configure = |unit: AudioUnit| -> Result<(f64, usize), OSStatus> {
            // Input on. For a plain unit also turn the output side off, since it is a capture-only
            // client. For VPIO, only do that when asked: cubeb disables the output scope for an
            // input-only stream, WebKit leaves the unit's default duplex configuration alone, and
            // that difference is one of the things worth measuring.
            set_prop(
                unit,
                kAudioOutputUnitProperty_EnableIO,
                kAudioUnitScope_Input,
                AU_IN_BUS,
                &1u32,
            )?;
            let disable_output = match kind {
                ProbeKind::Hal => true,
                ProbeKind::Vpio {
                    disable_output_io, ..
                } => disable_output_io,
            };
            if disable_output {
                set_prop(
                    unit,
                    kAudioOutputUnitProperty_EnableIO,
                    kAudioUnitScope_Output,
                    AU_OUT_BUS,
                    &0u32,
                )?;
            }
            // The element matters for VPIO, which can have separate input and output devices, so
            // the capture device goes on the input bus -- what cubeb's set_device_to_audiounit and
            // WebKit both do. For AUHAL the element is ignored and bus 0 is the convention.
            set_prop(
                unit,
                kAudioOutputUnitProperty_CurrentDevice,
                kAudioUnitScope_Global,
                match kind {
                    ProbeKind::Hal => AU_OUT_BUS,
                    ProbeKind::Vpio { .. } => AU_IN_BUS,
                },
                &device,
            )?;

            if let ProbeKind::Vpio { ducking: true, .. } = kind {
                let configuration = AUVoiceIOOtherAudioDuckingConfiguration {
                    mEnableAdvancedDucking: 1,
                    mDuckingLevel: kAUVoiceIOOtherAudioDuckingLevelMin
                        as AUVoiceIOOtherAudioDuckingLevel,
                };
                // WebKit tolerates this property being unavailable, so do the same.
                match set_prop(
                    unit,
                    kAUVoiceIOProperty_OtherAudioDuckingConfiguration,
                    kAudioUnitScope_Global,
                    AU_IN_BUS,
                    &configuration,
                ) {
                    Ok(()) => {}
                    Err(e) if e == kAudioUnitErr_InvalidProperty as OSStatus => {}
                    Err(e) => return Err(e),
                }
            }

            match kind {
                ProbeKind::Hal => {
                    // Capture at the hardware's rate and channel count, so that nothing in the AU
                    // chain resamples or mixes what the device delivers.
                    let hw_desc: AudioStreamBasicDescription = get_prop(
                        unit,
                        kAudioUnitProperty_StreamFormat,
                        kAudioUnitScope_Input,
                        AU_IN_BUS,
                    )?;
                    let client_desc = AudioStreamBasicDescription {
                        mSampleRate: hw_desc.mSampleRate,
                        mFormatID: kAudioFormatLinearPCM,
                        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
                        mBytesPerPacket: 4 * hw_desc.mChannelsPerFrame,
                        mFramesPerPacket: 1,
                        mBytesPerFrame: 4 * hw_desc.mChannelsPerFrame,
                        mChannelsPerFrame: hw_desc.mChannelsPerFrame,
                        mBitsPerChannel: 32,
                        mReserved: 0,
                    };
                    set_prop(
                        unit,
                        kAudioUnitProperty_StreamFormat,
                        kAudioUnitScope_Output,
                        AU_IN_BUS,
                        &client_desc,
                    )?;
                    Ok((hw_desc.mSampleRate, hw_desc.mChannelsPerFrame as usize))
                }
                ProbeKind::Vpio { rate, .. } => {
                    // Read what the unit advertises on the scope it delivers from. Without a rate,
                    // impose nothing, so this measures the unit as it comes. With one, set the
                    // format's rate the way WebKit configures the mic proc.
                    let mut desc: AudioStreamBasicDescription = get_prop(
                        unit,
                        kAudioUnitProperty_StreamFormat,
                        kAudioUnitScope_Output,
                        AU_IN_BUS,
                    )?;
                    if let Some(rate) = rate {
                        desc.mSampleRate = rate;
                        set_prop(
                            unit,
                            kAudioUnitProperty_StreamFormat,
                            kAudioUnitScope_Output,
                            AU_IN_BUS,
                            &desc,
                        )?;
                        // The output side has to match even when it is unused, or
                        // AudioUnitInitialize fails with -10875. cubeb says the same thing at
                        // mod.rs:4048, and WebKit configures its speaker proc alongside the mic one.
                        set_prop(
                            unit,
                            kAudioUnitProperty_StreamFormat,
                            kAudioUnitScope_Input,
                            AU_OUT_BUS,
                            &desc,
                        )?;
                        // Read it back: the unit may not accept the rate.
                        desc = get_prop(
                            unit,
                            kAudioUnitProperty_StreamFormat,
                            kAudioUnitScope_Output,
                            AU_IN_BUS,
                        )?;
                    }
                    Ok((desc.mSampleRate, desc.mChannelsPerFrame as usize))
                }
            }
        };

        let (rate, channels) = match configure(unit) {
            Ok(v) => v,
            Err(e) => {
                unsafe { AudioComponentInstanceDispose(unit) };
                return Err(e);
            }
        };

        let mut probe = Self {
            unit,
            state: Box::new(ProbeState {
                unit,
                meter,
                channels,
                buffer: vec![0.0; MAX_RENDER_FRAMES * channels.max(1)],
            }),
            device,
            kind,
            rate,
            channels,
            started: AtomicBool::new(false),
        };

        let callback = AURenderCallbackStruct {
            inputProc: Some(input_callback),
            inputProcRefCon: probe.state.as_mut() as *mut ProbeState as *mut c_void,
        };
        let init = || -> Result<(), OSStatus> {
            set_prop(
                unit,
                kAudioOutputUnitProperty_SetInputCallback,
                kAudioUnitScope_Global,
                AU_OUT_BUS,
                &callback,
            )?;
            check(unsafe { AudioUnitInitialize(unit) })
        };
        if let Err(e) = init() {
            unsafe { AudioComponentInstanceDispose(unit) };
            probe.unit = ptr::null_mut();
            return Err(e);
        }

        if let ProbeKind::Vpio {
            voice_activity: true,
            ..
        } = kind
        {
            check(unsafe { vpio_enable_voice_activity_detection(unit, device) })?;
        }

        // Voice-processing params go on after initialization, which is where the unit's own
        // defaults have settled (AGC reads 0 before AudioUnitInitialize on some machines).
        if let ProbeKind::Vpio {
            params: Some(params),
            ..
        } = kind
        {
            let aec = params & 0x01 != 0;
            let agc = params & 0x04 != 0;
            let bypass = u32::from(!aec);
            set_prop(
                unit,
                kAUVoiceIOProperty_BypassVoiceProcessing,
                kAudioUnitScope_Global,
                AU_IN_BUS,
                &bypass,
            )?;
            set_prop(
                unit,
                kAUVoiceIOProperty_VoiceProcessingEnableAGC,
                kAudioUnitScope_Global,
                AU_IN_BUS,
                &u32::from(agc),
            )?;
        }

        Ok(probe)
    }

    pub fn start(&self) -> Result<(), OSStatus> {
        check(unsafe { AudioOutputUnitStart(self.unit) })?;
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn stop(&self) -> Result<(), OSStatus> {
        if self.started.swap(false, Ordering::SeqCst) {
            check(unsafe { AudioOutputUnitStop(self.unit) })?;
        }
        Ok(())
    }

    /// Set this unit's own Volume parameter (`kHALOutputParam_Volume`). Unlike the device's input
    /// volume this is per-client state, so it is the only candidate for an app-scoped input gain.
    pub fn set_unit_volume(&self, value: f32, element: AudioUnitElement) -> Result<(), OSStatus> {
        check(unsafe {
            AudioUnitSetParameter(
                self.unit,
                kHALOutputParam_Volume,
                kAudioUnitScope_Global,
                element,
                value,
                0,
            )
        })
    }

    pub fn kind(&self) -> ProbeKind {
        self.kind
    }

    pub fn device(&self) -> AudioDeviceID {
        self.device
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }
}

impl Drop for InputProbe {
    fn drop(&mut self) {
        if self.unit.is_null() {
            return;
        }
        let _ = self.stop();
        unsafe {
            AudioUnitUninitialize(self.unit);
            AudioComponentInstanceDispose(self.unit);
        }
    }
}

unsafe extern "C" fn input_callback(
    ref_con: *mut c_void,
    flags: *mut AudioUnitRenderActionFlags,
    time_stamp: *const AudioTimeStamp,
    bus: UInt32,
    frames: UInt32,
    _data: *mut AudioBufferList,
) -> OSStatus {
    let state = &mut *(ref_con as *mut ProbeState);
    let frames = frames as usize;
    let channels = state.channels.max(1);
    if frames == 0 || frames * channels > state.buffer.len() {
        state.meter.add_empty_callback();
        return 0;
    }

    let mut list = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: channels as u32,
            mDataByteSize: (frames * channels * mem::size_of::<f32>()) as u32,
            mData: state.buffer.as_mut_ptr() as *mut c_void,
        }],
    };
    let status = AudioUnitRender(state.unit, flags, time_stamp, bus, frames as u32, &mut list);
    if status != 0 {
        state.meter.add_empty_callback();
        return status;
    }

    state
        .meter
        .add_interleaved(&state.buffer[..frames * channels], channels);
    0
}

fn check(status: OSStatus) -> Result<(), OSStatus> {
    if status == 0 {
        Ok(())
    } else {
        Err(status)
    }
}

fn set_prop<T>(
    unit: AudioUnit,
    property: AudioUnitPropertyID,
    scope: AudioUnitScope,
    element: AudioUnitElement,
    value: &T,
) -> Result<(), OSStatus> {
    check(unsafe {
        AudioUnitSetProperty(
            unit,
            property,
            scope,
            element,
            value as *const T as *const c_void,
            mem::size_of::<T>() as u32,
        )
    })
}

fn get_prop<T: Default>(
    unit: AudioUnit,
    property: AudioUnitPropertyID,
    scope: AudioUnitScope,
    element: AudioUnitElement,
) -> Result<T, OSStatus> {
    let mut value = T::default();
    let mut size = mem::size_of::<T>() as u32;
    check(unsafe {
        AudioUnitGetProperty(
            unit,
            property,
            scope,
            element,
            &mut value as *mut T as *mut c_void,
            &mut size,
        )
    })?;
    Ok(value)
}
