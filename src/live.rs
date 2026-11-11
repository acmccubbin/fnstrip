//! Live inputs for the extras buttons and the test screen: a camera feed (ffmpeg → raw
//! pictures), the webcam service's preview file (the camera button's picture) and an audio
//! feed (pw-record → samples) for the spectra (see `spectrum`).
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
use std::time::SystemTime;

pub(crate) use crate::spectrum::{AudioSource, FFT_SIZE};

/// The built-in camera (H.264 only), and the browser-compatible loopback that
/// `t1-webcam.service` feeds while it holds the camera.
const CAMERA_DEVICE: &str = "/dev/v4l/by-id/usb-Apple_Inc._iBridge-video-index0";
const CAMERA_LOOPBACK: &str = "/dev/video10";
/// In `$XDG_RUNTIME_DIR`: see [`CameraPreview`].
const CAMERA_PREVIEW: &str = "t1-webcam-preview.bgr0";

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
    use crate::spectrum::{SAMPLE_RATE, Spectrum};

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
