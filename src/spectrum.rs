//! The spectrum behind the mic button, the playing play/pause button and the test screen:
//! an FFT of the latest audio samples into log-spaced bars that jump up and fall back, and
//! the mic face that draws them. Shared with the project page (WebAssembly), which feeds it
//! the browser's audio.

// Pixel and sample maths is f32; every float-to-int cast is clamped into range first.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use crate::clock::Instant;
use crate::theme::FaceTint;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioSource {
    /// The default source: the microphone, or line in when that is the default.
    Input,
    /// The default sink's monitor: whatever is playing.
    Speakers,
}

/// Samples per analysis (about 43 ms at 48 kHz).
pub const FFT_SIZE: usize = 2048;
/// The rate the samples come in (`pw-record` on the bar; the page asks the browser for it).
pub const SAMPLE_RATE: f32 = 48_000.0;
const LOWEST_HZ: f32 = 30.0;
const HIGHEST_HZ: f32 = 18_000.0;
/// Level mapping: this many dB below full scale is an empty bar, `TOP_DB` a full one.
const FLOOR_DB: f32 = -80.0;
const TOP_DB: f32 = -20.0;

/// Mic spectrum bars: pitch and width in pixels, and the inset from the plate's ends.
pub(crate) const MIC_BAR_PITCH: usize = 7;
const MIC_BAR_WIDTH: usize = 5;
pub(crate) const MIC_INSET: usize = 6;
/// Mic face colours as XRGB8888 bytes (B, G, R, X): a darker shade of the lit plate's
/// green, bars from that green (bottom) to pale green (top), and pale peak markers.
const MIC_BACKGROUND: [u8; 4] = [30, 56, 16, 0];
const MIC_BAR_BOTTOM: [f32; 3] = [92.0, 164.0, 58.0];
const MIC_BAR_TOP: [f32; 3] = [170.0, 235.0, 150.0];
const MIC_PEAK: [u8; 4] = [225, 255, 220, 0];

/// FFT state and smoothed bar levels for a spectrum display.
pub struct Spectrum {
    window: Vec<f32>,
    twiddles: Vec<[f32; 2]>,
    data: Vec<[f32; 2]>,
    /// FFT bin range per bar.
    pub bands: Vec<(usize, usize)>,
    pub targets: Vec<f32>,
    pub levels: Vec<f32>,
    pub peaks: Vec<f32>,
    /// When [`Spectrum::settle`] last ran.
    last: Option<Instant>,
}

impl Spectrum {
    #[must_use]
    pub fn new(bars: usize) -> Self {
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
            last: None,
        }
    }

    /// Analyses the latest [`FFT_SIZE`] samples (mono, 48 kHz) into the bars' targets.
    pub fn analyse(&mut self, samples: &[f32]) {
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
    pub fn settle(&mut self) {
        let now = Instant::now();
        let dt = self
            .last
            .map_or(0.0, |last| now.duration_since(last).as_secs_f32());
        self.last = Some(now);
        self.settle_by(dt);
    }

    /// [`Spectrum::settle`] for `dt` seconds since the last frame.
    pub fn settle_by(&mut self, dt: f32) {
        let dt = dt.min(0.1);
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

/// Bars for a mic face `width` pixels wide.
#[must_use]
pub const fn face_bars(width: usize) -> usize {
    width.saturating_sub(2 * MIC_INSET) / MIC_BAR_PITCH
}

/// The mic face: the bars in green over a darker green, with peak markers, in the theme's
/// tint (the playing play/pause button and the page use it too).
pub fn draw_mic_spectrum<'a>(
    rows: impl Iterator<Item = &'a mut [u8]>,
    spectrum: &Spectrum,
    width: usize,
    height: usize,
    tint: FaceTint,
) {
    let background = tint.apply(MIC_BACKGROUND);
    let peak_colour = tint.apply(MIC_PEAK);
    let bars = spectrum.levels.len();
    let first = (width - bars * MIC_BAR_PITCH) / 2 + (MIC_BAR_PITCH - MIC_BAR_WIDTH) / 2;
    let full = height as f32;
    for (y, row) in rows.enumerate() {
        let from_bottom = (height - y) as f32;
        let shade = from_bottom / full;
        let bar_colour = std::array::from_fn::<f32, 3, _>(|channel| {
            MIC_BAR_BOTTOM[channel] + (MIC_BAR_TOP[channel] - MIC_BAR_BOTTOM[channel]) * shade
        });
        let bar_colour = tint.apply([
            bar_colour[0] as u8,
            bar_colour[1] as u8,
            bar_colour[2] as u8,
            0,
        ]);
        for pixel in row.as_chunks_mut::<4>().0 {
            *pixel = background;
        }
        for (bar, (&level, &peak)) in spectrum.levels.iter().zip(&spectrum.peaks).enumerate() {
            let peak_row = (peak * full).round();
            let colour = if (from_bottom - peak_row).abs() < 1.0 && peak > 0.02 {
                peak_colour
            } else if from_bottom <= level * full {
                bar_colour
            } else {
                continue;
            };
            let left = first + bar * MIC_BAR_PITCH;
            for pixel in row[left * 4..(left + MIC_BAR_WIDTH) * 4]
                .as_chunks_mut::<4>()
                .0
            {
                *pixel = colour;
            }
        }
    }
}
