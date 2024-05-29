use clap::Parser;
use cubeb_backend::ffi::*;
use cubeb_coreaudio_samples::{traverse_with_options, TraversalOptions};
use std::{
    ffi::{c_char, c_void},
    io, mem, ptr,
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

#[derive(Parser, Debug)]
struct Args {
    /// Wait indefinitely, re-traversing on <Enter>.
    #[clap(long, short, action)]
    wait: bool,
    /// Include everything when traversing.
    #[clap(long, short = 'a', action)]
    include_all: bool,
    /// Include boxes when traversing.
    #[clap(long, short = 'b', action)]
    include_boxes: bool,
    /// Include clock devices when traversing.
    #[clap(long, short = 'k', action)]
    include_clocks: bool,
    /// Include streams when traversing.
    #[clap(long, short = 's', action)]
    include_streams: bool,
    /// Include available stream formats when traversing. Lists of available formats can be quite verbose.
    #[clap(long, short = 'f', action)]
    include_formats: bool,
    /// Include device channels when traversing. Lists of channel descriptions can be quite verbose.
    #[clap(long, short = 'n', action)]
    include_channels: bool,
    /// Include controls when traversing. They are often plenty and therefore quite verbose.
    #[clap(long, short = 'c', action)]
    include_controls: bool,
    /// Include plugins when traversing.
    #[clap(long, short = 'l', action)]
    include_plugins: bool,
    /// Include processes when traversing.
    #[clap(long, short = 'p', action)]
    include_processes: bool,
    /// Debug mode. Show all errors for getters that failed.
    #[clap(long, short = 'd', action)]
    debug: bool,
    /// Set up a VoiceProcessingIO unit before traversing, to see what streams and channels it adds.
    #[clap(long, short = 'v', action)]
    use_vpio: bool,
}

fn main() {
    let args = Args::parse();

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
    if args.use_vpio {
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
        assert_eq!(CUBEB_OK, unsafe { cubeb_stream_start(stream) });
    }

    let mut opt = TraversalOptions::empty();
    if args.include_boxes {
        opt.insert(TraversalOptions::INCLUDE_BOXES);
    }
    if args.include_clocks {
        opt.insert(TraversalOptions::INCLUDE_CLOCKS);
    }
    if args.include_streams {
        opt.insert(TraversalOptions::INCLUDE_STREAMS);
    }
    if args.include_formats {
        opt.insert(TraversalOptions::INCLUDE_FORMATS);
    }
    if args.include_channels {
        opt.insert(TraversalOptions::INCLUDE_CHANNELS);
    }
    if args.include_controls {
        opt.insert(TraversalOptions::INCLUDE_CONTROLS);
    }
    if args.include_plugins {
        opt.insert(TraversalOptions::INCLUDE_PLUGINS);
    }
    if args.include_processes {
        opt.insert(TraversalOptions::INCLUDE_PROCESSES);
    }
    if args.include_all {
        opt = TraversalOptions::all();
        opt.remove(TraversalOptions::DEBUG);
    }
    if args.debug {
        opt.insert(TraversalOptions::DEBUG);
    }

    if args.wait {
        loop {
            println!("Waiting... <ENTER> to traverse. q/quit/exit to quit.");
            let mut command = String::new();
            let _ = io::stdin().read_line(&mut command);
            assert_eq!(command.pop().unwrap(), '\n');
            if ["q", "quit", "exit"].contains(&command.as_str()) {
                break;
            }
            traverse_with_options(opt);
        }
    } else {
        traverse_with_options(opt);
    }

    if !stream.is_null() {
        unsafe { cubeb_stream_stop(stream) };
        unsafe { cubeb_stream_destroy(stream) };
    }
    unsafe { cubeb_destroy(ctx) };

    assert_eq!(CUBEB_OK, unsafe { cubeb_set_log_callback(CUBEB_LOG_DISABLED, None) });
}
