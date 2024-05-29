extern crate cubeb_coreaudio_samples;
use cubeb_backend::ffi::*;
use std::{
    ffi::{c_char, c_void},
    mem, ptr, thread,
    time::Duration,
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
    println!(
        "\n\
         ############################################################\n\
         ###                  ECHOING VPIO TEST                   ###\n\
         ############################################################\n\
         # This test creates a VPIO unit, starts it and waits 10    #\n\
         # seconds while while dumping the input to a file.         #\n\
         # It should cancel echo, but for some reason does not, on  #\n\
         # macOS 14.                                                #\n\
         # Play some audio on the machine to test while waiting!    #\n\
         ############################################################\n"
    );

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
            c"vpio-echoes".as_ptr(),   // Stream name.
            ptr::null_mut(),           // Default input device.
            &mut params,               // Input params.
            ptr::null_mut(),           // Default output device.
            ptr::null_mut(),           // Don't set up output.
            512,                       // Latency in frames.
            Some(noop_data_callback),  // Data callback.
            Some(noop_state_callback), // State Callback.
            ptr::null_mut(),           // User pointer.
        )
    });

    assert_eq!(CUBEB_OK, unsafe { cubeb_stream_start(stream) });

    thread::sleep(Duration::from_secs(10));

    assert_eq!(CUBEB_OK, unsafe { cubeb_stream_stop(stream) });
    unsafe { cubeb_stream_destroy(stream) };
    unsafe { cubeb_destroy(ctx) };

    assert_eq!(CUBEB_OK, unsafe { cubeb_set_log_callback(CUBEB_LOG_DISABLED, None) });
}
