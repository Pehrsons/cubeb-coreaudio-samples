extern crate cubeb_coreaudio_samples;
use cubeb_backend::ffi::*;
use std::{
    ffi::{c_char, c_void, CStr},
    mem, ptr,
};

extern "C" {
    fn print_log(msg: *const c_char, ...);
}

pub extern "C" fn noop_data_callback(
    stream: *mut cubeb_stream,
    _user_ptr: *mut c_void,
    _input_buffer: *const c_void,
    output_buffer: *mut c_void,
    nframes: i64,
) -> i64 {
    assert!(!stream.is_null());

    // Feed silence data to output buffer
    if !output_buffer.is_null() {
        const CHANNELS: usize = 1;
        let samples = nframes as usize * CHANNELS as usize;
        const SAMPLE_SIZE: usize = mem::size_of::<f32>();
        unsafe {
            ptr::write_bytes(output_buffer, 0, samples * SAMPLE_SIZE);
        }
    }

    nframes
}

pub extern "C" fn noop_state_callback(
    stream: *mut cubeb_stream,
    _user_ptr: *mut c_void,
    state: u32,
) {
    println!("Stream {:p}: STATE is now {}", stream, state);
}

fn main() {
    assert_eq!(CUBEB_OK, unsafe { cubeb_set_log_callback(CUBEB_LOG_NORMAL, Some(print_log)) });

    let mut ctx: *mut cubeb = ptr::null_mut();
    assert_eq!(CUBEB_OK, unsafe {
        cubeb_coreaudio::audiounit_rust_init(&mut ctx, ptr::null_mut())
    });
    assert_ne!(ctx, ptr::null_mut());

    let mut stream: *mut cubeb_stream = ptr::null_mut();
    let mut params = cubeb_stream_params {
        channels: 1,
        format: CUBEB_SAMPLE_FLOAT32NE,
        rate: 48000,
        layout: CUBEB_LAYOUT_MONO,
        prefs: CUBEB_STREAM_PREF_VOICE,
    };
    assert_eq!(CUBEB_OK, unsafe {
        cubeb_stream_init(
            ctx,
            &mut stream,
            c"vpio-enumeration".as_ptr(), // Stream name.
            ptr::null_mut(),              // Default input device.
            &mut params,                  // Input params.
            ptr::null_mut(),              // Default output device.
            ptr::null_mut(),              // Don't set up output.
            512,                          // Latency in frames.
            Some(noop_data_callback),     // Data callback.
            Some(noop_state_callback),    // State Callback.
            ptr::null_mut(),              // User pointer.
        )
    });

    let mut collection = cubeb_device_collection::default();
    assert_eq!(CUBEB_OK, unsafe {
        cubeb_enumerate_devices(
            ctx,
            CUBEB_DEVICE_TYPE_INPUT | CUBEB_DEVICE_TYPE_OUTPUT,
            &mut collection,
        )
    });
    let devices = ptr::slice_from_raw_parts(collection.device, collection.count);
    let devices = unsafe { &*devices };
    println!("Enumerated {} devices:", collection.count);
    for d in devices {
        let tup = (
            unsafe { CStr::from_ptr(d.friendly_name) },
            match d.device_type {
                CUBEB_DEVICE_TYPE_INPUT => "IN",
                CUBEB_DEVICE_TYPE_OUTPUT => "OUT",
                _ => "WHAT",
            },
            d.max_channels,
        );
        println!("{:?}", tup);
    }
    unsafe { cubeb_device_collection_destroy(ctx, &mut collection) };

    unsafe { cubeb_stream_destroy(stream) };
    unsafe { cubeb_destroy(ctx) };

    assert_eq!(CUBEB_OK, unsafe { cubeb_set_log_callback(CUBEB_LOG_DISABLED, None) });
}
