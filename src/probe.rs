//! A plain HAL input capture client, independent of cubeb.
//!
//! This is the equivalent of another app (Chrome, QuickTime, the macOS input meter) capturing the
//! same device at the same time as cubeb: an AUHAL unit with input enabled and no voice processing
//! whatsoever. It exists to measure what such a client receives while cubeb has VoiceProcessingIO
//! hooked up to the same device.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::{mem, ptr};

use coreaudio_sys::*;

use crate::meter::Meter;

const AU_IN_BUS: AudioUnitElement = 1;
const AU_OUT_BUS: AudioUnitElement = 0;
// Renders bigger than this are dropped rather than allocating on the audio thread. The HAL asks for
// the device buffer frame size, which is orders of magnitude below this.
const MAX_RENDER_FRAMES: usize = 16384;

pub struct InputProbe {
    unit: AudioUnit,
    // Boxed so the address handed to the audio unit as a callback refcon stays valid on moves.
    state: Box<ProbeState>,
    device: AudioDeviceID,
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
    /// Open (but don't start) a capture client on `device`, metering into `meter`.
    pub fn new(device: AudioDeviceID, meter: Arc<Meter>) -> Result<Self, OSStatus> {
        let desc = AudioComponentDescription {
            componentType: kAudioUnitType_Output,
            componentSubType: kAudioUnitSubType_HALOutput,
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

        let probe = |unit: AudioUnit| -> Result<(f64, usize), OSStatus> {
            // Input on, output off: a capture-only client.
            set_prop(
                unit,
                kAudioOutputUnitProperty_EnableIO,
                kAudioUnitScope_Input,
                AU_IN_BUS,
                &1u32,
            )?;
            set_prop(
                unit,
                kAudioOutputUnitProperty_EnableIO,
                kAudioUnitScope_Output,
                AU_OUT_BUS,
                &0u32,
            )?;
            set_prop(
                unit,
                kAudioOutputUnitProperty_CurrentDevice,
                kAudioUnitScope_Global,
                AU_OUT_BUS,
                &device,
            )?;

            // Capture at the hardware's rate and channel count, so that nothing in the AU chain
            // resamples or mixes what the device delivers.
            let hw_desc: AudioStreamBasicDescription =
                get_prop(unit, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, AU_IN_BUS)?;
            let rate = hw_desc.mSampleRate;
            let channels = hw_desc.mChannelsPerFrame as usize;
            let client_desc = AudioStreamBasicDescription {
                mSampleRate: rate,
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
            Ok((rate, channels))
        };

        let (rate, channels) = match probe(unit) {
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
