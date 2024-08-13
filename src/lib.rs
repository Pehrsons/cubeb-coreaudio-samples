use std::ffi::CString;
use std::fmt;
use std::mem;

use coreaudio_sys::*;
use debug_tree::{add_branch, add_leaf, default_tree};

use std::os::raw::c_void;
use std::ptr;

#[derive(Debug)]
struct StringRef(CFStringRef);

#[allow(dead_code)]
impl StringRef {
    fn new(string_ref: CFStringRef) -> Self {
        assert!(!string_ref.is_null());
        Self(string_ref)
    }

    fn into_string(self) -> String {
        self.to_string()
    }

    fn to_cstring(&self) -> CString {
        unsafe {
            // Assume that bytes doesn't contain `0` in the middle.
            CString::from_vec_unchecked(utf8_from_cfstringref(self.0))
        }
    }

    fn into_cstring(self) -> CString {
        self.to_cstring()
    }

    fn get_raw(&self) -> CFStringRef {
        self.0
    }
}

fn utf8_from_cfstringref(string_ref: CFStringRef) -> Vec<u8> {
    use std::ptr;

    assert!(!string_ref.is_null());

    let length: CFIndex = unsafe { CFStringGetLength(string_ref) };
    if length == 0 {
        return Vec::new();
    }

    // Get the buffer size of the string.
    let range: CFRange = CFRange {
        location: 0,
        length,
    };
    let mut size: CFIndex = 0;
    let mut converted_chars: CFIndex = unsafe {
        CFStringGetBytes(
            string_ref,
            range,
            kCFStringEncodingUTF8,
            0,
            false as Boolean,
            ptr::null_mut() as *mut u8,
            0,
            &mut size,
        )
    };
    assert!(converted_chars > 0 && size > 0);

    // Then, allocate the buffer with the required size and actually copy data into it.
    let mut buffer = vec![b'\x00'; size as usize];
    converted_chars = unsafe {
        CFStringGetBytes(
            string_ref,
            range,
            kCFStringEncodingUTF8,
            0,
            false as Boolean,
            buffer.as_mut_ptr(),
            size,
            ptr::null_mut() as *mut CFIndex,
        )
    };
    assert!(converted_chars > 0);

    buffer
}

impl Drop for StringRef {
    fn drop(&mut self) {
        use std::os::raw::c_void;
        unsafe { CFRelease(self.0 as *mut c_void) };
    }
}

impl fmt::Display for StringRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let string =
            String::from_utf8(utf8_from_cfstringref(self.0)).expect("convert bytes to a String");
        write!(f, "{}", string)
    }
}

pub fn audio_object_has_property(id: AudioObjectID, address: &AudioObjectPropertyAddress) -> bool {
    unsafe { AudioObjectHasProperty(id, address) != 0 }
}

pub fn audio_object_get_property_data<T>(
    id: AudioObjectID,
    address: &AudioObjectPropertyAddress,
    size: *mut usize,
    data: *mut T,
) -> OSStatus {
    unsafe {
        AudioObjectGetPropertyData(
            id,
            address,
            0,
            ptr::null(),
            size as *mut UInt32,
            data as *mut c_void,
        )
    }
}

pub fn audio_object_get_property_data_with_qualifier<T, Q>(
    id: AudioObjectID,
    address: &AudioObjectPropertyAddress,
    qualifier_size: usize,
    qualifier_data: *const Q,
    size: *mut usize,
    data: *mut T,
) -> OSStatus {
    unsafe {
        AudioObjectGetPropertyData(
            id,
            address,
            qualifier_size as UInt32,
            qualifier_data as *const c_void,
            size as *mut UInt32,
            data as *mut c_void,
        )
    }
}

pub fn audio_object_get_property_data_size(
    id: AudioObjectID,
    address: &AudioObjectPropertyAddress,
    size: *mut usize,
) -> OSStatus {
    unsafe { AudioObjectGetPropertyDataSize(id, address, 0, ptr::null(), size as *mut UInt32) }
}

pub fn audio_object_get_property_data_size_with_qualifier<Q>(
    id: AudioObjectID,
    address: &AudioObjectPropertyAddress,
    qualifier_size: usize,
    qualifier_data: *const Q,
    size: *mut usize,
) -> OSStatus {
    unsafe {
        AudioObjectGetPropertyDataSize(
            id,
            address,
            qualifier_size as UInt32,
            qualifier_data as *const c_void,
            size as *mut UInt32,
        )
    }
}

pub fn has_property_scoped(obj: AudioObjectID, selector: u32, scope: u32) -> bool {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMaster,
    };
    audio_object_has_property(obj, &address)
}

pub fn get_property_scoped<T: Default>(
    obj: AudioObjectID,
    selector: u32,
    scope: u32,
) -> Result<T, OSStatus> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMaster,
    };
    let mut value: T = T::default();
    let mut size = mem::size_of_val(&value);
    let status = audio_object_get_property_data(obj, &address, &mut size, &mut value);
    match status {
        0 => Ok(value),
        e => Err(e),
    }
}

pub fn get_property<T: Default>(obj: AudioObjectID, selector: u32) -> Result<T, OSStatus> {
    get_property_scoped(obj, selector, kAudioObjectPropertyScopeGlobal)
}

pub fn get_list_property_scoped<T: Clone + Default>(
    obj: AudioObjectID,
    selector: u32,
    scope: u32,
) -> Result<Vec<T>, OSStatus> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMaster,
    };
    let mut size = 0;
    let status = audio_object_get_property_data_size(obj, &address, &mut size);
    if status != 0 {
        return Err(status);
    }
    let mut objects: Vec<T> = vec![T::default(); size / mem::size_of::<T>()];
    let status = audio_object_get_property_data(obj, &address, &mut size, objects.as_mut_ptr());
    match status {
        0 => Ok(objects),
        e => Err(e),
    }
}

pub fn get_list_property<T: Clone + Default>(
    obj: AudioObjectID,
    selector: u32,
) -> Result<Vec<T>, OSStatus> {
    get_list_property_scoped(obj, selector, kAudioObjectPropertyScopeGlobal)
}

pub fn get_string_property(obj: AudioObjectID, selector: u32) -> Result<String, OSStatus> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMaster,
    };
    let mut str: CFStringRef = ptr::null();
    let mut size = mem::size_of_val(&str);
    let status = audio_object_get_property_data(obj, &address, &mut size, &mut str);
    match status {
        0 => Ok(StringRef::new(str).into_string()),
        e => Err(e),
    }
}

fn class_to_str(obj: AudioClassID) -> Option<&'static str> {
    #[allow(non_upper_case_globals, non_snake_case)]
    match obj {
        // AudioHardware.h
        kAudioSystemObjectClassID => Some("AudioSystemObject"),
        kAudioAggregateDeviceClassID => Some("AudioAggregateDevice"),
        kAudioSubDeviceClassID => Some("AudioSubDevice"),
        kAudioSubTapClassID => Some("AudioSubTap"),
        kAudioProcessClassID => Some("AudioProcess"),
        kAudioTapClassID => Some("AudioTap"),
        // AudioHardwareBase.h
        kAudioObjectClassIDWildcard => Some("AudioObjectClassIDWildcard"),
        kAudioObjectClassID => Some("AudioObject"),
        kAudioPlugInClassID => Some("AudioPlugIn"),
        kAudioTransportManagerClassID => Some("AudioTransportManager"),
        kAudioBoxClassID => Some("AudioBox"),
        kAudioDeviceClassID => Some("AudioDevice"),
        kAudioClockDeviceClassID => Some("AudioClockDevice"),
        kAudioEndPointDeviceClassID => Some("AudioEndPointDevice"),
        kAudioEndPointClassID => Some("AudioEndPoint"),
        kAudioStreamClassID => Some("AudioStream"),
        kAudioControlClassID => Some("AudioControl"),
        kAudioSliderControlClassID => Some("AudioSliderControl"),
        kAudioLevelControlClassID => Some("AudioLevelControl"),
        kAudioVolumeControlClassID => Some("AudioVolumeControl"),
        kAudioLFEVolumeControlClassID => Some("AudioLFEVolumeControl"),
        kAudioBooleanControlClassID => Some("AudioBooleanControl"),
        kAudioMuteControlClassID => Some("AudioMuteControl"),
        kAudioSoloControlClassID => Some("AudioSoloControl"),
        kAudioJackControlClassID => Some("AudioJackControl"),
        kAudioLFEMuteControlClassID => Some("AudioLFEMuteControl"),
        kAudioPhantomPowerControlClassID => Some("AudioPhantomPowerControl"),
        kAudioPhaseInvertControlClassID => Some("AudioPhaseInvertControl"),
        kAudioClipLightControlClassID => Some("AudioClipLightControl"),
        kAudioTalkbackControlClassID => Some("AudioTalkbackControl"),
        kAudioListenbackControlClassID => Some("AudioListenbackControl"),
        kAudioSelectorControlClassID => Some("AudioSelectorControl"),
        kAudioDataSourceControlClassID => Some("AudioDataSourceControl"),
        kAudioDataDestinationControlClassID => Some("AudioDataDestinationControl"),
        kAudioClockSourceControlClassID => Some("AudioClockSourceControl"),
        kAudioLineLevelControlClassID => Some("AudioLineLevelControl"),
        kAudioHighPassFilterControlClassID => Some("AudioHighPassFilterControl"),
        kAudioStereoPanControlClassID => Some("AudioStereoPanControl"),
        // AudioHardwareDeprecated.h
        kAudioISubOwnerControlClassID => Some("AudioISubOwnerControl"),
        kAudioBootChimeVolumeControlClassID => Some("AudioBootChimeVolumeControl"),
        _ => None,
    }
}

fn add_class_id(identifier: &str, id: Result<AudioClassID, OSStatus>) {
    if id.is_err() {
        add_leaf!("{}: {:?}", identifier, id);
        return;
    }
    let id = id.unwrap();
    if let Some(s) = class_to_str(id) {
        add_leaf!("{} (Known): {:?}", identifier, s);
        return;
    }
    add_leaf!("{} (FourCC): {:?}", identifier, CString::new(id.to_be_bytes().to_vec()).unwrap());
}

macro_rules! prop {
    (@print $name: expr, $value: expr) => {
        add_leaf!("{}: {:?}", $name, $value);
    };
    (@print @pretty $pretty: expr, $name: expr, $value: expr) => {
        add_leaf!("{}: {:#?}", $name, $value);
    };
    (@internal $fun: expr $(, @pretty $pretty: expr)? $(, @prefix $prefix: expr)?, ($obj: expr, $prop: expr $(, $args: expr),*), $opt: expr $(, $map: expr)?) => {
        let r = $fun($obj, $prop, $($args),*)$(.map($map))?;
        let name = stringify!($prop).split("Property").last().unwrap();
        $(let name = format!("{} {}", stringify!($prefix), name);)?
        if $opt.contains(TraversalOptions::DEBUG) {
            prop!(@print $(@pretty $pretty,)? name, r);
        } else if let Ok(p) = r {
            prop!(@print $(@pretty $pretty,)? name, p);
        }
    };
    (bool, Input, $prop: expr, $obj: expr, $opt: expr) => {
        prop!(@internal get_property_scoped::<u32>, @prefix Input, ($obj, $prop, kAudioObjectPropertyScopeInput), $opt, |p| p != 0);
    };
    (bool, Output, $prop: expr, $obj: expr, $opt: expr) => {
        prop!(@internal get_property_scoped::<u32>, @prefix Output, ($obj, $prop, kAudioObjectPropertyScopeOutput), $opt, |p| p != 0);
    };
    (bool, $prop: expr, $obj: expr, $opt: expr) => {
        prop!(@internal get_property::<u32>, ($obj, $prop), $opt, |p| p != 0);
    };
    (string, $prop: expr, $obj: expr, $opt: expr) => {
        prop!(@internal get_string_property, ($obj, $prop), $opt);
    };
    (Vec<$t: ty>, Pretty, Input, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_list_property_scoped::<$t>, @pretty "", @prefix Input, ($obj, $prop, kAudioObjectPropertyScopeInput), $opt$(, $map)?);
    };
    (Vec<$t: ty>, Pretty, Output, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_list_property_scoped::<$t>, @pretty "", @prefix Output, ($obj, $prop, kAudioObjectPropertyScopeOutput), $opt$(, $map)?);
    };
    (Vec<$t: ty>, Input, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_list_property_scoped::<$t>, @prefix Input, ($obj, $prop, kAudioObjectPropertyScopeInput), $opt$(, $map)?);
    };
    (Vec<$t: ty>, Output, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_list_property_scoped::<$t>, @prefix Output, ($obj, $prop, kAudioObjectPropertyScopeOutput), $opt$(, $map)?);
    };
    (Vec<$t: ty>, Pretty, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_list_property::<$t>, @pretty "", ($obj, $prop), $opt$(, $map)?);
    };
    (Vec<$t: ty>, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_list_property::<$t>, ($obj, $prop), $opt$(, $map)?);
    };
    ($t: ty, Pretty, Input, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_property_scoped::<$t>, @pretty "", @prefix Input, ($obj, $prop, kAudioObjectPropertyScopeInput), $opt$(, $map)?);
    };
    ($t: ty, Pretty, Output, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_property_scoped::<$t>, @pretty "", @prefix Output, ($obj, $prop, kAudioObjectPropertyScopeOutput), $opt$(, $map)?);
    };
    ($t: ty, Input, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_property_scoped::<$t>, @prefix Input, ($obj, $prop, kAudioObjectPropertyScopeInput), $opt$(, $map)?);
    };
    ($t: ty, Output, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_property_scoped::<$t>, @prefix Output, ($obj, $prop, kAudioObjectPropertyScopeOutput), $opt$(, $map)?);
    };
    ($t: ty, Pretty, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_property::<$t>, @pretty "", ($obj, $prop), $opt$(, $map)?);
    };
    ($t: ty, $prop: expr, $obj: expr, $opt: expr $(, $map: expr)?) => {
        prop!(@internal get_property::<$t>, ($obj, $prop), $opt$(, $map)?);
    };
}

fn cfarray_get_count(r: usize) -> usize {
    let arr = r as CFArrayRef;
    if arr.is_null() {
        return 0;
    }
    unsafe { CFArrayGetCount(arr) as usize }
}

fn traverse_aggregate_device(obj: AudioObjectID, opt: TraversalOptions) {
    prop!(usize, kAudioAggregateDevicePropertyTapList, obj, opt, cfarray_get_count);
    prop!(usize, kAudioAggregateDevicePropertySubTapList, obj, opt, cfarray_get_count);
}

fn transporttype_to_str(p: u32) -> &'static str {
    #[allow(non_upper_case_globals, non_snake_case)]
    match p {
        kAudioDeviceTransportTypeUnknown => "Unknown",
        kAudioDeviceTransportTypeBuiltIn => "BuiltIn",
        kAudioDeviceTransportTypeAggregate => "Aggregate",
        kAudioDeviceTransportTypeVirtual => "Virtual",
        kAudioDeviceTransportTypePCI => "PCI",
        kAudioDeviceTransportTypeUSB => "USB",
        kAudioDeviceTransportTypeFireWire => "FireWire",
        kAudioDeviceTransportTypeBluetooth => "Bluetooth",
        kAudioDeviceTransportTypeBluetoothLE => "BluetoothLE",
        kAudioDeviceTransportTypeHDMI => "HDMI",
        kAudioDeviceTransportTypeDisplayPort => "DisplayPort",
        kAudioDeviceTransportTypeAirPlay => "AirPlay",
        kAudioDeviceTransportTypeAVB => "AVB",
        kAudioDeviceTransportTypeThunderbolt => "Thunderbolt",
        kAudioDeviceTransportTypeContinuityCaptureWired => "ContinuityCaptureWired",
        kAudioDeviceTransportTypeContinuityCaptureWireless => "ContinuityCaptureWireless",
        kAudioDeviceTransportTypeContinuityCapture => "ContinuityCapture",
        _ => "Unexpected TransportType",
    }
}

#[derive(Debug, Clone)]
#[allow(non_camel_case_types, non_snake_case, dead_code)]
struct AudioChannelLayout_ExpandedChannels {
    mChannelLayoutTag: AudioChannelLayoutTag,
    mChannelBitmap: AudioChannelBitmap,
    mNumberChannelDescriptions: UInt32,
    mChannelDescriptions: Vec<AudioChannelDescription>,
}

impl AudioChannelLayout_ExpandedChannels {
    fn new(l: AudioChannelLayout, cs: Vec<AudioChannelDescription>) -> Self {
        Self {
            mChannelLayoutTag: l.mChannelLayoutTag,
            mChannelBitmap: l.mChannelBitmap,
            mNumberChannelDescriptions: l.mNumberChannelDescriptions,
            mChannelDescriptions: cs,
        }
    }
}

fn expand_channel_layout(data: Vec<u8>) -> AudioChannelLayout_ExpandedChannels {
    let acl_len = mem::size_of::<AudioChannelLayout>();
    let acd_len = mem::size_of::<AudioChannelDescription>();
    let acl_base_len = acl_len - acd_len;
    assert!(data.len() >= acl_base_len);
    let layout_ptr = data.as_ptr() as *const AudioChannelLayout;
    let num_channels = (data.len() - acl_base_len) / acd_len;
    debug_assert_eq!(unsafe { *layout_ptr }.mNumberChannelDescriptions as usize, num_channels);
    let cs = unsafe {
        std::slice::from_raw_parts(
            (data.as_ptr().wrapping_add(acl_base_len)) as *const AudioChannelDescription,
            num_channels,
        )
    };
    AudioChannelLayout_ExpandedChannels::new(unsafe { *layout_ptr }, cs.into())
}

fn traverse_device(obj: AudioObjectID, opt: TraversalOptions) {
    prop!(string, kAudioDevicePropertyConfigurationApplication, obj, opt);
    prop!(string, kAudioDevicePropertyDeviceUID, obj, opt);
    prop!(string, kAudioDevicePropertyModelUID, obj, opt);
    prop!(u32, kAudioDevicePropertyTransportType, obj, opt, transporttype_to_str);
    prop!(pid_t, kAudioDevicePropertyHogMode, obj, opt);
    prop!(Vec<AudioDeviceID>, kAudioDevicePropertyRelatedDevices, obj, opt);
    prop!(Vec<AudioDeviceID>, kAudioAggregateDevicePropertyActiveSubDeviceList, obj, opt);
    prop!(u32, kAudioDevicePropertyClockDomain, obj, opt);
    prop!(string, kAudioDevicePropertyClockDevice, obj, opt);
    prop!(bool, kAudioDevicePropertyDeviceIsAlive, obj, opt);
    prop!(bool, kAudioDevicePropertyDeviceIsRunningSomewhere, obj, opt);
    prop!(bool, kAudioDevicePropertyDeviceIsRunning, obj, opt);
    prop!(bool, Input, kAudioDevicePropertyDeviceCanBeDefaultDevice, obj, opt);
    prop!(bool, Output, kAudioDevicePropertyDeviceCanBeDefaultDevice, obj, opt);
    prop!(bool, Output, kAudioDevicePropertyDeviceCanBeDefaultSystemDevice, obj, opt);
    prop!(u32, Input, kAudioDevicePropertyLatency, obj, opt);
    prop!(u32, Output, kAudioDevicePropertyLatency, obj, opt);
    prop!(Vec<AudioStreamID>, Input, kAudioDevicePropertyStreams, obj, opt);
    prop!(Vec<AudioStreamID>, Output, kAudioDevicePropertyStreams, obj, opt);
    prop!(Vec<AudioObjectID>, kAudioObjectPropertyControlList, obj, opt);
    prop!(u32, Input, kAudioDevicePropertySafetyOffset, obj, opt);
    prop!(u32, Output, kAudioDevicePropertySafetyOffset, obj, opt);
    prop!(f64, kAudioDevicePropertyActualSampleRate, obj, opt);
    prop!(f64, kAudioDevicePropertyNominalSampleRate, obj, opt);
    if opt.contains(TraversalOptions::INCLUDE_FORMATS) {
        prop!(
            Vec<AudioValueRange>,
            Pretty,
            kAudioDevicePropertyAvailableNominalSampleRates,
            obj,
            opt
        );
    }
    prop!(u32, kAudioDevicePropertyBufferFrameSize, obj, opt);
    prop!(AudioValueRange, kAudioDevicePropertyBufferFrameSizeRange, obj, opt);
    prop!(u32, kAudioDevicePropertyUsesVariableBufferFrameSizes, obj, opt);
    prop!(Vec<u32>, Input, kAudioDevicePropertyPreferredChannelsForStereo, obj, opt);
    prop!(Vec<u32>, Output, kAudioDevicePropertyPreferredChannelsForStereo, obj, opt);
    if opt.contains(TraversalOptions::INCLUDE_CHANNELS) {
        prop!(
            Vec<u8>,
            Pretty,
            Output,
            kAudioDevicePropertyPreferredChannelLayout,
            obj,
            opt,
            expand_channel_layout
        );
    }
    prop!(f32, kAudioDevicePropertyIOCycleUsage, obj, opt);
    prop!(u32, Input, kAudioDevicePropertyProcessMute, obj, opt);
}

fn terminaltype_to_str(t: u32) -> String {
    #[allow(non_upper_case_globals, non_snake_case)]
    match t {
        kAudioStreamTerminalTypeUnknown => "Unknown".to_string(),
        kAudioStreamTerminalTypeLine => "Line".to_string(),
        kAudioStreamTerminalTypeDigitalAudioInterface => "DigitalAudioInterface".to_string(),
        kAudioStreamTerminalTypeSpeaker => "Speaker".to_string(),
        kAudioStreamTerminalTypeHeadphones => "Headphones".to_string(),
        kAudioStreamTerminalTypeLFESpeaker => "LFESpeaker".to_string(),
        kAudioStreamTerminalTypeReceiverSpeaker => "ReceiverSpeaker".to_string(),
        kAudioStreamTerminalTypeMicrophone => "Microphone".to_string(),
        kAudioStreamTerminalTypeHeadsetMicrophone => "HeadsetMicrophone".to_string(),
        kAudioStreamTerminalTypeReceiverMicrophone => "ReceiverMicrophone".to_string(),
        kAudioStreamTerminalTypeTTY => "TTY".to_string(),
        kAudioStreamTerminalTypeHDMI => "HDMI".to_string(),
        kAudioStreamTerminalTypeDisplayPort => "DisplayPort".to_string(),
        t => format!("{:#06X}", t),
    }
}

fn traverse_stream(obj: AudioStreamID, opt: TraversalOptions) {
    prop!(bool, kAudioStreamPropertyIsActive, obj, opt);
    prop!(u32, kAudioStreamPropertyDirection, obj, opt, |p| if p == 1 {
        "Input"
    } else {
        "Output"
    });
    prop!(u32, kAudioStreamPropertyTerminalType, obj, opt, terminaltype_to_str);
    prop!(u32, kAudioStreamPropertyStartingChannel, obj, opt);
    prop!(u32, Input, kAudioStreamPropertyLatency, obj, opt);
    prop!(u32, Output, kAudioStreamPropertyLatency, obj, opt);
    prop!(AudioStreamBasicDescription, Pretty, kAudioStreamPropertyVirtualFormat, obj, opt);
    if opt.contains(TraversalOptions::INCLUDE_FORMATS) {
        prop!(
            Vec<AudioStreamRangedDescription>,
            Pretty,
            kAudioStreamPropertyAvailableVirtualFormats,
            obj,
            opt
        );
    }
    prop!(AudioStreamBasicDescription, Pretty, kAudioStreamPropertyPhysicalFormat, obj, opt);
    if opt.contains(TraversalOptions::INCLUDE_FORMATS) {
        prop!(
            Vec<AudioStreamRangedDescription>,
            Pretty,
            kAudioStreamPropertyAvailablePhysicalFormats,
            obj,
            opt
        );
    }
}

fn traverse_process(obj: AudioObjectID, opt: TraversalOptions) {
    prop!(pid_t, kAudioProcessPropertyPID, obj, opt);
    prop!(string, kAudioProcessPropertyBundleID, obj, opt);
    prop!(Vec<AudioObjectID>, Input, kAudioProcessPropertyDevices, obj, opt);
    prop!(Vec<AudioObjectID>, Output, kAudioProcessPropertyDevices, obj, opt);
    prop!(bool, kAudioProcessPropertyIsRunning, obj, opt);
    prop!(bool, kAudioProcessPropertyIsRunningInput, obj, opt);
    prop!(bool, kAudioProcessPropertyIsRunningOutput, obj, opt);
}

fn traverse_hw(obj: AudioObjectID, opt: TraversalOptions) {
    prop!(Vec<AudioObjectID>, kAudioHardwarePropertyDevices, obj, opt);
    prop!(AudioObjectID, kAudioHardwarePropertyDefaultInputDevice, obj, opt);
    prop!(AudioObjectID, kAudioHardwarePropertyDefaultOutputDevice, obj, opt);
    prop!(AudioObjectID, kAudioHardwarePropertyDefaultSystemOutputDevice, obj, opt);
    prop!(bool, kAudioHardwarePropertyMixStereoToMono, obj, opt);
    prop!(Vec<AudioObjectID>, kAudioHardwarePropertyPlugInList, obj, opt);
    prop!(Vec<AudioObjectID>, kAudioHardwarePropertyTransportManagerList, obj, opt);
    prop!(Vec<AudioObjectID>, kAudioHardwarePropertyBoxList, obj, opt);
    prop!(Vec<AudioObjectID>, kAudioHardwarePropertyClockDeviceList, obj, opt);
    prop!(bool, kAudioHardwarePropertyProcessIsMain, obj, opt);
    prop!(bool, kAudioHardwarePropertyIsInitingOrExiting, obj, opt);
    prop!(bool, kAudioHardwarePropertyProcessInputMute, obj, opt);
    prop!(bool, kAudioHardwarePropertyProcessIsAudible, obj, opt);
    prop!(bool, kAudioHardwarePropertySleepingIsAllowed, obj, opt);
    prop!(bool, kAudioHardwarePropertyUnloadingIsAllowed, obj, opt);
    prop!(bool, kAudioHardwarePropertyHogModeIsAllowed, obj, opt);
    prop!(bool, kAudioHardwarePropertyUserSessionIsActiveOrHeadless, obj, opt);
    prop!(AudioHardwarePowerHint, kAudioHardwarePropertyPowerHint, obj, opt);
    prop!(Vec<AudioObjectID>, kAudioHardwarePropertyProcessObjectList, obj, opt);
    prop!(Vec<AudioObjectID>, kAudioHardwarePropertyTapList, obj, opt);
}

fn traverse_obj(obj: AudioObjectID, opt: TraversalOptions) {
    let owned_objects = get_list_property::<AudioObjectID>(obj, kAudioObjectPropertyOwnedObjects);
    let base_class_id = get_property::<AudioClassID>(obj, kAudioObjectPropertyBaseClass);
    let class_id = get_property::<AudioClassID>(obj, kAudioObjectPropertyClass);
    if !opt.contains(TraversalOptions::INCLUDE_CONTROLS)
        && base_class_id.is_ok_and(|id| {
            [
                kAudioControlClassID,
                kAudioSliderControlClassID,
                kAudioLevelControlClassID,
                kAudioBooleanControlClassID,
                kAudioSelectorControlClassID,
                kAudioStereoPanControlClassID,
            ]
            .contains(&id)
        })
    {
        return;
    }
    if !opt.contains(TraversalOptions::INCLUDE_BOXES)
        && class_id.is_ok_and(|id| id == kAudioBoxClassID)
    {
        return;
    }
    if !opt.contains(TraversalOptions::INCLUDE_CLOCKS)
        && class_id.is_ok_and(|id| id == kAudioClockDeviceClassID)
    {
        return;
    }
    if !opt.contains(TraversalOptions::INCLUDE_STREAMS)
        && class_id.is_ok_and(|id| id == kAudioStreamClassID)
    {
        return;
    }
    if !opt.contains(TraversalOptions::INCLUDE_PLUGINS)
        && class_id.is_ok_and(|id| id == kAudioPlugInClassID)
    {
        return;
    }
    if !opt.contains(TraversalOptions::INCLUDE_PROCESSES)
        && class_id.is_ok_and(|id| id == kAudioProcessClassID)
    {
        return;
    }
    add_branch!("AudioObjectID: {}", obj);
    add_class_id("BaseClass", base_class_id);
    add_class_id("Class", class_id);
    prop!(bool, kAudioObjectPropertyOwner, obj, opt);
    prop!(string, kAudioObjectPropertyName, obj, opt);
    prop!(string, kAudioObjectPropertyModelName, obj, opt);
    prop!(string, kAudioObjectPropertyManufacturer, obj, opt);
    prop!(string, kAudioObjectPropertyElementName, obj, opt);
    prop!(string, kAudioObjectPropertyElementNumberName, obj, opt);
    prop!(string, kAudioDevicePropertyDeviceUID, obj, opt);
    #[allow(non_upper_case_globals, non_snake_case)]
    match class_id {
        Ok(kAudioSystemObjectClassID) => traverse_hw(obj, opt),
        Ok(kAudioAggregateDeviceClassID) => {
            traverse_aggregate_device(obj, opt);
            traverse_device(obj, opt);
        }
        Ok(kAudioSubDeviceClassID) | Ok(kAudioDeviceClassID) => traverse_device(obj, opt),
        Ok(kAudioStreamClassID) => traverse_stream(obj, opt),
        Ok(kAudioProcessClassID) => traverse_process(obj, opt),
        _ => {}
    }
    if let Ok(objects) = owned_objects {
        for obj in objects {
            traverse_obj(obj, opt);
        }
    }
}

pub fn traverse() {
    traverse_obj(kAudioObjectSystemObject, TraversalOptions::empty());
    default_tree().flush_print();
}

pub fn traverse_with_options(opt: TraversalOptions) {
    traverse_obj(kAudioObjectSystemObject, opt);
    default_tree().flush_print();
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct TraversalOptions: u16 {
        const INCLUDE_BOXES = 1 << 0;
        const INCLUDE_CLOCKS = 1 << 1;
        const INCLUDE_STREAMS = 1 << 2;
        const INCLUDE_FORMATS = 1 << 3;
        const INCLUDE_CHANNELS = 1 << 4;
        const INCLUDE_CONTROLS = 1 << 5;
        const INCLUDE_PLUGINS = 1 << 6;
        const INCLUDE_PROCESSES = 1 << 7;
        const DEBUG = 1 << 8;
    }
}
