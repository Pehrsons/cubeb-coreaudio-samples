//! Enumeration of every gain-adjacent knob CoreAudio exposes for an input device.
//!
//! Answers, for a given machine, the question "is there anywhere to compensate for a quiet input
//! path?" — by listing the parameters an AudioUnit actually advertises (for both a
//! VoiceProcessingIO and a plain AUHAL unit on the same device), the device's own control objects
//! and their ranges, and the readable VoiceProcessingIO properties.

use std::ffi::c_void;
use std::fmt::Write as _;
use std::{mem, ptr};

use coreaudio_sys::*;

use crate::{fourcc, get_property_scoped};

const AU_IN_BUS: AudioUnitElement = 1;
const AU_OUT_BUS: AudioUnitElement = 0;

#[allow(non_upper_case_globals)]
fn scope_name(scope: AudioUnitScope) -> &'static str {
    match scope {
        kAudioUnitScope_Global => "global",
        kAudioUnitScope_Input => "input",
        kAudioUnitScope_Output => "output",
        _ => "other",
    }
}

fn parameter_unit_name(unit: u32) -> String {
    #[allow(non_upper_case_globals)]
    match unit {
        kAudioUnitParameterUnit_Generic => "generic".to_string(),
        kAudioUnitParameterUnit_Boolean => "boolean".to_string(),
        kAudioUnitParameterUnit_Decibels => "dB".to_string(),
        kAudioUnitParameterUnit_LinearGain => "linear gain".to_string(),
        kAudioUnitParameterUnit_Hertz => "Hz".to_string(),
        kAudioUnitParameterUnit_Percent => "%".to_string(),
        other => format!("unit {}", other),
    }
}

fn parameter_name(info: &AudioUnitParameterInfo) -> String {
    if !info.cfNameString.is_null() {
        return crate::string_from_cfstringref(info.cfNameString);
    }
    let bytes: Vec<u8> = info
        .name
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as u8)
        .collect();
    String::from_utf8_lossy(&bytes).to_string()
}

/// The parameters an AudioUnit advertises, across the scopes and buses that matter for capture.
fn describe_unit_parameters(unit: AudioUnit, out: &mut String) {
    let mut found_any = false;
    for scope in [
        kAudioUnitScope_Global,
        kAudioUnitScope_Input,
        kAudioUnitScope_Output,
    ] {
        for element in [AU_OUT_BUS, AU_IN_BUS] {
            let mut size: u32 = 0;
            let status = unsafe {
                AudioUnitGetPropertyInfo(
                    unit,
                    kAudioUnitProperty_ParameterList,
                    scope,
                    element,
                    &mut size,
                    ptr::null_mut(),
                )
            };
            if status != 0 || size == 0 {
                continue;
            }
            let count = size as usize / mem::size_of::<AudioUnitParameterID>();
            let mut ids: Vec<AudioUnitParameterID> = vec![0; count];
            let mut size = size;
            let status = unsafe {
                AudioUnitGetProperty(
                    unit,
                    kAudioUnitProperty_ParameterList,
                    scope,
                    element,
                    ids.as_mut_ptr() as *mut c_void,
                    &mut size,
                )
            };
            if status != 0 {
                continue;
            }
            for id in ids {
                found_any = true;
                let mut info = AudioUnitParameterInfo::default();
                let mut info_size = mem::size_of::<AudioUnitParameterInfo>() as u32;
                let status = unsafe {
                    AudioUnitGetProperty(
                        unit,
                        kAudioUnitProperty_ParameterInfo,
                        scope,
                        id,
                        &mut info as *mut AudioUnitParameterInfo as *mut c_void,
                        &mut info_size,
                    )
                };
                let mut value: AudioUnitParameterValue = 0.0;
                let value_status =
                    unsafe { AudioUnitGetParameter(unit, id, scope, element, &mut value) };
                if status == 0 {
                    let _ = writeln!(
                        out,
                        "      {} scope, bus {}: id {} \"{}\" range {}..{} default {} ({}){}",
                        scope_name(scope),
                        element,
                        id,
                        parameter_name(&info),
                        info.minValue,
                        info.maxValue,
                        info.defaultValue,
                        parameter_unit_name(info.unit),
                        if value_status == 0 {
                            format!(", currently {}", value)
                        } else {
                            String::new()
                        }
                    );
                } else {
                    let _ = writeln!(
                        out,
                        "      {} scope, bus {}: id {} (no info, err {})",
                        scope_name(scope),
                        element,
                        id,
                        status
                    );
                }
            }
        }
    }
    if !found_any {
        let _ = writeln!(out, "      (no parameters on any scope or bus)");
    }
}

fn create_unit(sub_type: u32) -> Option<AudioUnit> {
    let desc = AudioComponentDescription {
        componentType: kAudioUnitType_Output,
        componentSubType: sub_type,
        componentManufacturer: kAudioUnitManufacturer_Apple,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    let comp = unsafe { AudioComponentFindNext(ptr::null_mut(), &desc) };
    if comp.is_null() {
        return None;
    }
    let mut unit: AudioUnit = ptr::null_mut();
    if unsafe { AudioComponentInstanceNew(comp, &mut unit) } != 0 {
        return None;
    }
    Some(unit)
}

fn set_u32(
    unit: AudioUnit,
    property: u32,
    scope: AudioUnitScope,
    element: AudioUnitElement,
    value: u32,
) {
    unsafe {
        AudioUnitSetProperty(
            unit,
            property,
            scope,
            element,
            &value as *const u32 as *const c_void,
            mem::size_of::<u32>() as u32,
        )
    };
}

fn get_u32(
    unit: AudioUnit,
    property: u32,
    scope: AudioUnitScope,
    element: AudioUnitElement,
) -> Result<u32, OSStatus> {
    let mut value: u32 = 0;
    let mut size = mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioUnitGetProperty(
            unit,
            property,
            scope,
            element,
            &mut value as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    if status == 0 {
        Ok(value)
    } else {
        Err(status)
    }
}

/// Parameters and voice-processing properties of a freshly created unit on `device`.
pub fn describe_units(device: AudioDeviceID) -> String {
    let mut out = String::new();
    for (label, sub_type) in [
        ("VoiceProcessingIO", kAudioUnitSubType_VoiceProcessingIO),
        ("HALOutput (plain capture)", kAudioUnitSubType_HALOutput),
    ] {
        let _ = writeln!(out, "    {}:", label);
        let Some(unit) = create_unit(sub_type) else {
            let _ = writeln!(out, "      (could not create)");
            continue;
        };
        // The voice-processing defaults are worth catching at each configuration stage: WebKit
        // never sets AGC or bypass, so whatever these read is what Safari runs with.
        if sub_type == kAudioUnitSubType_VoiceProcessingIO {
            let stage = |unit: AudioUnit, label: &str, out: &mut String| {
                let agc = get_u32(
                    unit,
                    kAUVoiceIOProperty_VoiceProcessingEnableAGC,
                    kAudioUnitScope_Global,
                    AU_IN_BUS,
                );
                let bypass = get_u32(
                    unit,
                    kAUVoiceIOProperty_BypassVoiceProcessing,
                    kAudioUnitScope_Global,
                    AU_IN_BUS,
                );
                let fmt = |r: Result<u32, OSStatus>| {
                    r.map(|v| v.to_string())
                        .unwrap_or_else(|e| format!("err {}", e))
                };
                let _ = writeln!(
                    out,
                    "      defaults {}: AGC = {}, bypass = {}",
                    label,
                    fmt(agc),
                    fmt(bypass)
                );
            };
            stage(unit, "on a fresh instance", &mut out);
            set_u32(unit, kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Input, AU_IN_BUS, 1);
            unsafe {
                AudioUnitSetProperty(
                    unit,
                    kAudioOutputUnitProperty_CurrentDevice,
                    kAudioUnitScope_Global,
                    AU_OUT_BUS,
                    &device as *const AudioDeviceID as *const c_void,
                    mem::size_of::<AudioDeviceID>() as u32,
                )
            };
            stage(unit, "after the device is set", &mut out);
            unsafe { AudioUnitInitialize(unit) };
            stage(unit, "after AudioUnitInitialize", &mut out);
        }

        // Configure it as a capture unit on this device, so the parameter list reflects a unit that
        // is actually wired up rather than a bare instance.
        set_u32(unit, kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Input, AU_IN_BUS, 1);
        if sub_type == kAudioUnitSubType_HALOutput {
            set_u32(unit, kAudioOutputUnitProperty_EnableIO, kAudioUnitScope_Output, AU_OUT_BUS, 0);
        }
        unsafe {
            AudioUnitSetProperty(
                unit,
                kAudioOutputUnitProperty_CurrentDevice,
                kAudioUnitScope_Global,
                AU_OUT_BUS,
                &device as *const AudioDeviceID as *const c_void,
                mem::size_of::<AudioDeviceID>() as u32,
            )
        };
        unsafe { AudioUnitInitialize(unit) };

        // The format the unit reports on the input bus is what cubeb adopts for the device side
        // (mod.rs reads kAudioUnitScope_Output of the input bus for a VPIO unit). If that comes
        // back as 2 channels while the client asked for mono, cubeb's BufferManager averages the
        // pair, which halves the level when only one of them carries signal.
        for (scope, label) in [
            (kAudioUnitScope_Output, "output scope of the input bus, what cubeb reads"),
            (kAudioUnitScope_Input, "input scope of the input bus"),
        ] {
            let mut desc = AudioStreamBasicDescription::default();
            let mut size = mem::size_of::<AudioStreamBasicDescription>() as u32;
            let status = unsafe {
                AudioUnitGetProperty(
                    unit,
                    kAudioUnitProperty_StreamFormat,
                    scope,
                    AU_IN_BUS,
                    &mut desc as *mut AudioStreamBasicDescription as *mut c_void,
                    &mut size,
                )
            };
            let _ = if status == 0 {
                writeln!(
                    out,
                    "      format, {}: {} Hz, {} ch, {} bit",
                    label, desc.mSampleRate, desc.mChannelsPerFrame, desc.mBitsPerChannel
                )
            } else {
                writeln!(out, "      format, {}: unavailable (err {})", label, status)
            };
        }

        describe_unit_parameters(unit, &mut out);

        if sub_type == kAudioUnitSubType_VoiceProcessingIO {
            for (name, property, scope, element) in [
                (
                    "BypassVoiceProcessing",
                    kAUVoiceIOProperty_BypassVoiceProcessing,
                    kAudioUnitScope_Global,
                    AU_IN_BUS,
                ),
                (
                    "VoiceProcessingEnableAGC",
                    kAUVoiceIOProperty_VoiceProcessingEnableAGC,
                    kAudioUnitScope_Global,
                    AU_IN_BUS,
                ),
                ("MuteOutput", kAUVoiceIOProperty_MuteOutput, kAudioUnitScope_Global, AU_OUT_BUS),
                (
                    "VoiceProcessingQuality",
                    kAUVoiceIOProperty_VoiceProcessingQuality,
                    kAudioUnitScope_Global,
                    AU_IN_BUS,
                ),
            ] {
                let _ = match get_u32(unit, property, scope, element) {
                    Ok(value) => writeln!(out, "      property {} = {}", name, value),
                    Err(e) => writeln!(out, "      property {} unavailable (err {})", name, e),
                };
            }
        }

        unsafe {
            AudioUnitUninitialize(unit);
            AudioComponentInstanceDispose(unit);
        }
    }
    out
}

/// The device's input volume, as a scalar in 0..1, and its decibel equivalent.
pub fn get_input_volume(device: AudioDeviceID) -> Option<(f32, f32)> {
    let scalar = get_property_scoped::<f32>(
        device,
        kAudioDevicePropertyVolumeScalar,
        kAudioObjectPropertyScopeInput,
    )
    .ok()?;
    let db = get_property_scoped::<f32>(
        device,
        kAudioDevicePropertyVolumeDecibels,
        kAudioObjectPropertyScopeInput,
    )
    .unwrap_or(f32::NAN);
    Some((scalar, db))
}

/// Set the device's input volume. This is the system input slider, i.e. user-visible state, so
/// callers are expected to put it back.
pub fn set_input_volume(device: AudioDeviceID, scalar: f32) -> Result<(), OSStatus> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyVolumeScalar,
        mScope: kAudioObjectPropertyScopeInput,
        mElement: kAudioObjectPropertyElementMain,
    };
    let status = unsafe {
        AudioObjectSetPropertyData(
            device,
            &address,
            0,
            ptr::null(),
            mem::size_of::<f32>() as u32,
            &scalar as *const f32 as *const c_void,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(status)
    }
}

fn control_class_name(class: AudioClassID) -> String {
    #[allow(non_upper_case_globals)]
    match class {
        kAudioVolumeControlClassID => "Volume".to_string(),
        kAudioLevelControlClassID => "Level".to_string(),
        kAudioMuteControlClassID => "Mute".to_string(),
        kAudioBooleanControlClassID => "Boolean".to_string(),
        kAudioSelectorControlClassID => "Selector".to_string(),
        kAudioDataSourceControlClassID => "DataSource".to_string(),
        kAudioSliderControlClassID => "Slider".to_string(),
        kAudioLineLevelControlClassID => "LineLevel".to_string(),
        kAudioHighPassFilterControlClassID => "HighPassFilter".to_string(),
        kAudioStereoPanControlClassID => "StereoPan".to_string(),
        kAudioClipLightControlClassID => "ClipLight".to_string(),
        kAudioPhantomPowerControlClassID => "PhantomPower".to_string(),
        other => fourcc(other),
    }
}

/// Every control object the device owns in its input scope, with the values of the level ones.
pub fn describe_device_controls(device: AudioDeviceID) -> String {
    let mut out = String::new();
    let controls =
        crate::get_list_property::<AudioObjectID>(device, kAudioObjectPropertyControlList)
            .unwrap_or_default();
    if controls.is_empty() {
        let _ = writeln!(out, "    (device owns no control objects)");
    }
    for control in controls {
        let class = get_property_scoped::<AudioClassID>(
            control,
            kAudioObjectPropertyClass,
            kAudioObjectPropertyScopeGlobal,
        );
        let scope = get_property_scoped::<AudioObjectPropertyScope>(
            control,
            kAudioControlPropertyScope,
            kAudioObjectPropertyScopeGlobal,
        );
        let element = get_property_scoped::<AudioObjectPropertyElement>(
            control,
            kAudioControlPropertyElement,
            kAudioObjectPropertyScopeGlobal,
        );
        // Only the input side is interesting when hunting for capture gain.
        if scope
            .map(|s| s != kAudioObjectPropertyScopeInput)
            .unwrap_or(false)
        {
            continue;
        }
        let mut line = format!(
            "    control {}: {} (scope {}, element {})",
            control,
            class
                .map(control_class_name)
                .unwrap_or_else(|e| format!("err {}", e)),
            scope.map(fourcc).unwrap_or_else(|e| format!("err {}", e)),
            element
                .map(|e| e.to_string())
                .unwrap_or_else(|e| format!("err {}", e)),
        );
        if let Ok(scalar) = get_property_scoped::<f32>(
            control,
            kAudioLevelControlPropertyScalarValue,
            kAudioObjectPropertyScopeGlobal,
        ) {
            let _ = write!(line, ", scalar {}", scalar);
        }
        if let Ok(db) = get_property_scoped::<f32>(
            control,
            kAudioLevelControlPropertyDecibelValue,
            kAudioObjectPropertyScopeGlobal,
        ) {
            let _ = write!(line, ", {} dB", db);
        }
        if let Ok(range) = get_property_scoped::<AudioValueRange>(
            control,
            kAudioLevelControlPropertyDecibelRange,
            kAudioObjectPropertyScopeGlobal,
        ) {
            let _ = write!(line, ", range {}..{} dB", range.mMinimum, range.mMaximum);
        }
        let _ = writeln!(out, "{}", line);
    }

    // The device-level properties a client would actually reach for, which are the same controls
    // seen through the device object.
    let input = kAudioObjectPropertyScopeInput;
    let _ = writeln!(out, "    device properties, input scope:");
    for (name, value) in [
        (
            "VolumeScalar[main]",
            get_property_scoped::<f32>(device, kAudioDevicePropertyVolumeScalar, input)
                .map(|v| v.to_string()),
        ),
        (
            "VolumeDecibels[main]",
            get_property_scoped::<f32>(device, kAudioDevicePropertyVolumeDecibels, input)
                .map(|v| v.to_string()),
        ),
        (
            "VolumeRangeDecibels[main]",
            get_property_scoped::<AudioValueRange>(
                device,
                kAudioDevicePropertyVolumeRangeDecibels,
                input,
            )
            .map(|r| format!("{}..{}", r.mMinimum, r.mMaximum)),
        ),
        (
            "HighPassFilterSetting",
            get_property_scoped::<u32>(device, kAudioDevicePropertyHighPassFilterSetting, input)
                .map(|v| v.to_string()),
        ),
    ] {
        let _ = match value {
            Ok(value) => writeln!(out, "      {} = {}", name, value),
            Err(e) => writeln!(out, "      {} unavailable (err {})", name, e),
        };
    }
    out
}
