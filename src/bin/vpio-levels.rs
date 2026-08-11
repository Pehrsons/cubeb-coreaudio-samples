//! Measure input levels, in dBFS, across combinations of cubeb streams.
//!
//! Built for bug 1896938: the built-in mic delivers a very quiet signal in some combinations of
//! streams, notably the ones Google Meet produces (a processed stream, then a raw stream, then the
//! processed one goes away). Each measurement reports the level every live cubeb stream receives,
//! plus optionally what a plain non-cubeb CoreAudio capture client receives at the same time, plus
//! what changed in the device's CoreAudio properties.
//!
//! Runs are scripted, so combinations are cheap to try:
//!
//!     vpio-levels --scenario meet
//!     vpio-levels "probe on; open a voice proc duplex; measure 5; open b; measure 5; close a; measure 5"

use clap::{CommandFactory, FromArgMatches, Parser};
use cubeb_backend::ffi::*;
use cubeb_coreaudio_samples::devinfo::{
    cubeb_input_channel_count, default_input_device, default_output_device, device_name,
    device_uid, input_channels, input_devices, output_devices, resolve_input_device,
    resolve_output_device, DeviceSnapshot,
};
use cubeb_coreaudio_samples::knobs;
use cubeb_coreaudio_samples::meter::{fmt_dbfs, Meter, Report, Snapshot};
use cubeb_coreaudio_samples::probe::{InputProbe, ProbeKind};
use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{process, ptr, slice, thread};

extern "C" {
    fn print_log(msg: *const c_char, ...);
}

// Built with --features zeroing-alloc to test whether anything here depends on reading
// uninitialized heap memory, as Safari's zeroing allocator would mask.
#[cfg(feature = "zeroing-alloc")]
#[global_allocator]
static ALLOC: cubeb_coreaudio_samples::zeroing::ZeroingAlloc =
    cubeb_coreaudio_samples::zeroing::ZeroingAlloc;

const LATENCY_FRAMES: u32 = 512;
const TONE_HZ: f64 = 440.0;
/// How long to wait for a freshly started stream to deliver its first input buffer.
const FIRST_INPUT_TIMEOUT: Duration = Duration::from_secs(15);

/// Scripts for the stream combinations of interest. `--list` prints these.
const SCENARIOS: &[(&str, &str, &str)] = &[
    (
        "baseline",
        "What a plain CoreAudio client gets with no cubeb stream at all",
        "probe on; measure",
    ),
    (
        "vpio-processed",
        "One VPIO input stream with AEC+NS+AGC (Firefox, processed gUM request)",
        "probe on; open a voice proc; measure",
    ),
    (
        "vpio-bypass",
        "One VPIO input stream with processing off, i.e. VPIO in bypass mode",
        "probe on; open a voice none; measure",
    ),
    (
        "plain",
        "One input stream with no voice pref; on the built-in mic the forcelist still makes this \
         VPIO, in bypass",
        "probe on; open a; measure",
    ),
    (
        "meet",
        "The Google Meet sequence from comment 60: processed duplex stream, then a raw stream, \
         then the processed one is dropped, then processed again",
        "probe on; measure; open a voice proc duplex; measure; open b; measure; close a; \
         measure; open c voice proc duplex; measure; close b; measure",
    ),
    (
        "meet-reverse",
        "The same combination reached in the opposite order, to see if ordering matters",
        "probe on; measure; open b; measure; open a voice proc duplex; measure; close b; measure",
    ),
    (
        "two-processed",
        "Two simultaneous processed VPIO streams, i.e. two VPIO units on one device",
        "probe on; measure; open a voice proc; measure; open b voice proc; measure; close a; measure",
    ),
    (
        "reopen-raw",
        "A raw stream while a processed one exists, then reopened after the processed one is gone \
         (what Firefox would do if it re-created the stream)",
        "probe on; open a voice proc duplex; measure; open b; measure; close a; measure; \
         close b; open b2; measure",
    ),
    (
        "bug1896938",
        "One-command diagnostic for bug 1896938: the level every path gets, measured \
         simultaneously, plus recovery after the VPIO idle timeout. Run this with something \
         playing steadily in the room and paste the summary table.",
        "probe on; \
         note baseline, no VPIO anywhere -- the plain client should read the device normal level; \
         measure; \
         open bypass voice none; \
         note VPIO in bypass -- it should be loud, while the plain client loses 25-30 dB as the \
         device switches to its 3ch raw array; \
         measure; \
         open processed voice proc; \
         note bypass and processed side by side -- both should be within a few dB, processing \
         usually costs about 5 dB; \
         measure; \
         close bypass; close processed; \
         note both streams destroyed but the pooled VPIO unit is still alive -- the plain client \
         should still be attenuated; \
         measure; \
         sleep 12; probe restart; \
         note past VPIO_IDLE_TIMEOUT so the unit is disposed -- the plain client should be back \
         at the baseline level; \
         measure",
    ),
    (
        "safari-compare",
        "Isolates the ways cubeb configures a non-bypassed VPIO unit differently from WebKit, all \
         measured in the same window so the acoustics are identical: whether VPIO's output side is \
         enabled (WebKit never disables it on macOS) and whether AGC is on (WebKit never touches \
         it, so it keeps VPIO's default). Worth also running with --rate 44100 and --rate 96000: \
         WebKit configures VPIO at the device's nominal rate, cubeb leaves the unit at its 44100 \
         default and resamples to the requested rate.",
        "probe on; \
         note baseline, no VPIO anywhere; \
         measure 5; \
         open input-agc voice proc; open duplex-agc voice proc duplex; \
         open input-noagc voice aec+ns; \
         note three non-bypassed VPIO configurations side by side, in the first seconds after \
         start -- input-agc is what cubeb does today for an input-only stream, duplex-agc adds the \
         output side WebKit always leaves enabled, input-noagc turns AGC off which WebKit never \
         does -- all three should land within a dB or two of each other, so any real gap points at \
         that setting; \
         measure 5; \
         sleep 12; volume ?; \
         note the same three once the voice processing has settled -- bug 2054983 comment 6 \
         reports the level dropping about 5 dB a few seconds in, so compare against the window \
         above, and check whether the device volume moved on its own; \
         measure 6",
    ),
    (
        "churn",
        "For bug 2054983 comment 6: capture that degrades after repeated use, where Safari stays \
         fine. Each round crosses VPIO_IDLE_TIMEOUT so the pooled unit is really disposed of and \
         recreated, which is what a user starting and stopping capture repeatedly produces. Watch \
         whether the processed level drifts down round over round, and whether any stream warns \
         about receiving no input.",
        "probe on; \
         note baseline before any VPIO use; \
         measure 4; \
         open ref voice proc; \
         note first use, in the first seconds after start; measure 4; \
         sleep 10; volume ?; \
         note first use, once the voice processing has settled -- bug 2054983 comment 6 reports \
         losing about 5 dB a few seconds in, which would show up as a gap against the window \
         above; \
         measure 4; close ref; \
         cycle 5 voice proc; sleep 12; cycle 5 voice proc; sleep 12; \
         open ref2 voice proc; \
         note after 10 cycles and two real disposals, first seconds; measure 4; \
         sleep 10; note after 10 cycles, settled; measure 4; close ref2; \
         cycle 5 voice proc; sleep 12; cycle 5 voice proc; sleep 12; \
         open ref3 voice proc; \
         note after 20 cycles, first seconds -- compare with the very first stream; measure 4; \
         sleep 10; volume ?; \
         note after 20 cycles, settled -- compare with the first settled window; measure 4; \
         close ref3; sleep 12; probe restart; \
         note the plain client alone again, past the idle timeout; measure 4",
    ),
    (
        "native-vpio",
        "Sets up VoiceProcessingIO directly, without cubeb, and measures it beside cubeb's own \
         stream and a plain client, all in the same window. If each native and cubeb pair agrees, \
         the level behaviour belongs to Apple's voice processing; if they differ, it belongs to how \
         cubeb configures the unit (rate, format, channel count, output-scope IO). The bypassed pair \
         is the one to watch on M3 and later, where bypass reads the raw array.",
        "probe on; \
         note baseline, plain client only, no voice processing anywhere; \
         measure 5; \
         native nat-proc vpio proc; native nat-bypass vpio none; \
         open cub-proc voice proc; open cub-bypass voice none; \
         note all four side by side, in the first seconds after start -- each native and cubeb pair \
         should agree, since the only difference within a pair is who configured the unit; \
         measure 5; \
         sleep 12; \
         note the same four once the voice processing has settled; \
         measure 6",
    ),
    (
        "probe-during-vpio",
        "What a plain CoreAudio client gets before, during and after a processed VPIO stream \
         (the reason the VPIO forcelist exists)",
        "probe on; measure; open a voice proc; measure; close a; measure; sleep 12; measure",
    ),
];

#[derive(Parser, Debug)]
#[clap(about = "Measure cubeb input levels in dBFS across stream combinations")]
struct Args {
    /// Script of steps separated by ';'. See --list for examples.
    script: Option<String>,
    /// Run a named scenario instead of a script. Listed at the end of --help.
    #[clap(long, short)]
    scenario: Option<String>,
    /// List the step grammar, plus every scenario with its description and the steps it runs.
    #[clap(long, short)]
    list: bool,
    /// Print cubeb's log.
    #[clap(long, short = 'g')]
    log: bool,
    /// Duration in seconds of a `measure` step that doesn't give its own.
    #[clap(long, short, default_value = "5")]
    measure: f64,
    /// Print the device's full CoreAudio property set at every measurement, not just what changed.
    #[clap(long, short)]
    devinfo: bool,
    /// Input device to use, as an AudioDeviceID or a substring of its name (e.g. "MacBook Pro
    /// Microphone"). Defaults to the system default input device. Both the cubeb streams and the
    /// plain capture probe use it, so they always measure the same device.
    #[clap(long)]
    device: Option<String>,
    /// Output device for duplex streams, as an AudioDeviceID or a substring of its name. Defaults
    /// to the system default output. Per the bug report the built-in speakers are what matters:
    /// they share a device group with the built-in mic, so that is the pairing where VPIO's
    /// internal voice path engages.
    #[clap(long)]
    output_device: Option<String>,
    /// List the input and output devices and exit.
    #[clap(long)]
    list_devices: bool,
    /// List every gain-adjacent knob for the input device -- AudioUnit parameters for both a VPIO
    /// and a plain capture unit, the device's control objects, and the voice-processing
    /// properties -- then exit.
    #[clap(long)]
    knobs: bool,
    /// Sample rate to request for the cubeb streams.
    #[clap(long, default_value = "48000")]
    rate: u32,
    /// Channel count to request for the cubeb streams. Worth varying on the built-in mic, which
    /// presents 3 channels while VPIO is attached: asking for 1 makes cubeb mix them down.
    #[clap(long, default_value = "1")]
    channels: u32,
}

fn main() {
    // The scenario list lives in SCENARIOS, so attach it to --help at runtime rather than
    // duplicating it in an attribute.
    let args = Args::command()
        .after_help(scenario_listing(false))
        .get_matches();
    let args = Args::from_arg_matches(&args).unwrap_or_else(|e| e.exit());
    if args.list {
        print_help();
        return;
    }
    if args.list_devices {
        println!("Input devices:");
        for (id, name) in input_devices() {
            println!("{:>12}  {}", id, name);
        }
        println!("Output devices:");
        for (id, name) in output_devices() {
            println!("{:>12}  {}", id, name);
        }
        return;
    }

    if args.knobs {
        let device = match &args.device {
            Some(spec) => resolve_input_device(spec).unwrap_or_else(|e| fail(&e)),
            None => default_input_device().expect("no default input device"),
        };
        println!("Input device: {} \"{}\"", device, device_name(device));
        println!("  AudioUnit parameters and voice-processing properties:");
        print!("{}", cubeb_coreaudio_samples::knobs::describe_units(device));
        println!("  Device controls:");
        print!("{}", cubeb_coreaudio_samples::knobs::describe_device_controls(device));
        return;
    }

    let script = match (&args.script, &args.scenario) {
        (Some(script), None) => script.clone(),
        (None, Some(name)) => match SCENARIOS.iter().find(|(n, _, _)| n == name) {
            Some((_, _, script)) => script.to_string(),
            None => {
                eprintln!("Unknown scenario \"{}\". Use --list to see them.", name);
                process::exit(2);
            }
        },
        (Some(_), Some(_)) => {
            eprintln!("Pass either a script or --scenario, not both.");
            process::exit(2);
        }
        (None, None) => {
            eprintln!("Nothing to run. Pass a script or --scenario, or see --list.");
            process::exit(2);
        }
    };

    let steps = match parse(&script) {
        Ok(steps) => steps,
        Err(e) => {
            eprintln!("{}", e);
            process::exit(2);
        }
    };

    if args.log {
        assert_eq!(CUBEB_OK, unsafe { cubeb_set_log_callback(CUBEB_LOG_NORMAL, Some(print_log)) });
    }

    let mut runner = Runner::new(&args);
    if let Some(name) = &args.scenario {
        println!("Scenario: {}", name);
    }
    println!("Script:   {}\n", script);
    for step in steps {
        runner.run(step);
    }
    runner.finish();
}

/// Bail out with a message. Clears cubeb's log callback first: its async logger asserts in its
/// static destructor if a callback is still installed, which turns a clean exit into an abort.
fn fail(message: &str) -> ! {
    eprintln!("{}", message);
    unsafe { cubeb_set_log_callback(CUBEB_LOG_DISABLED, None) };
    process::exit(2);
}

/// Resolve a device spec to its UID, for handing to cubeb as a devid.
fn resolve_uid(spec: &str, input: bool) -> CString {
    let resolve = if input {
        resolve_input_device
    } else {
        resolve_output_device
    };
    let device = resolve(spec).unwrap_or_else(|e| fail(&e));
    let uid = cubeb_coreaudio_samples::devinfo::device_uid(device)
        .unwrap_or_else(|| panic!("device {} has no UID", device));
    CString::new(uid).unwrap()
}

fn print_help() {
    print!("{}", STEP_HELP.replace("{tone_hz}", &TONE_HZ.to_string()));
    println!();
    print!("{}", scenario_listing(true));
}

/// Wrap `text` to `width` columns, prefixing every line with `indent` spaces.
fn wrap_indent(text: &str, width: usize, indent: usize) -> String {
    let prefix = " ".repeat(indent);
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && indent + line.len() + 1 + word.len() > width {
            out.push_str(&prefix);
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push_str(&prefix);
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The scenarios and what each one is for, shown by --help and, with the steps each one runs, by
/// --list.
fn scenario_listing(scripts: bool) -> String {
    const WIDTH: usize = 96;
    let mut out = String::from("Scenarios (--scenario <name>):\n");
    for (name, description, script) in SCENARIOS {
        out.push_str(&format!("  {}\n", name));
        out.push_str(&wrap_indent(description, WIDTH, 6));
        if scripts {
            out.push_str(&wrap_indent(script, WIDTH, 10));
        }
    }
    if !scripts {
        out.push_str("\nUse --list for the step grammar and the steps each scenario runs.\n");
    }
    out
}

const STEP_HELP: &str = "\
Steps, separated by ';':
  open <name> [voice] [duplex] [proc|aec|ns|agc|none] [tone] [in=<dev>] [out=<dev>] [ch=<n>]
                        Create and start a stream. `voice` sets CUBEB_STREAM_PREF_VOICE,
                        `duplex` adds an output side, `proc` is AEC+NS+AGC, `none` sets the
                        processing params explicitly to none (VPIO bypass), and `tone` plays
                        a {tone_hz} Hz tone on the output side.
                        `in=`/`out=` override this run's devices for that stream, to point a
                        recycled VPIO unit at a device it was not configured for. `out=` implies
                        duplex. `ch=` overrides --channels for that stream, so a mono and a
                        stereo request can be compared in one window.
  close <name>          Stop and destroy a stream.
  stop <name> / start <name>
                        Stop or restart a stream without destroying it. Processing params set
                        on a running stream only take effect on restart.
  params <name> <spec>  Set input processing params, e.g. `proc`, `none`, `aec+ns`.
  native <name> hal|vpio [params] [rate=<hz>] [no-outio]
                        Open a capture client set up natively, without cubeb. `vpio` instantiates a
                        VoiceProcessingIO unit directly, so its level can be compared with cubeb's;
                        without a params spec the unit keeps its own defaults, as WebKit leaves
                        them. `rate=` sets the unit's capture format rate the way WebKit configures
                        the mic proc, where omitting it imposes no format at all; note --rate only
                        applies to cubeb streams. `no-outio` disables IO on the output scope the way
                        cubeb does for an input-only stream.
  native off <name>     Close a native client.
  probe on|off|restart  Start/stop a plain non-cubeb CoreAudio capture client, or reopen it so
                        its channel count matches the device's current one (the built-in mic
                        changes it when VPIO attaches).
  tone <name> on|off    Start/stop the output tone on a duplex stream, to give VPIO's echo
                        canceller something to cancel.
  note <text>           Describe what the next measurement is for, and what to expect. Shown
                        with that measurement and in the closing summary.
  volume <scalar|?>     Read, or set, the device's input volume (the system input slider). The
                        original value is restored when the run ends.
  probevol <v> [bus]    Set the probe unit's own Volume parameter (kHALOutputParam_Volume), to
                        test whether that per-client knob affects capture. Bus 1 by default.
  cycle <n> [spec...]   Open and immediately close a stream n times, to churn VPIO units. Note
                        that back-to-back cycles reuse the pooled unit; put `sleep 12` between
                        `cycle` steps to cross VPIO_IDLE_TIMEOUT so units are really disposed of
                        and recreated.
  measure [secs]        Capture for a while and report levels for everything live.
  sleep <secs>          Wait without reporting (e.g. past the 10s VPIO idle timeout).
  devinfo               Dump the input device's CoreAudio properties.
";

#[derive(Debug)]
enum Step {
    Open {
        name: String,
        spec: StreamSpec,
    },
    Close(String),
    Stop(String),
    Start(String),
    Params {
        name: String,
        params: cubeb_input_processing_params,
        spec: String,
    },
    Probe(ProbeAction),
    Native {
        name: String,
        kind: ProbeKind,
    },
    NativeOff(String),
    Tone {
        name: String,
        on: bool,
    },
    Measure(Option<f64>),
    Note(String),
    Volume(Option<f32>),
    ProbeUnitVolume {
        value: f32,
        element: u32,
    },
    Cycle {
        count: usize,
        spec: StreamSpec,
    },
    Sleep(f64),
    DevInfo,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ProbeAction {
    On,
    Off,
    /// Close and reopen the probe, so its client format matches the device's current one.
    Restart,
}

#[derive(Clone, Debug)]
struct StreamSpec {
    voice: bool,
    duplex: bool,
    tone: bool,
    /// Per-stream device overrides, so a recycled VPIO unit can be pointed at a different device
    /// than the one it was last configured for.
    input: Option<String>,
    output: Option<String>,
    /// Per-stream channel count, overriding --channels. Asking for 2 where cubeb would otherwise
    /// downmix to 1 shows whether that downmix is costing level.
    channels: Option<u32>,
    /// `None` means the stream never sets processing params, like a cubeb client that doesn't care.
    params: Option<cubeb_input_processing_params>,
    text: String,
}

fn parse_params(spec: &str) -> Option<cubeb_input_processing_params> {
    let mut params = CUBEB_INPUT_PROCESSING_PARAM_NONE;
    for token in spec.split('+') {
        match token {
            "none" => {}
            "proc" | "all" => {
                params |= CUBEB_INPUT_PROCESSING_PARAM_ECHO_CANCELLATION
                    | CUBEB_INPUT_PROCESSING_PARAM_NOISE_SUPPRESSION
                    | CUBEB_INPUT_PROCESSING_PARAM_AUTOMATIC_GAIN_CONTROL;
            }
            "aec" => params |= CUBEB_INPUT_PROCESSING_PARAM_ECHO_CANCELLATION,
            "ns" => params |= CUBEB_INPUT_PROCESSING_PARAM_NOISE_SUPPRESSION,
            "agc" => params |= CUBEB_INPUT_PROCESSING_PARAM_AUTOMATIC_GAIN_CONTROL,
            "vi" => params |= CUBEB_INPUT_PROCESSING_PARAM_VOICE_ISOLATION,
            _ => return None,
        }
    }
    Some(params)
}

fn describe_params(params: cubeb_input_processing_params) -> String {
    let mut parts = Vec::new();
    if params & CUBEB_INPUT_PROCESSING_PARAM_ECHO_CANCELLATION != 0 {
        parts.push("aec");
    }
    if params & CUBEB_INPUT_PROCESSING_PARAM_NOISE_SUPPRESSION != 0 {
        parts.push("ns");
    }
    if params & CUBEB_INPUT_PROCESSING_PARAM_AUTOMATIC_GAIN_CONTROL != 0 {
        parts.push("agc");
    }
    if params & CUBEB_INPUT_PROCESSING_PARAM_VOICE_ISOLATION != 0 {
        parts.push("vi");
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("+")
    }
}

/// Parse the tokens that describe a stream, shared by `open` and `cycle`.
fn parse_stream_spec(tokens: &[&str], raw: &str) -> Result<StreamSpec, String> {
    let mut spec = StreamSpec {
        voice: false,
        duplex: false,
        tone: false,
        input: None,
        output: None,
        channels: None,
        params: None,
        text: String::new(),
    };
    for token in tokens {
        match *token {
            "voice" => spec.voice = true,
            "duplex" => spec.duplex = true,
            "tone" => spec.tone = true,
            token if token.starts_with("ch=") => {
                spec.channels = Some(
                    token["ch=".len()..]
                        .parse::<u32>()
                        .map_err(|e| format!("bad channel count: {}", e))?,
                )
            }
            token if token.starts_with("in=") => {
                spec.input = Some(token["in=".len()..].to_string())
            }
            token if token.starts_with("out=") => {
                spec.duplex = true;
                spec.output = Some(token["out=".len()..].to_string())
            }
            other => match parse_params(other) {
                Some(params) => spec.params = Some(params),
                None => return Err(format!("Unknown token \"{}\" in \"{}\"", other, raw.trim())),
            },
        }
    }
    if let Some(device) = &spec.input {
        spec.text = format!("in={} ", device);
    }
    if let Some(device) = &spec.output {
        spec.text = format!("{}out={} ", spec.text, device);
    }
    if let Some(channels) = spec.channels {
        spec.text = format!("{}ch={} ", spec.text, channels);
    }
    spec.text = format!(
        "{}{}, {}, params {}",
        spec.text,
        if spec.voice { "voice" } else { "no voice pref" },
        if spec.duplex { "duplex" } else { "input-only" },
        match spec.params {
            Some(p) => describe_params(p),
            None => "unset".to_string(),
        }
    );
    Ok(spec)
}

fn parse(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for raw in script.split(';') {
        let tokens: Vec<&str> = raw.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        let step = match tokens[0] {
            "cycle" => {
                let count = tokens
                    .get(1)
                    .ok_or("`cycle` needs a count")?
                    .parse::<usize>()
                    .map_err(|e| format!("bad cycle count: {}", e))?;
                Step::Cycle {
                    count,
                    spec: parse_stream_spec(&tokens[2..], raw)?,
                }
            }
            "open" => {
                let name = tokens
                    .get(1)
                    .ok_or_else(|| format!("`open` needs a stream name: \"{}\"", raw.trim()))?;
                Step::Open {
                    name: name.to_string(),
                    spec: parse_stream_spec(&tokens[2..], raw)?,
                }
            }
            "close" | "stop" | "start" => {
                let name = tokens
                    .get(1)
                    .ok_or_else(|| format!("`{}` needs a stream name", tokens[0]))?
                    .to_string();
                match tokens[0] {
                    "close" => Step::Close(name),
                    "stop" => Step::Stop(name),
                    _ => Step::Start(name),
                }
            }
            "params" => {
                let name = tokens
                    .get(1)
                    .ok_or("`params` needs a stream name")?
                    .to_string();
                let spec = tokens.get(2).ok_or("`params` needs a params spec")?;
                let params = parse_params(spec)
                    .ok_or_else(|| format!("Unknown params spec \"{}\"", spec))?;
                Step::Params {
                    name,
                    params,
                    spec: spec.to_string(),
                }
            }
            "native" => {
                let name = tokens.get(1).ok_or("`native` needs a name")?.to_string();
                if name == "off" {
                    let target = tokens
                        .get(2)
                        .ok_or("`native off` needs a name")?
                        .to_string();
                    steps.push(Step::NativeOff(target));
                    continue;
                }
                let kind =
                    match tokens.get(2) {
                        Some(&"hal") => ProbeKind::Hal,
                        Some(&"vpio") => ProbeKind::Vpio {
                            params: match tokens.get(3) {
                                Some(spec) if !spec.starts_with("rate=") && *spec != "no-outio" => {
                                    Some(parse_params(spec).ok_or_else(|| {
                                        format!("Unknown params spec \"{}\"", spec)
                                    })?)
                                }
                                _ => None,
                            },
                            rate: match tokens.iter().find(|t| t.starts_with("rate=")) {
                                Some(token) => Some(
                                    token["rate=".len()..]
                                        .parse::<f64>()
                                        .map_err(|e| format!("bad rate: {}", e))?,
                                ),
                                None => None,
                            },
                            disable_output_io: tokens.iter().any(|t| *t == "no-outio"),
                        },
                        _ => return Err("`native <name>` needs `hal` or `vpio`".to_string()),
                    };
                Step::Native { name, kind }
            }
            "probe" => match tokens.get(1) {
                Some(&"on") => Step::Probe(ProbeAction::On),
                Some(&"off") => Step::Probe(ProbeAction::Off),
                Some(&"restart") => Step::Probe(ProbeAction::Restart),
                _ => return Err("`probe` needs `on`, `off` or `restart`".to_string()),
            },
            "measure" => {
                let secs = match tokens.get(1) {
                    Some(s) => Some(
                        s.parse::<f64>()
                            .map_err(|e| format!("bad duration: {}", e))?,
                    ),
                    None => None,
                };
                Step::Measure(secs)
            }
            "sleep" => {
                let secs = tokens
                    .get(1)
                    .ok_or("`sleep` needs a duration")?
                    .parse::<f64>()
                    .map_err(|e| format!("bad duration: {}", e))?;
                Step::Sleep(secs)
            }
            "tone" => {
                let name = tokens
                    .get(1)
                    .ok_or("`tone` needs a stream name")?
                    .to_string();
                let on = match tokens.get(2) {
                    Some(&"on") => true,
                    Some(&"off") => false,
                    _ => return Err("`tone` needs `on` or `off`".to_string()),
                };
                Step::Tone { name, on }
            }
            "probevol" => {
                let value = tokens
                    .get(1)
                    .ok_or("`probevol` needs a value")?
                    .parse::<f32>()
                    .map_err(|e| format!("bad probevol value: {}", e))?;
                let element = match tokens.get(2) {
                    Some(bus) => bus.parse::<u32>().map_err(|e| format!("bad bus: {}", e))?,
                    None => 1,
                };
                Step::ProbeUnitVolume { value, element }
            }
            "volume" => match tokens.get(1) {
                Some(&"?") | None => Step::Volume(None),
                Some(value) => Step::Volume(Some(
                    value
                        .parse::<f32>()
                        .map_err(|e| format!("bad volume: {}", e))?,
                )),
            },
            "note" => Step::Note(tokens[1..].join(" ")),
            "devinfo" => Step::DevInfo,
            other => return Err(format!("Unknown step \"{}\"", other)),
        };
        steps.push(step);
    }
    Ok(steps)
}

/// A capture client set up natively rather than through cubeb.
struct Native {
    name: String,
    probe: InputProbe,
    meter: Arc<Meter>,
}

/// One `measure` step: what was live, what it was for, and what each source received.
struct Measurement {
    index: usize,
    elapsed: f64,
    description: String,
    rows: Vec<(String, String, Report)>,
}

/// Per-stream state reachable from the data callback.
struct StreamCtx {
    name: String,
    meter: Arc<Meter>,
    channels: usize,
    rate: u32,
    /// Toggled from the main thread while the audio callback reads it.
    tone: AtomicBool,
    phase: f64,
}

struct Stream {
    name: String,
    spec: StreamSpec,
    ptr: *mut cubeb_stream,
    meter: Arc<Meter>,
    ctx: Box<StreamCtx>,
    running: bool,
}

extern "C" fn data_callback(
    _stream: *mut cubeb_stream,
    user_ptr: *mut c_void,
    input_buffer: *const c_void,
    output_buffer: *mut c_void,
    nframes: i64,
) -> i64 {
    let ctx = unsafe { &mut *(user_ptr as *mut StreamCtx) };
    let frames = nframes.max(0) as usize;

    if !input_buffer.is_null() && frames > 0 {
        let samples =
            unsafe { slice::from_raw_parts(input_buffer as *const f32, frames * ctx.channels) };
        ctx.meter.add_interleaved(samples, ctx.channels);
    } else {
        ctx.meter.add_empty_callback();
    }

    if !output_buffer.is_null() {
        let samples =
            unsafe { slice::from_raw_parts_mut(output_buffer as *mut f32, frames * ctx.channels) };
        if ctx.tone.load(Ordering::Relaxed) {
            let step = TONE_HZ * std::f64::consts::TAU / f64::from(ctx.rate);
            for frame in samples.chunks_mut(ctx.channels) {
                let value = (ctx.phase.sin() * 0.25) as f32;
                ctx.phase = (ctx.phase + step) % std::f64::consts::TAU;
                frame.fill(value);
            }
        } else {
            samples.fill(0.0);
        }
    }

    nframes
}

extern "C" fn state_callback(_stream: *mut cubeb_stream, user_ptr: *mut c_void, state: u32) {
    let ctx = unsafe { &*(user_ptr as *const StreamCtx) };
    let state = match state {
        CUBEB_STATE_STARTED => "started",
        CUBEB_STATE_STOPPED => "stopped",
        CUBEB_STATE_DRAINED => "drained",
        CUBEB_STATE_ERROR => "ERROR",
        _ => "unknown",
    };
    println!("    [{}] state: {}", ctx.name, state);
}

struct Runner {
    ctx: *mut cubeb,
    streams: Vec<Stream>,
    natives: Vec<Native>,
    device: u32,
    device_uid: CString,
    output_device_uid: Option<CString>,
    rate: u32,
    channels: u32,
    start: Instant,
    default_measure: Duration,
    print_devinfo: bool,
    last_device_snapshot: DeviceSnapshot,
    /// Every measurement made, for the summary table.
    results: Vec<Measurement>,
    /// Set by a `note` step, consumed by the next measurement.
    pending_note: Option<String>,
    /// The device's input volume as found, restored on the way out since it is user-visible state.
    original_volume: Option<f32>,
    step: usize,
}

impl Runner {
    fn new(args: &Args) -> Self {
        let mut ctx: *mut cubeb = ptr::null_mut();
        assert_eq!(CUBEB_OK, unsafe {
            cubeb_coreaudio::audiounit_rust_init(&mut ctx, ptr::null_mut())
        });
        assert_ne!(ctx, ptr::null_mut());

        let mut supported: cubeb_input_processing_params = CUBEB_INPUT_PROCESSING_PARAM_NONE;
        let r = unsafe { cubeb_get_supported_input_processing_params(ctx, &mut supported) };
        println!(
            "Backend supported input processing params: {} (rv {})",
            describe_params(supported),
            r
        );

        let device = match &args.device {
            Some(spec) => match resolve_input_device(spec) {
                Ok(device) => device,
                Err(e) => fail(&e),
            },
            None => default_input_device().expect("no default input device"),
        };
        let device_uid = CString::new(device_uid(device).expect("device has no UID")).unwrap();
        // Report the allocator in effect: a run built with --features zeroing-alloc is otherwise
        // indistinguishable from a default one in the output, which makes pasted results ambiguous.
        println!(
            "Allocator: {}{}",
            if cfg!(feature = "zeroing-alloc") {
                "Rust allocations zeroed (--features zeroing-alloc)"
            } else {
                "Rust default, not zeroed"
            },
            match std::env::var("DYLD_INSERT_LIBRARIES") {
                Ok(libs) if !libs.is_empty() => format!(", DYLD_INSERT_LIBRARIES={}", libs),
                _ => ", no DYLD_INSERT_LIBRARIES".to_string(),
            }
        );
        println!("Requesting {} Hz, {} ch for the cubeb streams", args.rate, args.channels);

        // Duplex streams need an output device. Naming it explicitly matters here: with the
        // default output the VPIO unit may end up pairing the built-in mic with an unrelated
        // device, which is not the configuration the bug report describes.
        let output_device = match &args.output_device {
            Some(spec) => match resolve_output_device(spec) {
                Ok(device) => Some(device),
                Err(e) => fail(&e),
            },
            None => default_output_device(),
        };
        // `device_uid` the function is shadowed by the local of the same name above.
        let output_device_uid = output_device
            .and_then(cubeb_coreaudio_samples::devinfo::device_uid)
            .map(|uid| CString::new(uid).unwrap());
        match (output_device, &output_device_uid) {
            (Some(device), Some(uid)) => println!(
                "Output device (duplex streams): {} \"{}\" (uid {:?}){}",
                device,
                device_name(device),
                uid,
                if args.output_device.is_some() {
                    ""
                } else {
                    " [system default]"
                }
            ),
            _ => println!("Output device (duplex streams): none resolved"),
        }
        println!("Input device: {} \"{}\" (uid {:?})", device, device_name(device), device_uid);

        let snapshot = DeviceSnapshot::capture(device);
        if args.devinfo {
            println!("Device state at start:\n{}", snapshot.describe());
        }

        Self {
            ctx,
            streams: Vec::new(),
            natives: Vec::new(),
            device,
            device_uid,
            output_device_uid,
            rate: args.rate,
            channels: args.channels,
            start: Instant::now(),
            default_measure: Duration::from_secs_f64(args.measure),
            print_devinfo: args.devinfo,
            last_device_snapshot: snapshot,
            results: Vec::new(),
            pending_note: None,
            original_volume: None,
            step: 0,
        }
    }

    fn elapsed(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn run(&mut self, step: Step) {
        self.step += 1;
        match step {
            Step::Open { name, spec } => self.open(name, spec),
            Step::Close(name) => self.close(&name),
            Step::Stop(name) => self.set_running(&name, false),
            Step::Start(name) => self.set_running(&name, true),
            Step::Params { name, params, spec } => self.set_params(&name, params, &spec),
            Step::Probe(action) => self.set_probe(action),
            Step::Native { name, kind } => self.open_native(name, kind),
            Step::NativeOff(name) => {
                println!("[{:6.1}s] native off {}", self.elapsed(), name);
                self.natives.retain(|n| n.name != name);
            }
            Step::Tone { name, on } => {
                println!(
                    "[{:6.1}s] tone {} {}",
                    self.elapsed(),
                    name,
                    if on { "on" } else { "off" }
                );
                self.find(&name).ctx.tone.store(on, Ordering::Relaxed);
            }
            Step::Measure(secs) => {
                let duration = secs
                    .map(Duration::from_secs_f64)
                    .unwrap_or(self.default_measure);
                self.measure(duration);
            }
            Step::Sleep(secs) => {
                println!("[{:6.1}s] sleep {}s", self.elapsed(), secs);
                thread::sleep(Duration::from_secs_f64(secs));
            }
            Step::Note(text) => self.pending_note = Some(text),
            Step::Volume(scalar) => self.volume(scalar),
            Step::ProbeUnitVolume { value, element } => {
                let result = match self.natives.iter().find(|n| n.name == "probe") {
                    Some(native) => native.probe.set_unit_volume(value, element),
                    None => {
                        println!("[{:6.1}s] probevol: no probe running", self.elapsed());
                        return;
                    }
                };
                println!(
                    "[{:6.1}s] probevol {} on bus {}: {}",
                    self.elapsed(),
                    value,
                    element,
                    match result {
                        Ok(()) => "set".to_string(),
                        Err(e) => format!("failed, err {}", e),
                    }
                );
            }
            Step::Cycle { count, spec } => self.cycle(count, spec),
            Step::DevInfo => {
                let snapshot = DeviceSnapshot::capture(self.device);
                println!("[{:6.1}s] device state:\n{}", self.elapsed(), snapshot.describe());
                self.last_device_snapshot = snapshot;
            }
        }
    }

    /// Churn VPIO units: open and immediately close a stream `count` times. Back-to-back cycles
    /// reuse the pooled unit, so a `sleep` past VPIO_IDLE_TIMEOUT between `cycle` steps is what
    /// makes units actually get disposed of and recreated.
    fn cycle(&mut self, count: usize, spec: StreamSpec) {
        println!("[{:6.1}s] cycle {} x ({}) ...", self.elapsed(), count, spec.text);
        for i in 0..count {
            let name = format!("cycle{}", i);
            self.open_inner(name.clone(), spec.clone(), false);
            thread::sleep(Duration::from_millis(250));
            self.close_inner(&name, false);
        }
        println!("    {} cycles done", count);
    }

    fn open(&mut self, name: String, spec: StreamSpec) {
        self.open_inner(name, spec, true);
    }

    fn open_inner(&mut self, name: String, spec: StreamSpec, verbose: bool) {
        if verbose {
            println!(
                "[{:6.1}s] open {} ({}) [device at {} ch]",
                self.elapsed(),
                name,
                spec.text,
                input_channels(self.device)
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            );
        }
        assert!(!self.streams.iter().any(|s| s.name == name), "duplicate stream name");

        let channels = spec.channels.unwrap_or(self.channels);
        // cubeb rejects a request for more channels than it counts on the device, and with the
        // audio-dump feature its teardown path then asserts and aborts the process, so check first.
        if let Some(available) = cubeb_input_channel_count(self.device) {
            if channels > available {
                println!(
                    "    [{}] SKIPPED: asked for {} channels but cubeb counts {} on this device, \
                     which would fail stream_init",
                    name, channels, available
                );
                return;
            }
        }
        let meter = Arc::new(Meter::new(name.clone()));
        let mut ctx = Box::new(StreamCtx {
            name: name.clone(),
            meter: meter.clone(),
            channels: channels as usize,
            rate: self.rate,
            tone: AtomicBool::new(spec.tone),
            phase: 0.0,
        });

        let prefs = if spec.voice {
            CUBEB_STREAM_PREF_VOICE
        } else {
            CUBEB_STREAM_PREF_NONE
        };
        let mut input_params = cubeb_stream_params {
            channels,
            format: CUBEB_SAMPLE_FLOAT32NE,
            rate: self.rate,
            layout: if channels == 1 {
                CUBEB_LAYOUT_MONO
            } else {
                CUBEB_LAYOUT_UNDEFINED
            },
            prefs,
            input_params: CUBEB_INPUT_PROCESSING_PARAM_NONE,
        };
        let mut output_params = input_params;

        let mut stream: *mut cubeb_stream = ptr::null_mut();
        let stream_name = CString::new(name.clone()).unwrap();
        let user_ptr = ctx.as_mut() as *mut StreamCtx as *mut c_void;
        // A cubeb-coreaudio devid is the device's UID as a C string. Naming the input device
        // explicitly keeps the cubeb streams and the plain probe on the same device.
        // Held until cubeb_stream_init returns, which reads the UID strings synchronously.
        let input_override = spec.input.as_deref().map(|spec| resolve_uid(spec, true));
        let output_override = spec.output.as_deref().map(|spec| resolve_uid(spec, false));
        let input_device = match &input_override {
            Some(uid) => uid.as_ptr() as cubeb_devid,
            None => self.device_uid.as_ptr() as cubeb_devid,
        };
        let output_device = match (&output_override, &self.output_device_uid) {
            (Some(uid), _) if spec.duplex => uid.as_ptr() as cubeb_devid,
            (None, Some(uid)) if spec.duplex => uid.as_ptr() as cubeb_devid,
            _ => ptr::null_mut(),
        };
        let r = unsafe {
            cubeb_stream_init(
                self.ctx,
                &mut stream,
                stream_name.as_ptr(),
                input_device,
                &mut input_params,
                output_device,
                if spec.duplex {
                    &mut output_params
                } else {
                    ptr::null_mut()
                },
                LATENCY_FRAMES,
                Some(data_callback),
                Some(state_callback),
                user_ptr,
            )
        };
        assert_eq!(CUBEB_OK, r, "cubeb_stream_init failed for {}", name);

        // Processing params must be applied before the stream is started: the backend defers
        // anything set while the units are running to the next start.
        if let Some(params) = spec.params {
            let r = unsafe { cubeb_stream_set_input_processing_params(stream, params) };
            if r == CUBEB_OK {
                println!("    [{}] processing params set to {}", name, describe_params(params));
            } else {
                println!(
                    "    [{}] WARNING: could not set processing params {} (rv {})",
                    name,
                    describe_params(params),
                    r
                );
            }
        }

        assert_eq!(CUBEB_OK, unsafe { cubeb_stream_start(stream) });

        // Wait for the stream to actually deliver input before measuring it. A VPIO unit can take
        // a while to produce its first buffer, and a measurement window that starts too early
        // reports an artificially low level, or nothing at all.
        let start = Instant::now();
        let mut first_input = None;
        while verbose && start.elapsed() < FIRST_INPUT_TIMEOUT {
            if meter.snapshot().frames > 0 {
                first_input = Some(start.elapsed());
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        match first_input {
            Some(latency) => {
                println!("    [{}] first input after {} ms", name, latency.as_millis())
            }
            None if verbose => println!(
                "    [{}] WARNING: no input within {:.0}s of starting",
                name,
                FIRST_INPUT_TIMEOUT.as_secs_f64()
            ),
            None => {}
        }

        self.streams.push(Stream {
            name,
            spec,
            ptr: stream,
            meter,
            ctx,
            running: true,
        });
    }

    fn find(&mut self, name: &str) -> &mut Stream {
        match self.streams.iter_mut().find(|s| s.name == name) {
            Some(stream) => stream,
            None => panic!("no such stream: {}", name),
        }
    }

    fn close(&mut self, name: &str) {
        self.close_inner(name, true);
    }

    fn close_inner(&mut self, name: &str, verbose: bool) {
        if verbose {
            println!("[{:6.1}s] close {}", self.elapsed(), name);
        }
        let index = self
            .streams
            .iter()
            .position(|s| s.name == name)
            .unwrap_or_else(|| panic!("no such stream: {}", name));
        let stream = self.streams.remove(index);
        if stream.running {
            assert_eq!(CUBEB_OK, unsafe { cubeb_stream_stop(stream.ptr) });
        }
        unsafe { cubeb_stream_destroy(stream.ptr) };
        // The callback context outlives the stream by construction; drop it after destroy.
        drop(stream.ctx);
    }

    fn set_running(&mut self, name: &str, running: bool) {
        println!("[{:6.1}s] {} {}", self.elapsed(), if running { "start" } else { "stop" }, name);
        let stream = self.find(name);
        if stream.running == running {
            println!("    [{}] already {}", name, if running { "started" } else { "stopped" });
            return;
        }
        let r = if running {
            unsafe { cubeb_stream_start(stream.ptr) }
        } else {
            unsafe { cubeb_stream_stop(stream.ptr) }
        };
        assert_eq!(CUBEB_OK, r);
        stream.running = running;
    }

    fn set_params(&mut self, name: &str, params: cubeb_input_processing_params, spec: &str) {
        println!("[{:6.1}s] params {} {}", self.elapsed(), name, spec);
        let stream = self.find(name);
        let r = unsafe { cubeb_stream_set_input_processing_params(stream.ptr, params) };
        if r != CUBEB_OK {
            println!("    [{}] WARNING: set_input_processing_params rv {}", name, r);
            return;
        }
        if stream.running {
            println!(
                "    [{}] set to {} -- deferred by the backend until the stream restarts",
                name,
                describe_params(params)
            );
        }
        stream.spec.params = Some(params);
        stream.spec.text = format!("{}, params {}", stream.spec.text, describe_params(params));
    }

    fn set_probe(&mut self, action: ProbeAction) {
        println!("[{:6.1}s] probe {:?}", self.elapsed(), action);
        if action == ProbeAction::Off {
            self.natives.retain(|n| n.name != "probe");
            return;
        }
        if action == ProbeAction::Restart {
            self.natives.retain(|n| n.name != "probe");
        } else if self.natives.iter().any(|n| n.name == "probe") {
            println!("    probe already running");
            return;
        }
        self.open_native("probe".to_string(), ProbeKind::Hal);
    }

    /// Open a capture client set up natively, without cubeb. The plain kind stands in for another
    /// app on the same device; the VPIO kind separates what Apple's voice processing does from what
    /// cubeb's configuration of it does.
    fn open_native(&mut self, name: String, kind: ProbeKind) {
        let description = match kind {
            ProbeKind::Hal => "plain CoreAudio client, no cubeb".to_string(),
            ProbeKind::Vpio {
                params,
                rate,
                disable_output_io,
            } => format!(
                "native VPIO, no cubeb, params {}{}{}",
                match params {
                    Some(p) => describe_params(p),
                    None => "left at the unit's defaults".to_string(),
                },
                match rate {
                    Some(rate) => format!(", format rate set to {} as WebKit does", rate),
                    None => ", format left as the unit advertises it".to_string(),
                },
                if disable_output_io {
                    ", output IO disabled as cubeb does"
                } else {
                    ", output IO untouched as WebKit does"
                }
            ),
        };
        if name != "probe" {
            println!("[{:6.1}s] native {} ({})", self.elapsed(), name, description);
        }
        if self.natives.iter().any(|n| n.name == name) {
            println!("    [{}] already running", name);
            return;
        }
        let meter = Arc::new(Meter::new(name.clone()));
        match InputProbe::with_kind(self.device, meter.clone(), kind) {
            Ok(probe) => {
                if let Err(e) = probe.start() {
                    println!("    WARNING: could not start {}: {}", name, e);
                    return;
                }
                println!(
                    "    {} running at {} Hz, {} ch",
                    description,
                    probe.rate(),
                    probe.channels()
                );
                self.natives.push(Native { name, probe, meter });
            }
            Err(e) => println!("    WARNING: could not create {}: {}", name, e),
        }
    }

    fn volume(&mut self, scalar: Option<f32>) {
        let current = knobs::get_input_volume(self.device);
        match (scalar, current) {
            (_, None) => {
                println!("[{:6.1}s] volume: device exposes no input volume control", self.elapsed())
            }
            (None, Some((scalar, db))) => {
                println!("[{:6.1}s] volume is {} ({:.2} dB)", self.elapsed(), scalar, db)
            }
            (Some(target), Some((scalar, db))) => {
                if self.original_volume.is_none() {
                    self.original_volume = Some(scalar);
                }
                match knobs::set_input_volume(self.device, target) {
                    Ok(()) => {
                        let now = knobs::get_input_volume(self.device);
                        println!(
                            "[{:6.1}s] volume {} ({:.2} dB) -> {}",
                            self.elapsed(),
                            scalar,
                            db,
                            now.map(|(s, d)| format!("{} ({:.2} dB)", s, d))
                                .unwrap_or_else(|| "unknown".to_string())
                        );
                    }
                    Err(e) => {
                        println!("[{:6.1}s] volume: could not set (err {})", self.elapsed(), e)
                    }
                }
            }
        }
    }

    fn measure(&mut self, duration: Duration) {
        // A `note` step says what this measurement is for. Without one, fall back to listing what
        // is capturing, so a step number is never the only thing identifying a measurement.
        let description = self.pending_note.take().unwrap_or_else(|| {
            let mut live: Vec<String> = self.streams.iter().map(|s| s.name.clone()).collect();
            live.extend(self.natives.iter().map(|n| n.name.clone()));
            if live.is_empty() {
                "nothing capturing".to_string()
            } else {
                format!("live: {}", live.join(", "))
            }
        });
        let elapsed = self.elapsed();
        println!(
            "[{:6.1}s] measure #{}, {:.1}s ...",
            elapsed,
            self.results.len() + 1,
            duration.as_secs_f64()
        );
        println!("    ── {}", description);

        let mut before: Vec<(String, Snapshot)> = self
            .streams
            .iter()
            .map(|s| (s.name.clone(), s.meter.snapshot()))
            .collect();
        before.extend(
            self.natives
                .iter()
                .map(|n| (n.name.clone(), n.meter.snapshot())),
        );

        thread::sleep(duration);

        let mut rows = Vec::new();
        for (name, snapshot) in &before {
            let meter = match self.natives.iter().find(|n| &n.name == name) {
                Some(native) => &native.meter,
                None => {
                    &self
                        .streams
                        .iter()
                        .find(|s| &s.name == name)
                        .expect("stream vanished mid-measurement")
                        .meter
                }
            };
            let report = snapshot.delta(&meter.snapshot());
            let spec = match self.streams.iter().find(|s| &s.name == name) {
                Some(stream) => stream.spec.text.clone(),
                None => match self
                    .natives
                    .iter()
                    .find(|n| &n.name == name)
                    .map(|n| n.probe.kind())
                {
                    Some(ProbeKind::Vpio { params, .. }) => format!(
                        "native VPIO, no cubeb, params {}",
                        match params {
                            Some(p) => describe_params(p),
                            None => "unit defaults".to_string(),
                        }
                    ),
                    _ => "plain CoreAudio client, no cubeb".to_string(),
                },
            };
            println!("    {:<10} {}", name, report);
            println!("    {:<10} └─ {}", "", spec);
            rows.push((name.clone(), spec, report));
            if let Some(native) = self.natives.iter().find(|n| &n.name == name) {
                // A native client's format was fixed when it was created. If the device has since
                // changed its channel count, the HAL is converting, and comparing this level to a
                // measurement from before the change is not apples to apples.
                if let Some(now) = input_channels(self.device) {
                    if now as usize != native.probe.channels()
                        && native.probe.kind() == ProbeKind::Hal
                    {
                        println!(
                            "    {:<10} └─ NOTE: device now has {} input channels but the probe was \
                             opened with {}; the HAL is converting",
                            "", now, native.probe.channels()
                        );
                    }
                }
            }
        }
        self.results.push(Measurement {
            index: self.results.len() + 1,
            elapsed,
            description,
            rows,
        });
        if before.is_empty() {
            println!("    (nothing capturing)");
        }

        let snapshot = DeviceSnapshot::capture(self.device);
        if self.print_devinfo {
            println!("    device state:\n{}", snapshot.describe());
        } else {
            let diffs = snapshot.diff(&self.last_device_snapshot);
            if !diffs.is_empty() {
                println!("    device property changes since last measurement:");
                for diff in diffs {
                    println!("      {}", diff);
                }
            }
        }
        self.last_device_snapshot = snapshot;
    }

    fn finish(&mut self) {
        if let Some(scalar) = self.original_volume.take() {
            match knobs::set_input_volume(self.device, scalar) {
                Ok(()) => println!("Restored the device input volume to {}", scalar),
                Err(e) => {
                    println!("WARNING: could not restore input volume to {} (err {})", scalar, e)
                }
            }
        }
        let names: Vec<String> = self.streams.iter().map(|s| s.name.clone()).collect();
        for name in names {
            self.close(&name);
        }
        self.natives.clear();

        if !self.results.is_empty() {
            println!("\nSummary (rms of the loudest channel, dBFS):");
            for measurement in &self.results {
                println!(
                    "\n  measurement {} @ {:.1}s ── {}",
                    measurement.index, measurement.elapsed, measurement.description
                );
                // The room is never perfectly steady, so when a plain client is running alongside,
                // its level is the reference that makes rows comparable across measurements.
                let reference = measurement
                    .rows
                    .iter()
                    .find(|(name, _, _)| name == "probe")
                    .map(|(_, _, report)| report.loudest().1)
                    .filter(|rms| rms.is_finite());
                for (name, spec, report) in &measurement.rows {
                    let (_, rms) = report.loudest();
                    let delta = match (reference, name.as_str()) {
                        (Some(reference), name) if name != "probe" && rms.is_finite() => {
                            format!("{:+.1} vs probe", rms - reference)
                        }
                        _ => String::new(),
                    };
                    println!(
                        "    {:<10} {:>8} {:>14}  {:<16} {}",
                        name,
                        fmt_dbfs(rms),
                        delta,
                        if report.frames == 0 {
                            "no input".to_string()
                        } else if report.digital_silence() {
                            "digital silence".to_string()
                        } else {
                            format!("peak {}", fmt_dbfs(report.peak_dbfs))
                        },
                        spec
                    );
                }
            }
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        unsafe { cubeb_destroy(self.ctx) };
        unsafe { cubeb_set_log_callback(CUBEB_LOG_DISABLED, None) };
    }
}
