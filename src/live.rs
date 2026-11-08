//! Live inputs shared by the screensaver scenes and the extras buttons: a camera feed
//! (ffmpeg → raw pictures), the webcam service's preview file, an audio feed
//! (pw-record → samples) and the FFT spectrum.
//!
//! Each feed is a child process plus a reader thread keeping only the latest data; dropping
//! the feed kills the process.

// Pixel and sample maths is f32; every float-to-int cast is clamped into range first.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Instant, SystemTime};

const FFT_SIZE: usize = 2048;
const SAMPLE_RATE: f32 = 48_000.0;
const LOWEST_HZ: f32 = 30.0;
const HIGHEST_HZ: f32 = 18_000.0;
/// Level mapping: this many dB below full scale is an empty bar, `TOP_DB` a full one.
const FLOOR_DB: f32 = -80.0;
const TOP_DB: f32 = -20.0;
/// The built-in camera (H.264 only), and the browser-compatible loopback that
/// `t1-webcam.service` feeds while it holds the camera.
const CAMERA_DEVICE: &str = "/dev/v4l/by-id/usb-Apple_Inc._iBridge-video-index0";
const CAMERA_LOOPBACK: &str = "/dev/video10";
/// In `$XDG_RUNTIME_DIR`: see [`CameraPreview`].
const CAMERA_PREVIEW: &str = "t1-webcam-preview.bgr0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AudioSource {
    /// The default source: the microphone, or line in when that is the default.
    Input,
    /// The default sink's monitor: whatever is playing.
    Speakers,
}

/// FFT state and smoothed bar levels for a spectrum display.
pub(crate) struct Spectrum {
    window: Vec<f32>,
    twiddles: Vec<[f32; 2]>,
    data: Vec<[f32; 2]>,
    /// FFT bin range per bar.
    pub(crate) bands: Vec<(usize, usize)>,
    pub(crate) targets: Vec<f32>,
    pub(crate) levels: Vec<f32>,
    pub(crate) peaks: Vec<f32>,
    last: Instant,
}

impl Spectrum {
    pub(crate) fn new(bars: usize) -> Self {
        let bars = bars.max(1);
        let size = FFT_SIZE as f32;
        let bin = |hz: f32| (hz * size / SAMPLE_RATE) as usize;
        let bands = (0..bars)
            .map(|bar| {
                let edge =
                    |i: usize| LOWEST_HZ * (HIGHEST_HZ / LOWEST_HZ).powf(i as f32 / bars as f32);
                let low = bin(edge(bar)).max(1);
                (low, bin(edge(bar + 1)).max(low + 1).min(FFT_SIZE / 2))
            })
            .collect();
        Self {
            window: (0..FFT_SIZE)
                .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size).cos())
                .collect(),
            twiddles: (0..FFT_SIZE / 2)
                .map(|k| {
                    let angle = -std::f32::consts::TAU * k as f32 / size;
                    [angle.cos(), angle.sin()]
                })
                .collect(),
            data: vec![[0.0; 2]; FFT_SIZE],
            bands,
            targets: vec![0.0; bars],
            levels: vec![0.0; bars],
            peaks: vec![0.0; bars],
            last: Instant::now(),
        }
    }

    pub(crate) fn analyse(&mut self, samples: &[f32]) {
        for ((slot, sample), weight) in self.data.iter_mut().zip(samples).zip(&self.window) {
            *slot = [sample * weight, 0.0];
        }
        fft(&mut self.data, &self.twiddles);
        // Hann window sums to N/2, so a full-scale sine peaks at amplitude 1.0 (0 dB).
        let scale = 4.0 / FFT_SIZE as f32;
        for (target, &(low, high)) in self.targets.iter_mut().zip(&self.bands) {
            let magnitude = self.data[low..high]
                .iter()
                .map(|[re, im]| (re * re + im * im).sqrt())
                .fold(0.0, f32::max);
            let db = 20.0 * (magnitude * scale).max(1e-9).log10();
            *target = ((db - FLOOR_DB) / (TOP_DB - FLOOR_DB)).clamp(0.0, 1.0);
        }
    }

    /// Bars jump up and fall at a steady rate; peak markers fall more slowly.
    pub(crate) fn settle(&mut self) {
        let dt = self.last.elapsed().as_secs_f32().min(0.1);
        self.last = Instant::now();
        for ((level, peak), &target) in self
            .levels
            .iter_mut()
            .zip(&mut self.peaks)
            .zip(&self.targets)
        {
            *level = target.max(*level - 2.5 * dt);
            *peak = level.max(*peak - 0.4 * dt);
        }
    }
}

/// In-place iterative radix-2 FFT; `data.len()` is a power of two.
fn fft(data: &mut [[f32; 2]], twiddles: &[[f32; 2]]) {
    let size = data.len();
    let mut j = 0;
    for i in 1..size {
        let mut bit = size >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            data.swap(i, j);
        }
    }
    let mut length = 2;
    while length <= size {
        let half = length / 2;
        let step = size / length;
        for start in (0..size).step_by(length) {
            for k in 0..half {
                let [wr, wi] = twiddles[k * step];
                let [ar, ai] = data[start + k];
                let [br, bi] = data[start + k + half];
                let (tr, ti) = (br * wr - bi * wi, br * wi + bi * wr);
                data[start + k] = [ar + tr, ai + ti];
                data[start + k + half] = [ar - tr, ai - ti];
            }
        }
        length <<= 1;
    }
}

/// `pw-record` (mono f32 at 48 kHz) into a rolling window of the latest `FFT_SIZE`
/// samples. Only alive while something shows it.
pub(crate) struct AudioFeed {
    pub(crate) source: AudioSource,
    child: Child,
    pub(crate) samples: Arc<Mutex<Vec<f32>>>,
}

impl AudioFeed {
    pub(crate) fn start(source: AudioSource) -> Option<Self> {
        let mut command = Command::new("pw-record");
        if source == AudioSource::Speakers {
            command.args(["-P", "{ stream.capture.sink = true }"]);
        }
        let spawned = command
            .args([
                "--raw",
                "--rate",
                "48000",
                "--channels",
                "1",
                "--format",
                "f32",
                "--latency",
                "10ms",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                eprintln!("fnstrip: could not run pw-record: {error}");
                return None;
            }
        };
        let mut stdout = child.stdout.take()?;
        let samples = Arc::new(Mutex::new(vec![0.0; FFT_SIZE]));
        let shared = Arc::clone(&samples);
        thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            let mut pending = Vec::new();
            loop {
                let read = match stdout.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                pending.extend_from_slice(&buffer[..read]);
                let whole = pending.len() / 4 * 4;
                let fresh: Vec<f32> = pending[..whole]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|bytes| f32::from_ne_bytes(*bytes))
                    .collect();
                pending.drain(..whole);
                let Ok(mut window) = shared.lock() else {
                    break;
                };
                let keep = fresh.len().min(FFT_SIZE);
                window.drain(..keep);
                window.extend_from_slice(&fresh[fresh.len() - keep..]);
            }
        });
        Some(Self {
            source,
            child,
            samples,
        })
    }
}

impl AudioFeed {
    /// The capture process has exited (device gone, or it never started properly).
    pub(crate) fn ended(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for AudioFeed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `ffmpeg` decoding the camera into raw XRGB8888 pictures of exactly `width`x`height`
/// (mirrored, cropped to that shape, then scaled). Only alive while something shows it;
/// the camera light is on meanwhile.
pub(crate) struct CameraFeed {
    child: Child,
    /// The latest whole picture; empty until the first arrives.
    pub(crate) frame: Arc<Mutex<Vec<u8>>>,
}

impl CameraFeed {
    pub(crate) fn start(width: usize, height: usize) -> Option<Self> {
        // The camera can only be opened once: while the browser webcam service runs, read
        // its loopback copy instead.
        let shared_by_webcam = Command::new("systemctl")
            .args(["--user", "-q", "is-active", "t1-webcam.service"])
            .status()
            .is_ok_and(|status| status.success());
        let mut command = Command::new("ffmpeg");
        command.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        command.args(["-fflags", "nobuffer", "-flags", "low_delay"]);
        if shared_by_webcam {
            command.args(["-i", CAMERA_LOOPBACK]);
        } else {
            command.args([
                "-f",
                "v4l2",
                "-input_format",
                "h264",
                "-video_size",
                "1280x720",
                "-framerate",
                "30",
                "-i",
                CAMERA_DEVICE,
            ]);
        }
        let spawned = command
            .args([
                "-vf",
                &format!("hflip,crop=iw:iw*{height}/{width},scale={width}:{height}"),
                "-pix_fmt",
                "bgr0",
                "-f",
                "rawvideo",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                eprintln!("fnstrip: could not run ffmpeg: {error}");
                return None;
            }
        };
        eprintln!(
            "fnstrip: camera {width}x{height} from {}",
            if shared_by_webcam {
                CAMERA_LOOPBACK
            } else {
                CAMERA_DEVICE
            }
        );
        let mut stdout = child.stdout.take()?;
        let frame = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&frame);
        thread::spawn(move || {
            let mut incoming = vec![0_u8; width * height * 4];
            while stdout.read_exact(&mut incoming).is_ok() {
                let Ok(mut latest) = shared.lock() else {
                    break;
                };
                if latest.len() == incoming.len() {
                    std::mem::swap(&mut *latest, &mut incoming);
                } else {
                    latest.clone_from(&incoming);
                }
            }
        });
        Some(Self { child, frame })
    }
}

impl Drop for CameraFeed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The small mirrored preview that `t1-webcam.service` writes while it runs (a second
/// ffmpeg output, `$XDG_RUNTIME_DIR/t1-webcam-preview.bgr0`): raw XRGB8888 at the extras
/// plate size, replaced whole (atomically) about 12 times a second. Unlike opening the
/// loopback device, reading it works while a browser call has the webcam.
pub(crate) struct CameraPreview {
    path: Option<PathBuf>,
    modified: Option<SystemTime>,
    /// The latest picture; empty until one of the expected size has been read.
    pub(crate) frame: Vec<u8>,
}

impl CameraPreview {
    pub(crate) fn new() -> Self {
        Self {
            path: std::env::var_os("XDG_RUNTIME_DIR")
                .map(|directory| PathBuf::from(directory).join(CAMERA_PREVIEW)),
            modified: None,
            frame: Vec::new(),
        }
    }

    /// Re-reads the file when it has changed; keeps it only if it is `bytes` long.
    pub(crate) fn refresh(&mut self, bytes: usize) {
        let Some(path) = &self.path else {
            return;
        };
        let modified = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok();
        if modified.is_none() || modified == self.modified {
            return;
        }
        self.modified = modified;
        if let Ok(frame) = fs::read(path)
            && frame.len() == bytes
        {
            self.frame = frame;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_finds_a_pure_tone() {
        let mut spectrum = Spectrum::new(108);
        let hz = 1_000.0;
        let tone: Vec<f32> = (0..FFT_SIZE)
            .map(|i| (std::f32::consts::TAU * hz * i as f32 / SAMPLE_RATE).sin() * 0.01)
            .collect();
        spectrum.analyse(&tone);
        let loudest = spectrum
            .targets
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(bar, _)| bar)
            .unwrap();
        let (low, high) = spectrum.bands[loudest];
        let bin = (hz * FFT_SIZE as f32 / SAMPLE_RATE) as usize;
        assert!(
            (low..high + 1).contains(&bin),
            "bar {loudest} covers {low}..{high}"
        );
    }
}
