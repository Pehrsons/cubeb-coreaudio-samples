use std::ffi::CString;
use std::mem;

use coreaudio_sys::*;
use debug_tree::{add_branch, add_leaf, default_tree};

use std::os::raw::c_void;
use std::ptr;

#[derive(Debug)]
pub struct StringRef(CFStringRef);
impl StringRef {
    pub fn new(string_ref: CFStringRef) -> Self {
        assert!(!string_ref.is_null());
        Self(string_ref)
    }

    pub fn into_string(self) -> String {
        self.to_string()
    }

    pub fn to_cstring(&self) -> CString {
        unsafe {
            // Assume that bytes doesn't contain `0` in the middle.
            CString::from_vec_unchecked(utf8_from_cfstringref(self.0))
        }
    }

    pub fn into_cstring(self) -> CString {
        self.to_cstring()
    }

    pub fn get_raw(&self) -> CFStringRef {
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

impl std::fmt::Display for StringRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

fn get_property<T: Default>(obj: AudioObjectID, selector: u32) -> Result<T, OSStatus> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
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

fn get_list_property_scoped<T: Clone + Default>(
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

fn get_list_property<T: Clone + Default>(
    obj: AudioObjectID,
    selector: u32,
) -> Result<Vec<T>, OSStatus> {
    get_list_property_scoped(obj, selector, kAudioObjectPropertyScopeGlobal)
}

fn get_string_property(obj: AudioObjectID, selector: u32) -> Result<String, OSStatus> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMaster,
    };
    let mut str: CFStringRef = ptr::null();
    let mut size = mem::size_of_val(&str);
    let status = audio_object_get_property_data(obj, &address, &mut size, &mut str);
    match status {
        0 => Ok(StringRef::new(str).to_string()),
        e => Err(e),
    }
}

fn class_to_str(obj: AudioClassID) -> Option<&'static str> {
    #[allow(non_upper_case_globals)]
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
    add_leaf!(
        "{} (FourCC): {:?}",
        identifier,
        CString::new(id.to_be_bytes().to_vec()).unwrap()
    );
}

pub fn traverse_aggregate_device(obj: AudioObjectID) {
    if let Ok(arr) = get_property::<usize>(obj, kAudioAggregateDevicePropertyTapList) {
        let arr = arr as CFArrayRef;
        if !arr.is_null() {
            add_leaf!("TapList count: {}", unsafe { CFArrayGetCount(arr) });
        }
    }
    if let Ok(arr) = get_property::<usize>(obj, kAudioAggregateDevicePropertySubTapList) {
        let arr = arr as CFArrayRef;
        if !arr.is_null() {
            add_leaf!("SubTapList count: {}", unsafe { CFArrayGetCount(arr) });
        }
    }
}

pub fn traverse_device(obj: AudioObjectID, opt: TraversalOptions) {
    if let Ok(s) = get_string_property(obj, kAudioDevicePropertyConfigurationApplication) {
        add_leaf!("ConfigurationApplication: {}", s);
    }
    if let Ok(s) = get_string_property(obj, kAudioDevicePropertyDeviceUID) {
        add_leaf!("DeviceUID: {}", s);
    }
    if let Ok(s) = get_string_property(obj, kAudioDevicePropertyModelUID) {
        add_leaf!("ModelUID: {}", s);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyTransportType) {
        #[allow(non_upper_case_globals)]
        let s = match p {
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
        };
        add_leaf!("TransportType: {}", s);
    }
    if let Ok(p) = get_property::<pid_t>(obj, kAudioDevicePropertyHogMode) {
        add_leaf!("HogMode: {}", p);
    }
    if let Ok(objects) = get_list_property::<AudioDeviceID>(obj, kAudioDevicePropertyRelatedDevices)
    {
        add_leaf!("RelatedDevices: {:?}", objects);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyClockDomain) {
        add_leaf!("ClockDomain: {}", p);
    }
    if let Ok(p) = get_string_property(obj, kAudioDevicePropertyClockDevice) {
        add_leaf!("ClockDevice: {:?}", p);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyDeviceIsAlive) {
        add_leaf!("DeviceIsAlive: {}", p == 1)
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyDeviceIsRunningSomewhere) {
        add_leaf!("DeviceIsRunningSomewhere: {}", p == 1);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyDeviceIsRunning) {
        add_leaf!("DeviceIsRunning: {}", p == 1)
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyDeviceCanBeDefaultDevice) {
        add_leaf!("DeviceCanBeDefaultDevice: {}", p == 1)
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyDeviceCanBeDefaultSystemDevice) {
        add_leaf!("DeviceCanBeDefaultSystemDevice: {}", p == 1)
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyLatency) {
        add_leaf!("Latency: {}", p)
    }
    if let Ok(objects) = get_list_property_scoped::<AudioStreamID>(
        obj,
        kAudioDevicePropertyStreams,
        kAudioObjectPropertyScopeInput,
    ) {
        add_leaf!("Input Streams: {:?}", objects);
    }
    if let Ok(objects) = get_list_property_scoped::<AudioStreamID>(
        obj,
        kAudioDevicePropertyStreams,
        kAudioObjectPropertyScopeOutput,
    ) {
        add_leaf!("Output Streams: {:?}", objects);
    }
    if let Ok(objects) = get_list_property::<AudioObjectID>(obj, kAudioObjectPropertyControlList) {
        add_leaf!("Controls: {:?}", objects);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertySafetyOffset) {
        add_leaf!("SafetyOffset: {}", p)
    }
    if let Ok(p) = get_property::<f64>(obj, kAudioDevicePropertyActualSampleRate) {
        add_leaf!("ActualSampleRate: {}", p);
    }
    if let Ok(p) = get_property::<f64>(obj, kAudioDevicePropertyNominalSampleRate) {
        add_leaf!("NominalSampleRate: {}", p)
    }
    if opt.contains(TraversalOptions::INCLUDE_FORMATS) {
        if let Ok(objects) = get_list_property::<AudioValueRange>(
            obj,
            kAudioDevicePropertyAvailableNominalSampleRates,
        ) {
            add_leaf!("AvailableNominalSampleRates: {:#?}", objects);
        }
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyBufferFrameSize) {
        add_leaf!("BufferFrameSize: {}", p);
    }
    if let Ok(p) = get_property::<AudioValueRange>(obj, kAudioDevicePropertyBufferFrameSizeRange) {
        add_leaf!("BufferFrameSizeRange: {:?}", p);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyUsesVariableBufferFrameSizes) {
        add_leaf!("UsesVariableBufferFrameSizes: {:?}", p == 1);
    }
    if let Ok(objects) =
        get_list_property::<u32>(obj, kAudioDevicePropertyPreferredChannelsForStereo)
    {
        add_leaf!("PreferredChannelsForStereo: {:?}", objects);
    }
    if let Ok(p) =
        get_property::<AudioChannelLayout>(obj, kAudioDevicePropertyPreferredChannelLayout)
    {
        add_leaf!("PreferredChannelLayout: {:?}", p);
    }
    if let Ok(p) = get_property::<f32>(obj, kAudioDevicePropertyIOCycleUsage) {
        add_leaf!("IOCycleUsage: {}", p);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioDevicePropertyProcessMute) {
        add_leaf!("ProcessMute: {}", p != 0);
    }
}

pub fn terminaltype_to_str(t: u32) -> String {
    #[allow(non_upper_case_globals)]
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

pub fn traverse_stream(obj: AudioStreamID, opt: TraversalOptions) {
    if let Ok(p) = get_property::<u32>(obj, kAudioStreamPropertyIsActive) {
        add_leaf!("IsActive: {}", p == 1);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioStreamPropertyDirection) {
        add_leaf!("Direction: {}", if p == 1 { "Input" } else { "Output" });
    }
    if let Ok(p) =
        get_property::<u32>(obj, kAudioStreamPropertyTerminalType).map(|p| terminaltype_to_str(p))
    {
        add_leaf!("TerminalType: {}", p);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioStreamPropertyStartingChannel) {
        add_leaf!("StartingChannel: {}", p);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioStreamPropertyLatency) {
        add_leaf!("Latency: {}", p);
    }
    if let Ok(p) =
        get_property::<AudioStreamBasicDescription>(obj, kAudioStreamPropertyVirtualFormat)
    {
        add_leaf!("VirtualFormat: {:#?}", p);
    }
    if opt.contains(TraversalOptions::INCLUDE_FORMATS) {
        if let Ok(p) = get_list_property::<AudioStreamRangedDescription>(
            obj,
            kAudioStreamPropertyAvailableVirtualFormats,
        ) {
            add_leaf!("AvailableVirtualFormats: {:#?}", p);
        }
    }
    if let Ok(p) =
        get_property::<AudioStreamBasicDescription>(obj, kAudioStreamPropertyPhysicalFormat)
    {
        add_leaf!("PhysicalFormat: {:#?}", p);
    }
    if opt.contains(TraversalOptions::INCLUDE_FORMATS) {
        if let Ok(p) = get_list_property::<AudioStreamRangedDescription>(
            obj,
            kAudioStreamPropertyAvailablePhysicalFormats,
        ) {
            add_leaf!("AvailablePhysicalFormats: {:#?}", p);
        }
    }
}

pub fn traverse_process(obj: AudioObjectID) {
    if let Ok(p) = get_property::<pid_t>(obj, kAudioProcessPropertyPID) {
        add_leaf!("PID: {}", p);
    }
    if let Ok(p) = get_string_property(obj, kAudioProcessPropertyBundleID) {
        add_leaf!("BundleID: {}", p);
    }
    if let Ok(p) = get_list_property_scoped::<AudioObjectID>(
        obj,
        kAudioProcessPropertyDevices,
        kAudioObjectPropertyScopeInput,
    ) {
        add_leaf!("Input Devices: {:?}", p);
    }
    if let Ok(p) = get_list_property_scoped::<AudioObjectID>(
        obj,
        kAudioProcessPropertyDevices,
        kAudioObjectPropertyScopeOutput,
    ) {
        add_leaf!("Output Devices: {:?}", p);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioProcessPropertyIsRunning) {
        add_leaf!("IsRunning: {:?}", p == 1);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioProcessPropertyIsRunningInput) {
        add_leaf!("IsRunningInput: {:?}", p == 1);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioProcessPropertyIsRunningOutput) {
        add_leaf!("IsRunningOutput: {:?}", p == 1);
    }
}

pub fn traverse_hw(obj: AudioObjectID) {
    if let Ok(p) = get_list_property::<AudioObjectID>(obj, kAudioHardwarePropertyDevices) {
        add_leaf!("Devices: {:?}", p);
    }
    if let Ok(p) = get_property::<AudioObjectID>(obj, kAudioHardwarePropertyDefaultInputDevice) {
        add_leaf!("DefaultInputDevice: {}", p);
    }
    if let Ok(p) = get_property::<AudioObjectID>(obj, kAudioHardwarePropertyDefaultOutputDevice) {
        add_leaf!("DefaultOutputDevice: {}", p);
    }
    if let Ok(p) =
        get_property::<AudioObjectID>(obj, kAudioHardwarePropertyDefaultSystemOutputDevice)
    {
        add_leaf!("DefaultSystemOutputDevice: {}", p);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioHardwarePropertyMixStereoToMono) {
        add_leaf!("MixStereoToMono: {}", p != 0);
    }
    if let Ok(p) = get_list_property::<AudioObjectID>(obj, kAudioHardwarePropertyPlugInList) {
        add_leaf!("PlugInList: {:?}", p);
    }
    if let Ok(p) =
        get_list_property::<AudioObjectID>(obj, kAudioHardwarePropertyTransportManagerList)
    {
        add_leaf!("TransportManagerList: {:?}", p);
    }
    if let Ok(p) = get_list_property::<AudioObjectID>(obj, kAudioHardwarePropertyBoxList) {
        add_leaf!("BoxList: {:?}", p);
    }
    if let Ok(p) = get_list_property::<AudioObjectID>(obj, kAudioHardwarePropertyClockDeviceList) {
        add_leaf!("ClockDeviceList: {:?}", p);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioHardwarePropertyProcessIsMain) {
        add_leaf!("ProcessIsMain: {}", p == 1);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioHardwarePropertyIsInitingOrExiting) {
        add_leaf!("IsInitingOrExiting: {}", p != 0);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioHardwarePropertyProcessInputMute) {
        add_leaf!("ProcessInputMute: {}", p != 0);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioHardwarePropertyProcessIsAudible) {
        add_leaf!("ProcessIsAudible: {}", p != 0);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioHardwarePropertySleepingIsAllowed) {
        add_leaf!("SleepingIsAllowed: {}", p == 1);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioHardwarePropertyUnloadingIsAllowed) {
        add_leaf!("UnloadingIsAllowed: {}", p == 1);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioHardwarePropertyHogModeIsAllowed) {
        add_leaf!("HogModeIsAllowed: {}", p == 1);
    }
    if let Ok(p) = get_property::<u32>(obj, kAudioHardwarePropertyUserSessionIsActiveOrHeadless) {
        add_leaf!("UserSessionIsActiveOrHeadless: {}", p != 0);
    }
    if let Ok(p) = get_property::<AudioHardwarePowerHint>(obj, kAudioHardwarePropertyPowerHint) {
        add_leaf!("PowerHint: {}", p);
    }
    if let Ok(p) = get_list_property::<AudioObjectID>(obj, kAudioHardwarePropertyProcessObjectList)
    {
        add_leaf!("ProcessObjectList: {:?}", p);
    }
    if let Ok(objects) = get_list_property::<AudioObjectID>(obj, kAudioHardwarePropertyTapList) {
        add_leaf!("TapList: {:?}", objects);
    }
}

pub fn traverse_obj(obj: AudioObjectID, opt: TraversalOptions) {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioObjectPropertyOwnedObjects,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMaster,
    };
    let mut size: usize = 0;
    let status = audio_object_get_property_data_size(obj, &address, &mut size);
    if status != 0 {
        size = 0;
    }

    let mut objects: Vec<AudioObjectID> =
        vec![AudioObjectID::default(); size / mem::size_of::<AudioObjectID>()];
    let status = audio_object_get_property_data(obj, &address, &mut size, objects.as_mut_ptr());
    let objects = match status {
        0 => Ok(objects
            .into_iter()
            .take(size)
            .collect::<Vec<AudioObjectID>>()),
        e => Err(e),
    };

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
    if let Ok(p) = get_property::<AudioObjectID>(obj, kAudioObjectPropertyOwner) {
        add_leaf!("Owner: {:?}", p);
    }
    if let Ok(p) = get_string_property(obj, kAudioObjectPropertyName) {
        add_leaf!("Name: {:?}", p);
    }
    if let Ok(p) = get_string_property(obj, kAudioObjectPropertyModelName) {
        add_leaf!("Model Name: {:?}", p);
    }
    if let Ok(p) = get_string_property(obj, kAudioObjectPropertyManufacturer) {
        add_leaf!("Manufacturer: {:?}", p);
    }
    if let Ok(p) = get_string_property(obj, kAudioObjectPropertyElementName) {
        add_leaf!("Element Name: {:?}", p);
    }
    if let Ok(p) = get_string_property(obj, kAudioObjectPropertyElementNumberName) {
        add_leaf!("Element Number Name: {:?}", p);
    }
    if let Ok(p) = get_string_property(obj, kAudioDevicePropertyDeviceUID) {
        add_leaf!("Device UID: {:?}", p);
    }
    #[allow(non_upper_case_globals)]
    match class_id {
        Ok(kAudioSystemObjectClassID) => traverse_hw(obj),
        Ok(kAudioAggregateDeviceClassID) => traverse_aggregate_device(obj),
        Ok(kAudioDeviceClassID) => traverse_device(obj, opt),
        Ok(kAudioStreamClassID) => traverse_stream(obj, opt),
        Ok(kAudioProcessClassID) => traverse_process(obj),
        _ => {}
    }
    if let Ok(objects) = objects {
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
    pub struct TraversalOptions: u8 {
        const INCLUDE_BOXES = 1 << 0;
        const INCLUDE_CLOCKS = 1 << 1;
        const INCLUDE_STREAMS = 1 << 2;
        const INCLUDE_FORMATS = 1 << 3;
        const INCLUDE_CONTROLS = 1 << 4;
        const INCLUDE_PLUGINS = 1 << 5;
        const INCLUDE_PROCESSES = 1 << 6;
    }
}
