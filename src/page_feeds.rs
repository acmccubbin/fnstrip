//! The test screen's audio and camera feeds in the browser build, filled by the page
//! (`web/`): the same shapes as `live`'s `pw-record` and `ffmpeg` feeds on the bar.

use std::sync::{Arc, LazyLock, Mutex};

use crate::spectrum::{AudioSource, FFT_SIZE};

type Shared<T> = LazyLock<Arc<Mutex<T>>>;

static MIC: Shared<Vec<f32>> = LazyLock::new(|| Arc::new(Mutex::new(vec![0.0; FFT_SIZE])));
static SPEAKERS: Shared<Vec<f32>> = LazyLock::new(|| Arc::new(Mutex::new(vec![0.0; FFT_SIZE])));
static CAMERA: Shared<Vec<u8>> = LazyLock::new(|| Arc::new(Mutex::new(Vec::new())));
/// The picture size a running camera feed wants, if one runs.
static CAMERA_SIZE: Mutex<Option<(usize, usize)>> = Mutex::new(None);

fn samples(source: AudioSource) -> Arc<Mutex<Vec<f32>>> {
    Arc::clone(match source {
        AudioSource::Input => &MIC,
        AudioSource::Speakers => &SPEAKERS,
    })
}

/// The page's latest `FFT_SIZE` samples (mono, 48 kHz) of `source`.
pub fn set_samples(source: AudioSource, latest: &[f32]) {
    if let Ok(mut samples) = samples(source).lock() {
        let keep = latest.len().min(samples.len());
        samples[..keep].copy_from_slice(&latest[..keep]);
    }
}

/// The camera picture size a running feed wants (width, height), if one runs.
#[must_use]
pub fn camera_size() -> Option<(usize, usize)> {
    CAMERA_SIZE.lock().ok().and_then(|size| *size)
}

/// The page's latest camera picture, RGBA at [`camera_size`].
pub fn set_camera_frame(rgba: &[u8]) {
    let Some((width, height)) = camera_size() else {
        return;
    };
    if rgba.len() != width * height * 4 {
        return;
    }
    if let Ok(mut frame) = CAMERA.lock() {
        frame.clear();
        frame.extend(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .flat_map(|[red, green, blue, _]| [*blue, *green, *red, 0]),
        );
    }
}

/// An audio capture: the page's samples of `source`.
pub struct AudioFeed {
    pub source: AudioSource,
    pub samples: Arc<Mutex<Vec<f32>>>,
}

impl AudioFeed {
    #[must_use]
    #[allow(
        clippy::unnecessary_wraps,
        reason = "the same shape as live::AudioFeed::start"
    )]
    pub fn start(source: AudioSource) -> Option<Self> {
        Some(Self {
            source,
            samples: samples(source),
        })
    }
}

/// A camera capture: the page's pictures, at the size asked for.
pub struct CameraFeed {
    /// The latest whole picture (XRGB8888); empty until the first arrives.
    pub frame: Arc<Mutex<Vec<u8>>>,
}

impl CameraFeed {
    #[must_use]
    #[allow(
        clippy::unnecessary_wraps,
        reason = "the same shape as live::CameraFeed::start"
    )]
    pub fn start(width: usize, height: usize) -> Option<Self> {
        if let Ok(mut size) = CAMERA_SIZE.lock() {
            *size = Some((width, height));
        }
        if let Ok(mut frame) = CAMERA.lock() {
            frame.clear();
        }
        Some(Self {
            frame: Arc::clone(&CAMERA),
        })
    }
}

impl Drop for CameraFeed {
    fn drop(&mut self) {
        if let Ok(mut size) = CAMERA_SIZE.lock() {
            *size = None;
        }
    }
}
