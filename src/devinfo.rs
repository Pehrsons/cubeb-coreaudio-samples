//! Snapshots of the CoreAudio state of an input device, and diffs between them.
//!
//! Levels alone say that something attenuates the signal; these snapshots are for finding out
//! *what*. Taking one before, during and after a VoiceProcessingIO unit is hooked up to a device
//! shows which device properties (gain, format, data source, voice activity detection, ...) the
//! system changes underneath.

use std::fmt::Write as _;
use std::mem;

use coreaudio_sys::*;

use crate::{audio_object_get_property_data, get_list_property_scoped, get_property_scoped};

pub fn default_input_device() -> Option<AudioDeviceID> {
    match crate::get_property::<AudioObjectID>(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyDefaultInputDevice,
    ) {
        Ok(id) if id != kAudioObjectUnknown => Some(id),
        _ => None,
    }
}

pub fn device_name(device: AudioDeviceID) -> String {
    crate::get_string_property(device, kAudioObjectPropertyName)
        .unwrap_or_else(|e| format!("<{}>", e))
}

pub fn device_uid(device: AudioDeviceID) -> Option<String> {
    crate::get_string_property(device, kAudioDevicePropertyDeviceUID).ok()
}

/// Channels in the device's first input stream, as clients currently see it. The built-in mic
/// changes this underneath running clients (it exposes its raw mic array when voice processing
/// attaches), which matters when comparing levels.
pub fn input_channels(device: AudioDeviceID) -> Option<u32> {
    let streams = get_list_property_scoped::<AudioStreamID>(
        device,
        kAudioDevicePropertyStreams,
        kAudioObjectPropertyScopeInput,
    )
    .ok()?;
    let stream = streams.first()?;
    get_property_scoped::<AudioStreamBasicDescription>(
        *stream,
        kAudioStreamPropertyVirtualFormat,
        kAudioObjectPropertyScopeGlobal,
    )
    .ok()
    .map(|format| format.mChannelsPerFrame)
}

/// The input channel count the way cubeb-coreaudio counts it (`get_channel_count`): one per
/// stream for a device the VPIO forcelist covers, since a VPIO unit is mono, otherwise the sum of
/// the streams' virtual formats. Requesting more channels than this makes cubeb's stream_init
/// fail with InvalidParameter.
pub fn cubeb_input_channel_count(device: AudioDeviceID) -> Option<u32> {
    let streams = get_list_property_scoped::<AudioStreamID>(
        device,
        kAudioDevicePropertyStreams,
        kAudioObjectPropertyScopeInput,
    )
    .ok()?;
    let forcelisted = cfg!(feature = "vpio-forcelist")
        && get_property_scoped::<u32>(
            device,
            kAudioDevicePropertyTransportType,
            kAudioObjectPropertyScopeGlobal,
        ) == Ok(kAudioDeviceTransportTypeBuiltIn);
    let mut count = 0;
    for stream in streams {
        if forcelisted {
            count += 1;
        } else {
            count += get_property_scoped::<AudioStreamBasicDescription>(
                stream,
                kAudioStreamPropertyVirtualFormat,
                kAudioObjectPropertyScopeGlobal,
            )
            .map(|f| f.mChannelsPerFrame)
            .unwrap_or(0);
        }
    }
    Some(count)
}

pub fn default_output_device() -> Option<AudioDeviceID> {
    match crate::get_property::<AudioObjectID>(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyDefaultOutputDevice,
    ) {
        Ok(id) if id != kAudioObjectUnknown => Some(id),
        _ => None,
    }
}

/// All devices that have at least one stream in `scope`, with their names.
pub fn devices_in_scope(scope: AudioObjectPropertyScope) -> Vec<(AudioDeviceID, String)> {
    let devices = crate::get_list_property::<AudioObjectID>(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyDevices,
    )
    .unwrap_or_default();
    devices
        .into_iter()
        .filter(|id| {
            get_list_property_scoped::<AudioStreamID>(*id, kAudioDevicePropertyStreams, scope)
                .map(|streams| !streams.is_empty())
                .unwrap_or(false)
        })
        .map(|id| (id, device_name(id)))
        .collect()
}

pub fn input_devices() -> Vec<(AudioDeviceID, String)> {
    devices_in_scope(kAudioObjectPropertyScopeInput)
}

pub fn output_devices() -> Vec<(AudioDeviceID, String)> {
    devices_in_scope(kAudioObjectPropertyScopeOutput)
}

/// Resolve a device given either its numeric `AudioDeviceID` or a case-insensitive substring of
/// its name.
pub fn resolve_output_device(spec: &str) -> Result<AudioDeviceID, String> {
    resolve_device(spec, output_devices())
}

pub fn resolve_input_device(spec: &str) -> Result<AudioDeviceID, String> {
    resolve_device(spec, input_devices())
}

fn resolve_device(
    spec: &str,
    devices: Vec<(AudioDeviceID, String)>,
) -> Result<AudioDeviceID, String> {
    if let Ok(id) = spec.parse::<AudioDeviceID>() {
        return match devices.iter().find(|(device, _)| *device == id) {
            Some(_) => Ok(id),
            None => Err(format!("No device with id {} in that scope", id)),
        };
    }
    let needle = spec.to_lowercase();
    let matches: Vec<&(AudioDeviceID, String)> = devices
        .iter()
        .filter(|(_, name)| name.to_lowercase().contains(&needle))
        .collect();
    match matches.as_slice() {
        [(id, _)] => Ok(*id),
        [] => Err(format!(
            "No device matching \"{}\". Available: {}",
            spec,
            devices
                .iter()
                .map(|(id, name)| format!("{} \"{}\"", id, name))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        many => Err(format!(
            "\"{}\" matches several devices: {}",
            spec,
            many.iter()
                .map(|(id, name)| format!("{} \"{}\"", id, name))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn get_element<T: Default>(
    device: AudioDeviceID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
    element: AudioObjectPropertyElement,
) -> Result<T, OSStatus> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: element,
    };
    let mut value = T::default();
    let mut size = mem::size_of::<T>();
    match audio_object_get_property_data(device, &address, &mut size, &mut value) {
        0 => Ok(value),
        e => Err(e),
    }
}

fn fmt_asbd(d: &AudioStreamBasicDescription) -> String {
    format!(
        "{} Hz, {} ch, {} bit, fmt {}, flags {:#x}",
        d.mSampleRate,
        d.mChannelsPerFrame,
        d.mBitsPerChannel,
        crate::fourcc(d.mFormatID),
        d.mFormatFlags
    )
}

/// A set of input-relevant device properties, captured at one point in time.
#[derive(Clone, Debug, Default)]
pub struct DeviceSnapshot {
    pub device: AudioDeviceID,
    entries: Vec<(String, String)>,
}

impl DeviceSnapshot {
    pub fn capture(device: AudioDeviceID) -> Self {
        let mut snapshot = Self {
            device,
            entries: Vec::new(),
        };
        snapshot.collect();
        snapshot
    }

    fn push<T: std::fmt::Debug>(&mut self, key: &str, value: Result<T, OSStatus>) {
        let value = match value {
            Ok(v) => format!("{:?}", v),
            Err(e) => format!("err {}", e),
        };
        self.entries.push((key.to_string(), value));
    }

    fn collect(&mut self) {
        let device = self.device;
        let input = kAudioObjectPropertyScopeInput;
        let global = kAudioObjectPropertyScopeGlobal;

        self.push("name", crate::get_string_property(device, kAudioObjectPropertyName));
        self.push("uid", crate::get_string_property(device, kAudioDevicePropertyDeviceUID));
        self.push(
            "transport",
            get_property_scoped::<u32>(device, kAudioDevicePropertyTransportType, global)
                .map(crate::fourcc),
        );
        self.push(
            "running",
            get_property_scoped::<u32>(device, kAudioDevicePropertyDeviceIsRunning, global),
        );
        self.push(
            "running_somewhere",
            get_property_scoped::<u32>(
                device,
                kAudioDevicePropertyDeviceIsRunningSomewhere,
                global,
            ),
        );
        self.push(
            "hog_mode",
            get_property_scoped::<pid_t>(device, kAudioDevicePropertyHogMode, global),
        );
        self.push(
            "nominal_rate",
            get_property_scoped::<f64>(device, kAudioDevicePropertyNominalSampleRate, global),
        );
        self.push(
            "actual_rate",
            get_property_scoped::<f64>(device, kAudioDevicePropertyActualSampleRate, global),
        );
        self.push(
            "buffer_frames",
            get_property_scoped::<u32>(device, kAudioDevicePropertyBufferFrameSize, input),
        );

        // Gain and mute, on the device as a whole (main element) and per channel. If the system
        // reconfigures the mic's gain when voice processing attaches, it shows up here.
        for element in 0..=4u32 {
            let label = if element == kAudioObjectPropertyElementMain {
                "main".to_string()
            } else {
                element.to_string()
            };
            let scalar =
                get_element::<f32>(device, kAudioDevicePropertyVolumeScalar, input, element);
            let db = get_element::<f32>(device, kAudioDevicePropertyVolumeDecibels, input, element);
            let mute = get_element::<u32>(device, kAudioDevicePropertyMute, input, element);
            // Only report elements that actually have controls.
            if scalar.is_ok() || db.is_ok() || mute.is_ok() {
                if scalar.is_ok() {
                    self.push(&format!("volume_scalar[{}]", label), scalar);
                }
                if db.is_ok() {
                    self.push(&format!("volume_db[{}]", label), db);
                }
                if mute.is_ok() {
                    self.push(&format!("mute[{}]", label), mute);
                }
            }
        }

        self.push(
            "process_mute",
            get_property_scoped::<u32>(device, kAudioDevicePropertyProcessMute, input),
        );
        self.push(
            "data_source",
            get_property_scoped::<u32>(device, kAudioDevicePropertyDataSource, input)
                .map(crate::fourcc),
        );
        self.push(
            "vad_enable",
            get_property_scoped::<u32>(
                device,
                kAudioDevicePropertyVoiceActivityDetectionEnable,
                input,
            ),
        );
        self.push(
            "vad_state",
            get_property_scoped::<u32>(
                device,
                kAudioDevicePropertyVoiceActivityDetectionState,
                input,
            ),
        );

        // The format the device presents to clients, and the format it runs its hardware at. A
        // "voice" mode on the hardware would show up as a physical format change.
        match get_list_property_scoped::<AudioStreamID>(device, kAudioDevicePropertyStreams, input)
        {
            Ok(streams) => {
                self.entries
                    .push(("input_streams".to_string(), format!("{}", streams.len())));
                for (i, stream) in streams.iter().enumerate() {
                    self.push(
                        &format!("stream[{}].active", i),
                        get_property_scoped::<u32>(*stream, kAudioStreamPropertyIsActive, global),
                    );
                    self.push(
                        &format!("stream[{}].virtual", i),
                        get_property_scoped::<AudioStreamBasicDescription>(
                            *stream,
                            kAudioStreamPropertyVirtualFormat,
                            global,
                        )
                        .map(|d| fmt_asbd(&d)),
                    );
                    self.push(
                        &format!("stream[{}].physical", i),
                        get_property_scoped::<AudioStreamBasicDescription>(
                            *stream,
                            kAudioStreamPropertyPhysicalFormat,
                            global,
                        )
                        .map(|d| fmt_asbd(&d)),
                    );
                    self.push(
                        &format!("stream[{}].terminal", i),
                        get_property_scoped::<u32>(
                            *stream,
                            kAudioStreamPropertyTerminalType,
                            global,
                        )
                        .map(crate::fourcc),
                    );
                }
            }
            Err(e) => self
                .entries
                .push(("input_streams".to_string(), format!("err {}", e))),
        }

        self.push(
            "process_input_mute",
            get_property_scoped::<u32>(
                kAudioObjectSystemObject,
                kAudioHardwarePropertyProcessInputMute,
                global,
            ),
        );
    }

    pub fn describe(&self) -> String {
        let mut out = String::new();
        for (key, value) in &self.entries {
            let _ = writeln!(out, "    {:<24} {}", key, value);
        }
        out
    }

    /// Properties whose values differ from `earlier`, formatted as `key: old -> new`.
    pub fn diff(&self, earlier: &DeviceSnapshot) -> Vec<String> {
        let mut diffs = Vec::new();
        for (key, value) in &self.entries {
            match earlier.entries.iter().find(|(k, _)| k == key) {
                Some((_, old)) if old == value => {}
                Some((_, old)) => diffs.push(format!("{}: {} -> {}", key, old, value)),
                None => diffs.push(format!("{}: (absent) -> {}", key, value)),
            }
        }
        for (key, old) in &earlier.entries {
            if !self.entries.iter().any(|(k, _)| k == key) {
                diffs.push(format!("{}: {} -> (absent)", key, old));
            }
        }
        diffs
    }
}
