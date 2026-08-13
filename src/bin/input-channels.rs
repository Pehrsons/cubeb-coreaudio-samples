//! What channel count cubeb offers for an input device, and whether a stream can actually be created
//! with it, while a VoiceProcessingIO unit holds the device in its raw multi-channel array mode.
//!
//! For bug 2054983, where the VPIO forcelist is being removed. With the forcelist, an input-only
//! stream that requests no processing still gets a VPIO unit in bypass, and the backend counts one
//! channel per stream for forcelisted devices. Without it, that stream gets a plain HALOutput unit
//! instead -- but the built-in mic is still switched to `96000 Hz, 3 ch` for as long as some other
//! stream's VPIO unit exists, so the question is whether the backend still reports one channel, as it
//! does when no VPIO is involved anywhere, or starts reporting the raw array.
//!
//! The check is run twice against the same device, once with no VPIO anywhere and once with a duplex
//! stream holding a VPIO unit open, so the two answers can be compared directly:
//!
//!   - what `cubeb_enumerate_devices` reports as `max_channels`
//!   - what the device reports to CoreAudio, for reference
//!   - whether `cubeb_stream_init` succeeds for an input-only stream with no processing params at 1,
//!     2 and 3 channels, and how many channels of data actually arrive
//!
//! Build both ways to compare: the default features include `vpio-forcelist`, and
//! `--no-default-features` is the behaviour the patch makes unconditional.
//!
//!     cargo run --release --bin input-channels -- --device "MacBook Pro Microphone"
//!     cargo run --release --no-default-features --bin input-channels -- --device "MacBook Pro Microphone"

use clap::Parser;
use cubeb_backend::ffi::*;
use cubeb_coreaudio_samples::devinfo::{
    default_input_device, default_output_device, device_name, device_uid, input_channels,
    resolve_input_device, resolve_output_device,
};
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::{process, ptr, slice, thread};

extern "C" {
    fn print_log(msg: *const c_char, ...);
}

#[derive(Parser)]
#[command(
    about = "Channel counts cubeb offers for an input device, with and without VPIO attached"
)]
struct Args {
    /// Input device, by name substring or AudioDeviceID. Defaults to the system default input.
    #[arg(long)]
    device: Option<String>,
    /// Output device for the duplex stream that holds the VPIO unit open.
    #[arg(long)]
    output_device: Option<String>,
    /// Rate to request for the cubeb streams.
    #[arg(long, default_value_t = 48000)]
    rate: u32,
    /// Channel counts to attempt, in order.
    #[arg(long, value_delimiter = ',', default_values_t = [1u32, 2, 3])]
    try_channels: Vec<u32>,
    /// Turn on cubeb's own logging, which reports how it counts channels.
    #[arg(short = 'g', long)]
    log: bool,
    /// How long to run a stream that opened, to see whether data arrives.
    #[arg(long, default_value_t = 1.0)]
    settle: f64,
}

/// Per-stream callback state. Records what actually arrives, which is the part a channel count on
/// paper does not tell you.
struct StreamCtx {
    name: String,
    channels: usize,
    frames: AtomicU64,
    callbacks: AtomicU64,
    /// Set per channel when a non-zero sample is seen, so a channel that is present but always
    /// silent is distinguishable from one carrying audio.
    nonzero: Vec<AtomicU64>,
}

extern "C" fn data_callback(
    _stream: *mut cubeb_stream,
    user_ptr: *mut c_void,
    input_buffer: *const c_void,
    output_buffer: *mut c_void,
    nframes: i64,
) -> i64 {
    let ctx = unsafe { &*(user_ptr as *const StreamCtx) };
    let frames = nframes.max(0) as usize;

    if !input_buffer.is_null() && frames > 0 {
        let samples =
            unsafe { slice::from_raw_parts(input_buffer as *const f32, frames * ctx.channels) };
        for (i, sample) in samples.iter().enumerate() {
            if *sample != 0.0 {
                ctx.nonzero[i % ctx.channels].fetch_add(1, Ordering::Relaxed);
            }
        }
        ctx.frames.fetch_add(frames as u64, Ordering::Relaxed);
        ctx.callbacks.fetch_add(1, Ordering::Relaxed);
    }

    // Duplex streams must fill the output, or the speakers get whatever was in the buffer.
    if !output_buffer.is_null() && frames > 0 {
        let out =
            unsafe { slice::from_raw_parts_mut(output_buffer as *mut f32, frames * ctx.channels) };
        out.fill(0.0);
    }
    nframes
}

extern "C" fn state_callback(_stream: *mut cubeb_stream, user_ptr: *mut c_void, state: u32) {
    let ctx = unsafe { &*(user_ptr as *const StreamCtx) };
    if state == CUBEB_STATE_ERROR {
        println!("      [{}] state: ERROR", ctx.name);
    }
}

fn rv_name(rv: i32) -> String {
    match rv {
        CUBEB_OK => "OK".to_string(),
        CUBEB_ERROR => "CUBEB_ERROR".to_string(),
        CUBEB_ERROR_INVALID_FORMAT => "CUBEB_ERROR_INVALID_FORMAT".to_string(),
        CUBEB_ERROR_INVALID_PARAMETER => "CUBEB_ERROR_INVALID_PARAMETER".to_string(),
        CUBEB_ERROR_NOT_SUPPORTED => "CUBEB_ERROR_NOT_SUPPORTED".to_string(),
        CUBEB_ERROR_DEVICE_UNAVAILABLE => "CUBEB_ERROR_DEVICE_UNAVAILABLE".to_string(),
        other => format!("rv {}", other),
    }
}

/// A live cubeb stream, with its callback state kept alive alongside it.
struct Stream {
    stream: *mut cubeb_stream,
    ctx: Box<StreamCtx>,
}

impl Stream {
    /// `voice` requests PREF_VOICE, which is what makes the backend use VPIO. `params` of `None`
    /// means the stream never sets processing params at all, like a client that does not care --
    /// which is the case the forcelist exists for.
    fn open(
        context: *mut cubeb,
        name: &str,
        device_uid: &CStr,
        output_uid: Option<&CStr>,
        channels: u32,
        rate: u32,
        voice: bool,
        params: Option<cubeb_input_processing_params>,
    ) -> Result<Stream, i32> {
        let mut ctx = Box::new(StreamCtx {
            name: name.to_string(),
            channels: channels as usize,
            frames: AtomicU64::new(0),
            callbacks: AtomicU64::new(0),
            nonzero: (0..channels).map(|_| AtomicU64::new(0)).collect(),
        });

        let mut input_params = cubeb_stream_params {
            channels,
            format: CUBEB_SAMPLE_FLOAT32NE,
            rate,
            layout: if channels == 1 {
                CUBEB_LAYOUT_MONO
            } else {
                CUBEB_LAYOUT_UNDEFINED
            },
            prefs: if voice {
                CUBEB_STREAM_PREF_VOICE
            } else {
                CUBEB_STREAM_PREF_NONE
            },
            input_params: params.unwrap_or(CUBEB_INPUT_PROCESSING_PARAM_NONE),
        };
        let mut output_params = input_params;

        let mut stream: *mut cubeb_stream = ptr::null_mut();
        let stream_name = CString::new(name).unwrap();
        let user_ptr = ctx.as_mut() as *mut StreamCtx as *mut c_void;
        let rv = unsafe {
            cubeb_stream_init(
                context,
                &mut stream,
                stream_name.as_ptr(),
                device_uid.as_ptr() as cubeb_devid,
                &mut input_params,
                match output_uid {
                    Some(uid) => uid.as_ptr() as cubeb_devid,
                    None => ptr::null(),
                },
                match output_uid {
                    Some(_) => &mut output_params,
                    None => ptr::null_mut(),
                },
                512,
                Some(data_callback),
                Some(state_callback),
                user_ptr,
            )
        };
        if rv != CUBEB_OK {
            return Err(rv);
        }

        // Processing params are set after init, as cubeb clients do, so a failure there is visible
        // separately from a failure to create the stream.
        if let Some(params) = params {
            let rv = unsafe { cubeb_stream_set_input_processing_params(stream, params) };
            if rv != CUBEB_OK {
                println!("      [{}] set_input_processing_params: {}", name, rv_name(rv));
            }
        }
        let rv = unsafe { cubeb_stream_start(stream) };
        if rv != CUBEB_OK {
            unsafe { cubeb_stream_destroy(stream) };
            return Err(rv);
        }
        Ok(Stream { stream, ctx })
    }

    fn report(&self) {
        let frames = self.ctx.frames.load(Ordering::Relaxed);
        let carrying = self
            .ctx
            .nonzero
            .iter()
            .filter(|n| n.load(Ordering::Relaxed) > 0)
            .count();
        println!(
            "      delivered {} frames in {} callbacks, {} of {} channels carrying audio",
            frames,
            self.ctx.callbacks.load(Ordering::Relaxed),
            carrying,
            self.ctx.channels
        );
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            cubeb_stream_stop(self.stream);
            cubeb_stream_destroy(self.stream);
        }
    }
}

/// What cubeb reports for the device under test. `None` if cubeb does not list it at all.
fn reported_max_channels(context: *mut cubeb, uid: &str) -> Option<u32> {
    let mut collection = cubeb_device_collection {
        device: ptr::null_mut(),
        count: 0,
    };
    let rv = unsafe { cubeb_enumerate_devices(context, CUBEB_DEVICE_TYPE_INPUT, &mut collection) };
    if rv != CUBEB_OK {
        println!("  cubeb_enumerate_devices failed: {}", rv_name(rv));
        return None;
    }
    let devices = unsafe { slice::from_raw_parts(collection.device, collection.count) };
    let found = devices
        .iter()
        .find(|info| {
            !info.device_id.is_null()
                && unsafe { CStr::from_ptr(info.device_id) }.to_string_lossy() == uid
        })
        .map(|info| info.max_channels);
    unsafe { cubeb_device_collection_destroy(context, &mut collection) };
    found
}

/// Asks cubeb for the device's channel count, then tries to open an input-only stream with no
/// processing params at each requested channel count. Returns what cubeb reported.
fn probe(
    context: *mut cubeb,
    label: &str,
    device: u32,
    uid: &CStr,
    args: &Args,
) -> (Option<u32>, Vec<(u32, Result<(), i32>)>) {
    println!("\n=== {} ===", label);
    let uid_str = uid.to_string_lossy().into_owned();
    let reported = reported_max_channels(context, &uid_str);
    println!(
        "  cubeb reports max_channels: {}",
        reported
            .map(|c| c.to_string())
            .unwrap_or("(not listed)".into())
    );
    println!(
        "  CoreAudio reports on the device: {} input channels",
        input_channels(device)
            .map(|c| c.to_string())
            .unwrap_or("?".into())
    );

    let mut results = Vec::new();
    for channels in &args.try_channels {
        // No voice pref and no processing params: the stream the forcelist is about.
        print!("  input-only, no PREF_VOICE, no params, {} ch: ", channels);
        match Stream::open(context, "probe", uid, None, *channels, args.rate, false, None) {
            Ok(stream) => {
                println!("opened");
                thread::sleep(Duration::from_secs_f64(args.settle));
                stream.report();
                results.push((*channels, Ok(())));
            }
            Err(rv) => {
                println!("REFUSED, {}", rv_name(rv));
                results.push((*channels, Err(rv)));
            }
        }
    }
    (reported, results)
}

fn main() {
    let args = Args::parse();

    let device = match &args.device {
        Some(spec) => resolve_input_device(spec).unwrap_or_else(|e| {
            eprintln!("{}", e);
            process::exit(2);
        }),
        None => default_input_device().unwrap_or_else(|| {
            eprintln!("no default input device");
            process::exit(2);
        }),
    };
    let uid = CString::new(device_uid(device).unwrap_or_else(|| {
        eprintln!("input device has no UID");
        process::exit(2);
    }))
    .unwrap();
    // The VPIO stream is duplex, so it needs an output device: the named one, or the system default.
    let output = match args.output_device.as_deref() {
        Some(spec) => resolve_output_device(spec).unwrap_or_else(|e| {
            eprintln!("{}", e);
            process::exit(2);
        }),
        None => default_output_device().unwrap_or_else(|| {
            eprintln!("no default output device");
            process::exit(2);
        }),
    };
    let output_uid = CString::new(device_uid(output).unwrap_or_else(|| {
        eprintln!("output device has no UID");
        process::exit(2);
    }))
    .unwrap();

    if args.log {
        unsafe { cubeb_set_log_callback(CUBEB_LOG_NORMAL, Some(print_log)) };
    }

    let mut context: *mut cubeb = ptr::null_mut();
    let context_name = CString::new("input-channels").unwrap();
    let rv = unsafe { cubeb_init(&mut context, context_name.as_ptr(), ptr::null()) };
    assert_eq!(rv, CUBEB_OK, "cubeb_init failed");

    println!("Input device:  {} \"{}\"", device, device_name(device));
    println!("Output device: {} \"{}\" (for the duplex VPIO stream)", output, device_name(output));
    println!(
        "vpio-forcelist: {}",
        if cfg!(feature = "vpio-forcelist") {
            "on"
        } else {
            "off"
        }
    );
    println!("Requesting {} Hz for the cubeb streams", args.rate);

    let (before, before_results) = probe(context, "no VPIO anywhere", device, &uid, &args);

    // The stream that holds a VPIO unit open: duplex, PREF_VOICE, full processing.
    println!("\n=== opening a duplex PREF_VOICE stream with aec+ns+agc ===");
    let params = CUBEB_INPUT_PROCESSING_PARAM_ECHO_CANCELLATION
        | CUBEB_INPUT_PROCESSING_PARAM_NOISE_SUPPRESSION
        | CUBEB_INPUT_PROCESSING_PARAM_AUTOMATIC_GAIN_CONTROL;
    let voice = match Stream::open(
        context,
        "voice",
        &uid,
        Some(output_uid.as_c_str()),
        1,
        args.rate,
        true,
        Some(params),
    ) {
        Ok(stream) => {
            println!("  opened");
            stream
        }
        Err(rv) => {
            eprintln!("  FAILED to open the VPIO stream: {}", rv_name(rv));
            unsafe { cubeb_destroy(context) };
            process::exit(1);
        }
    };
    thread::sleep(Duration::from_secs_f64(args.settle.max(1.0)));
    voice.report();

    let (during, during_results) = probe(context, "VPIO duplex stream live", device, &uid, &args);

    drop(voice);
    println!("\n=== verdict ===");
    let fmt = |c: Option<u32>| c.map(|c| c.to_string()).unwrap_or("(not listed)".into());
    println!(
        "  cubeb max_channels: {} with no VPIO, {} with a VPIO stream live{}",
        fmt(before),
        fmt(during),
        if before == during {
            ""
        } else {
            "   <-- CHANGED"
        }
    );
    for (channels, _) in &before_results {
        let b = before_results
            .iter()
            .find(|(c, _)| c == channels)
            .map(|(_, r)| r.is_ok());
        let d = during_results
            .iter()
            .find(|(c, _)| c == channels)
            .map(|(_, r)| r.is_ok());
        println!(
            "  {} ch: {} with no VPIO, {} with a VPIO stream live{}",
            channels,
            if b == Some(true) { "opens" } else { "refused" },
            if d == Some(true) { "opens" } else { "refused" },
            if b == d { "" } else { "   <-- CHANGED" }
        );
    }
    println!(
        "\n  Expected: identical columns. The built-in mic supports one channel, and another\n  \
         stream's VPIO unit switching the device to its raw array should not change what an\n  \
         input-only stream is offered or allowed to open."
    );

    unsafe { cubeb_destroy(context) };
}
