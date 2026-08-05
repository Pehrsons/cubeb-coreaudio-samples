//! Level metering for captured audio, in dBFS.
//!
//! A [`Meter`] is written to from an audio callback and read from another thread. All state is
//! cumulative and monotonic so a reader can snapshot and diff two points in time without having to
//! synchronize with the (single) writer.

use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub const MAX_CHANNELS: usize = 8;

// Sums of squares are accumulated as fixed point integers so they can be published with a single
// atomic add. A sample in [-1, 1] contributes at most this much, and f64 -> u64 keeps ~19 digits,
// so this leaves room for many hours of audio before overflowing.
const SUM_SCALE: f64 = 1e9;

#[derive(Default)]
pub struct Meter {
    label: String,
    channels: AtomicUsize,
    sums: [AtomicU64; MAX_CHANNELS],
    frames: AtomicU64,
    callbacks: AtomicU64,
    // Frames where every channel was exactly zero, to tell digital silence from a quiet signal.
    silent_frames: AtomicU64,
    peak: AtomicU64,
}

impl Meter {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ..Default::default()
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Accumulate interleaved samples. Called from the audio callback.
    pub fn add_interleaved(&self, data: &[f32], channels: usize) {
        assert!(channels > 0);
        let channels_capped = channels.min(MAX_CHANNELS);
        self.channels.store(channels, Ordering::Relaxed);

        let mut sums = [0f64; MAX_CHANNELS];
        let mut peak = 0f32;
        let mut silent = 0u64;
        for frame in data.chunks_exact(channels) {
            let mut frame_silent = true;
            for (ch, sample) in frame.iter().take(channels_capped).enumerate() {
                let sample = *sample;
                sums[ch] += f64::from(sample) * f64::from(sample);
                peak = peak.max(sample.abs());
                if sample != 0.0 {
                    frame_silent = false;
                }
            }
            if frame_silent {
                silent += 1;
            }
        }

        for (ch, sum) in sums.iter().take(channels_capped).enumerate() {
            self.sums[ch].fetch_add((sum * SUM_SCALE) as u64, Ordering::Relaxed);
        }
        self.frames
            .fetch_add((data.len() / channels) as u64, Ordering::Relaxed);
        self.callbacks.fetch_add(1, Ordering::Relaxed);
        self.silent_frames.fetch_add(silent, Ordering::Relaxed);
        self.peak
            .fetch_max(f64::from(peak).to_bits(), Ordering::Relaxed);
    }

    /// Count a callback that delivered no input at all, to distinguish "not running" from
    /// "running but silent".
    pub fn add_empty_callback(&self) {
        self.callbacks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut sums = [0u64; MAX_CHANNELS];
        for (ch, sum) in sums.iter_mut().enumerate() {
            *sum = self.sums[ch].load(Ordering::Relaxed);
        }
        Snapshot {
            channels: self.channels.load(Ordering::Relaxed),
            sums,
            frames: self.frames.load(Ordering::Relaxed),
            callbacks: self.callbacks.load(Ordering::Relaxed),
            silent_frames: self.silent_frames.load(Ordering::Relaxed),
            peak: f64::from_bits(self.peak.load(Ordering::Relaxed)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    channels: usize,
    sums: [u64; MAX_CHANNELS],
    pub frames: u64,
    callbacks: u64,
    silent_frames: u64,
    peak: f64,
}

impl Snapshot {
    /// The audio seen between `self` (the earlier snapshot) and `later`.
    pub fn delta(&self, later: &Snapshot) -> Report {
        let frames = later.frames.saturating_sub(self.frames);
        let channels = later.channels.max(1).min(MAX_CHANNELS);
        let mut rms_dbfs = [f64::NEG_INFINITY; MAX_CHANNELS];
        for ch in 0..channels {
            let sum = later.sums[ch].saturating_sub(self.sums[ch]) as f64 / SUM_SCALE;
            if frames > 0 {
                rms_dbfs[ch] = to_dbfs((sum / frames as f64).sqrt());
            }
        }
        Report {
            channels,
            rms_dbfs,
            frames,
            callbacks: later.callbacks.saturating_sub(self.callbacks),
            silent_frames: later.silent_frames.saturating_sub(self.silent_frames),
            // Peak is cumulative-max, so a delta is only exact for the total. Over a measurement
            // window it is the loudest sample since the meter was created; good enough as a
            // clipping/level sanity check next to the rms figure.
            peak_dbfs: to_dbfs(later.peak),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Report {
    pub channels: usize,
    pub rms_dbfs: [f64; MAX_CHANNELS],
    pub frames: u64,
    pub callbacks: u64,
    pub silent_frames: u64,
    pub peak_dbfs: f64,
}

impl Report {
    /// The loudest channel and its rms level, which is what we compare across configurations. The
    /// built-in mic is exposed with several channels on some Macs, not all of them carrying signal.
    pub fn loudest(&self) -> (usize, f64) {
        let mut best = (0, f64::NEG_INFINITY);
        for ch in 0..self.channels {
            if self.rms_dbfs[ch] > best.1 {
                best = (ch, self.rms_dbfs[ch]);
            }
        }
        best
    }

    pub fn digital_silence(&self) -> bool {
        self.frames > 0 && self.silent_frames == self.frames
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.frames == 0 {
            return write!(f, "no input ({} callbacks)", self.callbacks);
        }
        let (ch, rms) = self.loudest();
        write!(
            f,
            "rms {:>7} peak {:>7} (ch {}/{}, {} frames, {} cb{})",
            fmt_dbfs(rms),
            fmt_dbfs(self.peak_dbfs),
            ch,
            self.channels,
            self.frames,
            self.callbacks,
            if self.digital_silence() {
                ", DIGITAL SILENCE"
            } else {
                ""
            }
        )?;
        // With more than one channel the distribution is the interesting part: the built-in mic's
        // raw array does not carry the same level on every channel.
        if self.channels > 1 {
            write!(f, " per-channel:")?;
            for ch in 0..self.channels {
                write!(f, " [{}] {}", ch, fmt_dbfs(self.rms_dbfs[ch]))?;
            }
        }
        Ok(())
    }
}

pub fn to_dbfs(amplitude: f64) -> f64 {
    if amplitude <= 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * amplitude.log10()
    }
}

pub fn fmt_dbfs(dbfs: f64) -> String {
    if dbfs.is_infinite() {
        "-inf".to_string()
    } else {
        format!("{:.1}", dbfs)
    }
}
