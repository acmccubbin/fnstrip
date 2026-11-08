//! Triple-tap-and-hold Fn: full-colour animations to measure frame rate, render cost and
//! CPU (this renderer and the root `t1-touchbar-hw` service), plus spectrum analysers of
//! the default audio input (microphone or line in) and of the speaker output, and a live
//! view of the built-in camera (one plate, or a wide slice across the whole bar).
//!
//! Stats show on the right end of the bar and go to the journal once a second. Every
//! measured stretch (one scene at one buffer count) is appended to
//! `$XDG_STATE_HOME/fnstrip/screensaver.log` (default `~/.local/state`). While it runs:
//! tap the left end for the next scene, tap the stats to change how many frame buffers may
//! be in flight (1–3), tap anywhere else or press Fn to exit. After the last scene comes
//! "A": an automatic sweep of every non-audio scene at every buffer count, which logs a
//! results table and then exits.
//!
//! The button scenes animate only one to three plates and submit just those rectangles
//! as damage, to measure what partial updates cost compared with full frames.

// Pixel maths is f32; every float-to-int cast is clamped or wrapped into range first.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use t1_touchbar_hw::frame_state::DamageRectangle;

use crate::live::{AudioFeed, AudioSource, CameraFeed, Spectrum};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scene {
    Plasma,
    Ribbons,
    Starfield,
    Fire,
    /// Plasma in this many stock-sized plates; only they are submitted as damage.
    Buttons(usize),
    Spectrum(AudioSource),
    Camera(CameraView),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CameraView {
    /// The middle of the picture in the first stock plate; only that plate is damage.
    Button,
    /// A horizontal strip through the middle of the picture, the full width of the bar.
    Wide,
}

impl Scene {
    const fn name(self) -> &'static str {
        match self {
            Self::Plasma => "plasma",
            Self::Ribbons => "ribbons",
            Self::Starfield => "starfield",
            Self::Fire => "fire",
            Self::Buttons(1) => "1-button",
            Self::Buttons(2) => "2-buttons",
            Self::Buttons(_) => "3-buttons",
            Self::Spectrum(AudioSource::Input) => "mic",
            Self::Spectrum(AudioSource::Speakers) => "speakers",
            Self::Camera(CameraView::Button) => "cam",
            Self::Camera(CameraView::Wide) => "cam-wide",
        }
    }
}

const SCENES: [Scene; 11] = [
    Scene::Plasma,
    Scene::Ribbons,
    Scene::Starfield,
    Scene::Fire,
    Scene::Buttons(1),
    Scene::Buttons(2),
    Scene::Buttons(3),
    Scene::Spectrum(AudioSource::Input),
    Scene::Spectrum(AudioSource::Speakers),
    Scene::Camera(CameraView::Button),
    Scene::Camera(CameraView::Wide),
];
/// The mode after the last scene: the automatic sweep.
const SWEEP_MODE: usize = SCENES.len();
const SWEEP_STEP: Duration = Duration::from_secs(5);
/// Each measured stretch drops its first second (scene switch, buffer registration).
const WARMUP: Duration = Duration::from_secs(1);
/// Left end: next scene.
const CYCLE_ZONE: usize = 240;
/// Right end: the stats panel; tapping it cycles the buffer count.
const HUD_WIDTH: usize = 420;
const LABEL_WIDTH: usize = 120;
const GLYPH_SCALE: usize = 3;
const GLYPH_ADVANCE: usize = 6 * GLYPH_SCALE;
/// Stock plate geometry (see `StockBar`): the button scenes use the same slots.
const PLATE_LEFT: usize = 194;
const PLATE_PITCH: usize = 190;
const PLATE_WIDTH: usize = 182;
const PLATE_COUNT: usize = 8;
const PALETTE_SIZE: usize = 1024;
const STAR_COUNT: usize = 700;
const FIRE_STEPS_PER_SECOND: f32 = 60.0;
const BAR_PITCH: usize = 20;
const BAR_WIDTH: usize = 16;
/// Linux reports process CPU time in `USER_HZ` ticks, fixed at 100 per second.
const USER_HZ: f32 = 100.0;
const STATS_WINDOW: Duration = Duration::from_secs(1);

/// What a tap at some x does while the screensaver runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Zone {
    NextScene,
    NextBuffering,
    Exit,
}

pub(crate) struct Screensaver {
    width: usize,
    height: usize,
    stride: usize,
    max_buffers: usize,
    buffers: usize,
    /// Index into `SCENES`, or `SWEEP_MODE`.
    mode: usize,
    sweep: Option<Sweep>,
    started: Instant,
    full_frame: bool,
    rainbow: Vec<[u8; 4]>,
    flame: Vec<[u8; 4]>,
    columns: Vec<f32>,
    ribbons: Vec<RibbonColumn>,
    stars: Vec<Star>,
    heat: Vec<u8>,
    fire_steps: u64,
    rng: u32,
    spectrum: Spectrum,
    audio: Option<AudioFeed>,
    camera: Option<(CameraView, CameraFeed)>,
    in_flight: Vec<(u64, Instant)>,
    hw_pid: Option<u32>,
    window: Counters,
    segment: Counters,
    segment_warm: bool,
    hud: Summary,
    hud_text: String,
    total_presented: u64,
}

#[derive(Clone, Copy, Default)]
struct RibbonColumn {
    centre: [f32; 3],
    colour: [[f32; 3]; 3],
}

struct Star {
    x: f32,
    y: usize,
    speed: f32,
    colour: [f32; 3],
}

struct Sweep {
    steps: Vec<(usize, usize)>,
    step: usize,
    step_started: Instant,
    results: Vec<(Scene, usize, Summary)>,
}

/// Raw counts since some instant; turned into a [`Summary`] at the end of a window.
#[derive(Clone, Copy)]
struct Counters {
    since: Instant,
    rendered: u32,
    presented: u32,
    render_time: Duration,
    present_time: Duration,
    self_ticks: Option<u64>,
    hw_ticks: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Summary {
    seconds: f32,
    fps: f32,
    render_ms: f32,
    present_ms: f32,
    cpu: Option<u32>,
    hw_cpu: Option<u32>,
}

impl Counters {
    fn new(hw_pid: Option<u32>) -> Self {
        Self {
            since: Instant::now(),
            rendered: 0,
            presented: 0,
            render_time: Duration::ZERO,
            present_time: Duration::ZERO,
            self_ticks: process_ticks("self"),
            hw_ticks: hw_pid.and_then(|pid| process_ticks(&pid.to_string())),
        }
    }

    fn summary(&self, hw_pid: Option<u32>) -> Summary {
        let seconds = self.since.elapsed().as_secs_f32();
        let hw_ticks = hw_pid.and_then(|pid| process_ticks(&pid.to_string()));
        Summary {
            seconds,
            fps: self.presented as f32 / seconds.max(f32::EPSILON),
            render_ms: average_ms(self.render_time, self.rendered),
            present_ms: average_ms(self.present_time, self.presented),
            cpu: cpu_percent(self.self_ticks, process_ticks("self"), seconds),
            hw_cpu: cpu_percent(self.hw_ticks, hw_ticks, seconds),
        }
    }
}

impl Screensaver {
    pub(crate) fn new(width: u32, height: u32, max_buffers: u32) -> Self {
        let (width, height) = (width as usize, height as usize);
        let mut rng = 0x9E37_79B9;
        let stars = (0..STAR_COUNT)
            .map(|_| {
                let depth = unit(&mut rng);
                let hue = unit(&mut rng);
                let [blue, green, red, _] = hsv(hue, 0.35 * (1.0 - depth), 1.0);
                let brightness = 0.35 + 0.65 * depth;
                Star {
                    x: unit(&mut rng) * width as f32 * 2.0,
                    y: (unit(&mut rng) * height.saturating_sub(1) as f32) as usize,
                    speed: 40.0 + 1100.0 * depth * depth,
                    colour: [
                        f32::from(blue) * brightness,
                        f32::from(green) * brightness,
                        f32::from(red) * brightness,
                    ],
                }
            })
            .collect();
        let hw_pid = find_process("t1-touchbar-hw");
        let max_buffers = (max_buffers as usize).max(1);
        let header = format!(
            "== {} screensaver start: {width}x{height}, up to {max_buffers} buffers, hw pid {} ==",
            timestamp(),
            hw_pid.map_or_else(|| "not found".to_owned(), |pid| pid.to_string()),
        );
        eprintln!("fnstrip {header}");
        append_log(&header);
        Self {
            width,
            height,
            stride: width * 4,
            max_buffers,
            buffers: 1,
            mode: 0,
            sweep: None,
            started: Instant::now(),
            full_frame: true,
            rainbow: (0..PALETTE_SIZE)
                .map(|i| hsv(i as f32 / PALETTE_SIZE as f32, 0.9, 1.0))
                .collect(),
            flame: (0..=255).map(flame_colour).collect(),
            columns: vec![0.0; width],
            ribbons: vec![RibbonColumn::default(); width],
            stars,
            heat: vec![0; width * height],
            fire_steps: 0,
            rng,
            spectrum: Spectrum::new(width / BAR_PITCH),
            audio: None,
            camera: None,
            in_flight: Vec::new(),
            hw_pid,
            window: Counters::new(hw_pid),
            segment: Counters::new(hw_pid),
            segment_warm: false,
            hud: Summary::default(),
            hud_text: String::new(),
            total_presented: 0,
        }
    }

    pub(crate) const fn zone(&self, x: u32) -> Zone {
        let x = x as usize;
        if x < CYCLE_ZONE {
            Zone::NextScene
        } else if x + HUD_WIDTH >= self.width {
            Zone::NextBuffering
        } else {
            Zone::Exit
        }
    }

    fn scene(&self) -> Scene {
        match &self.sweep {
            Some(sweep) => SCENES[sweep.steps[sweep.step].0],
            None => SCENES[self.mode.min(SCENES.len() - 1)],
        }
    }

    /// How many frame buffers may be in flight at once.
    pub(crate) fn buffers(&self) -> usize {
        self.sweep
            .as_ref()
            .map_or(self.buffers, |sweep| sweep.steps[sweep.step].1)
    }

    pub(crate) fn next_scene(&mut self) {
        self.end_segment("manual");
        if self.sweep.take().is_some() {
            append_log(&format!("{} sweep cancelled", timestamp()));
            self.mode = 0;
        } else {
            self.mode = (self.mode + 1) % (SWEEP_MODE + 1);
        }
        if self.mode == SWEEP_MODE {
            self.start_sweep();
        }
        self.scene_changed();
    }

    pub(crate) fn next_buffering(&mut self) {
        if self.sweep.is_some() {
            return;
        }
        self.end_segment("manual");
        self.buffers = self.buffers % self.max_buffers + 1;
        self.begin_segment();
    }

    fn start_sweep(&mut self) {
        let steps: Vec<_> = (0..SCENES.len())
            .filter(|&scene| !matches!(SCENES[scene], Scene::Spectrum(_) | Scene::Camera(_)))
            .flat_map(|scene| (1..=self.max_buffers).map(move |buffers| (scene, buffers)))
            .collect();
        append_log(&format!(
            "{} sweep start: {} steps of {} s ({} s warm-up each)",
            timestamp(),
            steps.len(),
            SWEEP_STEP.as_secs(),
            WARMUP.as_secs(),
        ));
        self.sweep = Some(Sweep {
            steps,
            step: 0,
            step_started: Instant::now(),
            results: Vec::new(),
        });
    }

    fn scene_changed(&mut self) {
        self.full_frame = true;
        self.begin_segment();
        let wants = match self.scene() {
            Scene::Spectrum(source) => Some(source),
            _ => None,
        };
        if self.audio.as_ref().map(|audio| audio.source) != wants {
            // Drop (and stop) the old capture before starting the new one.
            self.audio = None;
            self.audio = wants.and_then(AudioFeed::start);
        }
        let wants = match self.scene() {
            Scene::Camera(view) => Some(view),
            _ => None,
        };
        if self.camera.as_ref().map(|(view, _)| *view) != wants {
            self.camera = None;
            self.camera = wants.and_then(|view| {
                let (width, height) = self.camera_size(view);
                CameraFeed::start(width, height).map(|feed| (view, feed))
            });
        }
    }

    fn begin_segment(&mut self) {
        self.segment = Counters::new(self.hw_pid);
        self.segment_warm = false;
    }

    /// Logs the stretch just measured, if it got past its warm-up.
    fn end_segment(&mut self, kind: &str) -> Option<Summary> {
        if !self.segment_warm {
            return None;
        }
        self.segment_warm = false;
        let summary = self.segment.summary(self.hw_pid);
        if summary.seconds < 1.0 {
            return None;
        }
        let line = format!(
            "{} {kind:<6} {:<9} {} buf {:>5.1} s {:>6.1} fps  render {:>5.2} ms  \
             submit->release {:>6.2} ms  cpu {:>4}  hw {:>4}",
            timestamp(),
            self.scene().name(),
            self.buffers(),
            summary.seconds,
            summary.fps,
            summary.render_ms,
            summary.present_ms,
            percent_label(summary.cpu),
            percent_label(summary.hw_cpu),
        );
        eprintln!("fnstrip screensaver: {line}");
        append_log(&line);
        Some(summary)
    }

    pub(crate) fn submitted(&mut self, frame_id: u64) {
        self.in_flight.push((frame_id, Instant::now()));
    }

    pub(crate) fn released(&mut self, frame_id: u64) {
        if let Some(index) = self.in_flight.iter().position(|(id, _)| *id == frame_id) {
            let (_, submitted) = self.in_flight.swap_remove(index);
            let latency = submitted.elapsed();
            for counters in [&mut self.window, &mut self.segment] {
                counters.presented += 1;
                counters.present_time += latency;
            }
            self.total_presented += 1;
        }
    }

    /// Housekeeping between frames: warm-up, the HUD's one-second window, and sweep
    /// steps. Returns true when a sweep has finished and the screensaver should exit.
    pub(crate) fn tick(&mut self) -> bool {
        if !self.segment_warm && self.segment.since.elapsed() >= WARMUP {
            self.segment = Counters::new(self.hw_pid);
            self.segment_warm = true;
        }
        if self.window.since.elapsed() >= STATS_WINDOW {
            self.hud = self.window.summary(self.hw_pid);
            self.window = Counters::new(self.hw_pid);
            eprintln!(
                "fnstrip screensaver: scene={} buffers={} fps={:.1} render_ms={:.2} \
                 submit_to_release_ms={:.2} renderer_cpu={} hw_cpu={}",
                self.scene().name(),
                self.buffers(),
                self.hud.fps,
                self.hud.render_ms,
                self.hud.present_ms,
                percent_label(self.hud.cpu),
                percent_label(self.hud.hw_cpu),
            );
        }
        let step_done = self
            .sweep
            .as_ref()
            .is_some_and(|sweep| sweep.step_started.elapsed() >= SWEEP_STEP);
        if !step_done {
            return false;
        }
        let scene = self.scene();
        let buffers = self.buffers();
        let summary = self.end_segment("sweep");
        let Some(sweep) = self.sweep.as_mut() else {
            return false;
        };
        if let Some(summary) = summary {
            sweep.results.push((scene, buffers, summary));
        }
        sweep.step += 1;
        sweep.step_started = Instant::now();
        if sweep.step < sweep.steps.len() {
            self.scene_changed();
            return false;
        }
        let table = results_table(&sweep.results);
        self.sweep = None;
        append_log(&table);
        eprintln!(
            "fnstrip screensaver: sweep finished, table in {}",
            log_path().display()
        );
        true
    }

    pub(crate) fn finish(&mut self) {
        self.end_segment("manual");
        if self.sweep.take().is_some() {
            append_log(&format!("{} sweep cancelled", timestamp()));
        }
        self.audio = None;
        self.camera = None;
        let seconds = self.started.elapsed().as_secs_f32();
        let line = format!(
            "== {} screensaver stop after {seconds:.1} s, {} frames presented ({:.1} fps average) ==",
            timestamp(),
            self.total_presented,
            self.total_presented as f32 / seconds.max(f32::EPSILON),
        );
        eprintln!("fnstrip {line}");
        append_log(&line);
    }

    /// Draws one frame into a negotiated XRGB8888 buffer and returns the damage to
    /// submit (empty = the whole frame).
    pub(crate) fn render(&mut self, pixels: &mut [u8]) -> Vec<DamageRectangle> {
        let begun = Instant::now();
        let t = self.started.elapsed().as_secs_f32();
        let full = std::mem::take(&mut self.full_frame);
        let mut damage = match self.scene() {
            Scene::Plasma => {
                self.plasma(pixels, t, 0, self.width);
                Vec::new()
            }
            Scene::Ribbons => {
                self.ribbons(pixels, t);
                Vec::new()
            }
            Scene::Starfield => {
                self.starfield(pixels, t);
                Vec::new()
            }
            Scene::Fire => {
                self.fire(pixels, t);
                Vec::new()
            }
            Scene::Buttons(count) => self.buttons(pixels, t, count, full),
            Scene::Spectrum(_) => {
                self.draw_spectrum(pixels);
                Vec::new()
            }
            Scene::Camera(view) => self.draw_camera(pixels, view, full),
        };
        if self.overlay(pixels) && !damage.is_empty() {
            damage.push(self.rectangle(self.width.saturating_sub(HUD_WIDTH), HUD_WIDTH));
        }
        let elapsed = begun.elapsed();
        for counters in [&mut self.window, &mut self.segment] {
            counters.rendered += 1;
            counters.render_time += elapsed;
        }
        damage
    }

    /// A full-height rectangle, clipped to the bar.
    fn rectangle(&self, left: usize, width: usize) -> DamageRectangle {
        let left = left.min(self.width);
        DamageRectangle {
            x: left as u32,
            y: 0,
            width: width.min(self.width - left) as u32,
            height: self.height as u32,
        }
    }

    /// Classic sine plasma between two columns: two column waves, a row wave, and
    /// moving rings, per pixel.
    fn plasma(&mut self, pixels: &mut [u8], t: f32, left: usize, right: usize) {
        let right = right.min(self.width);
        for (x, column) in self.columns.iter_mut().enumerate().take(right).skip(left) {
            let x = x as f32;
            *column = (x * 0.009 + t * 0.9).sin() + (x * 0.0031 - t * 0.43).sin();
        }
        let centre_x = self.width as f32 * (0.5 + 0.42 * (t * 0.23).sin());
        let centre_y = self.height as f32 * (0.5 + 0.5 * (t * 0.71).cos());
        let shift = t * 0.08;
        for (y, row) in pixels
            .chunks_exact_mut(self.stride)
            .take(self.height)
            .enumerate()
        {
            let dy = y as f32 - centre_y;
            let row_wave = (y as f32 * 0.07 + t * 1.7).sin();
            let row = &mut row[left * 4..right * 4];
            for (x, (pixel, column)) in row
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(&self.columns[left..right])
                .enumerate()
            {
                let dx = (left + x) as f32 - centre_x;
                let rings = ((dx * dx + dy * dy).sqrt() * 0.025 - t * 2.2).sin();
                let value = (column + row_wave + rings) / 8.0 + shift;
                *pixel = self.rainbow[palette_index(value)];
            }
        }
    }

    /// Stock-looking grey plates with plasma in the first `count`; only those are damage.
    fn buttons(
        &mut self,
        pixels: &mut [u8],
        t: f32,
        count: usize,
        full: bool,
    ) -> Vec<DamageRectangle> {
        if full {
            pixels.fill(0);
            for plate in count..PLATE_COUNT {
                let left = PLATE_LEFT + plate * PLATE_PITCH;
                let right = (left + PLATE_WIDTH).min(self.width);
                for row in pixels.chunks_exact_mut(self.stride).take(self.height) {
                    for pixel in row[left * 4..right * 4].as_chunks_mut::<4>().0 {
                        *pixel = [53, 53, 53, 0];
                    }
                }
            }
        }
        let mut damage = Vec::new();
        for plate in 0..count {
            let left = PLATE_LEFT + plate * PLATE_PITCH;
            self.plasma(pixels, t, left, left + PLATE_WIDTH);
            damage.push(self.rectangle(left, PLATE_WIDTH));
        }
        if full { Vec::new() } else { damage }
    }

    /// Three glowing sine ribbons drifting through the spectrum.
    fn ribbons(&mut self, pixels: &mut [u8], t: f32) {
        const RIBBONS: [(f32, f32, f32); 3] =
            [(0.004, 0.7, 0.0), (0.0061, -0.9, 2.1), (0.0023, 1.3, 4.2)];
        let middle = self.height as f32 / 2.0;
        for (x, column) in self.ribbons.iter_mut().enumerate() {
            let x = x as f32;
            for (i, (frequency, speed, phase)) in RIBBONS.into_iter().enumerate() {
                column.centre[i] = middle
                    + middle * 0.8 * (x * frequency + t * speed + phase).sin()
                    + 3.0 * (x * 0.031 - t * 3.0 + phase).sin();
                let hue = i as f32 / 3.0 + x / self.width as f32 * 0.5 + t * 0.05;
                let [blue, green, red, _] = self.rainbow[palette_index(hue)];
                column.colour[i] = [f32::from(blue), f32::from(green), f32::from(red)];
            }
        }
        for (y, row) in pixels
            .chunks_exact_mut(self.stride)
            .take(self.height)
            .enumerate()
        {
            let y = y as f32;
            for (pixel, column) in row.as_chunks_mut::<4>().0.iter_mut().zip(&self.ribbons) {
                let mut sum = [24.0, 6.0, 8.0];
                for (centre, colour) in column.centre.iter().zip(&column.colour) {
                    let distance = y - centre;
                    let glow = 1.0 / (1.0 + distance * distance * 0.04);
                    for (channel, value) in sum.iter_mut().zip(colour) {
                        *channel += value * glow;
                    }
                }
                *pixel = [
                    sum[0].min(255.0) as u8,
                    sum[1].min(255.0) as u8,
                    sum[2].min(255.0) as u8,
                    0,
                ];
            }
        }
    }

    /// Stars streaking right to left; faster (nearer) stars are brighter, longer and whiter.
    fn starfield(&self, pixels: &mut [u8], t: f32) {
        for row in pixels.chunks_exact_mut(self.stride).take(self.height) {
            for pixel in row.as_chunks_mut::<4>().0 {
                *pixel = [14, 3, 4, 0];
            }
        }
        for star in &self.stars {
            let tail = (star.speed * 0.06).max(2.0);
            let span = self.width as f32 + tail;
            let head = (star.x - star.speed * t).rem_euclid(span) - tail;
            let thick = star.speed > 500.0;
            for i in 0..tail as usize {
                let x = head + i as f32;
                if x < 0.0 {
                    continue;
                }
                let x = x as usize;
                if x >= self.width {
                    break;
                }
                let fade = 1.0 - i as f32 / tail;
                add_pixel(pixels, self.stride, x, star.y, star.colour, fade);
                if thick && star.y + 1 < self.height {
                    add_pixel(pixels, self.stride, x, star.y + 1, star.colour, fade * 0.6);
                }
            }
        }
    }

    /// Cellular fire: heat rises from a flickering bottom row and cools as it climbs.
    fn fire(&mut self, pixels: &mut [u8], t: f32) {
        let target = (t * FIRE_STEPS_PER_SECOND) as u64;
        let steps = target.saturating_sub(self.fire_steps).min(4);
        self.fire_steps = target;
        for _ in 0..steps {
            self.fire_step();
        }
        for (row, heat) in pixels
            .chunks_exact_mut(self.stride)
            .take(self.height)
            .zip(self.heat.chunks_exact(self.width))
        {
            for (pixel, &heat) in row.as_chunks_mut::<4>().0.iter_mut().zip(heat) {
                *pixel = self.flame[usize::from(heat)];
            }
        }
    }

    fn fire_step(&mut self) {
        let (width, height) = (self.width, self.height);
        if width < 3 || height < 3 {
            return;
        }
        let bottom = (height - 1) * width;
        for cell in &mut self.heat[bottom..] {
            *cell = if next_random(&mut self.rng).is_multiple_of(3) {
                255
            } else {
                90
            };
        }
        for y in 0..height - 1 {
            let below = (y + 1) * width;
            let below_two = (y + 2).min(height - 1) * width;
            for x in 0..width {
                let left = if x == 0 { width - 1 } else { x - 1 };
                let right = if x + 1 == width { 0 } else { x + 1 };
                let sum = u32::from(self.heat[below + left])
                    + u32::from(self.heat[below + x])
                    + u32::from(self.heat[below + right])
                    + u32::from(self.heat[below_two + x]);
                let decay = next_random(&mut self.rng) % 6;
                self.heat[y * width + x] = (sum / 4).saturating_sub(decay) as u8;
            }
        }
    }

    /// Log-spaced bars (30 Hz–18 kHz) of the latest audio, with falling peak markers.
    fn draw_spectrum(&mut self, pixels: &mut [u8]) {
        if let Some(audio) = &self.audio
            && let Ok(samples) = audio.samples.lock()
        {
            self.spectrum.analyse(&samples);
        }
        self.spectrum.settle();
        let height = self.height as f32;
        for (y, row) in pixels
            .chunks_exact_mut(self.stride)
            .take(self.height)
            .enumerate()
        {
            for pixel in row.as_chunks_mut::<4>().0 {
                *pixel = [12, 4, 6, 0];
            }
            let from_bottom = (self.height - y) as f32;
            for (bar, (&level, &peak)) in self
                .spectrum
                .levels
                .iter()
                .zip(&self.spectrum.peaks)
                .enumerate()
            {
                let peak_row = (peak * height).round();
                let colour = if (from_bottom - peak_row).abs() < 1.0 && peak > 0.02 {
                    [235, 235, 235, 0]
                } else if from_bottom <= level * height {
                    let [blue, green, red, _] = self.rainbow
                        [palette_index(bar as f32 / self.spectrum.levels.len() as f32 * 0.8)];
                    let shade = 0.45 + 0.55 * from_bottom / height;
                    [
                        (f32::from(blue) * shade) as u8,
                        (f32::from(green) * shade) as u8,
                        (f32::from(red) * shade) as u8,
                        0,
                    ]
                } else {
                    continue;
                };
                let left = bar * BAR_PITCH + (BAR_PITCH - BAR_WIDTH) / 2;
                let right = (left + BAR_WIDTH).min(self.width);
                for pixel in row[left * 4..right * 4].as_chunks_mut::<4>().0 {
                    *pixel = colour;
                }
            }
        }
    }

    /// Picture size ffmpeg delivers for a camera view: exactly the area it fills.
    fn camera_size(&self, view: CameraView) -> (usize, usize) {
        match view {
            CameraView::Button => (PLATE_WIDTH, self.height),
            CameraView::Wide => (self.width, self.height),
        }
    }

    /// The latest camera picture, mid-grey until the first one arrives.
    fn draw_camera(&self, pixels: &mut [u8], view: CameraView, full: bool) -> Vec<DamageRectangle> {
        let (width, height) = self.camera_size(view);
        let left = match view {
            CameraView::Button => PLATE_LEFT,
            CameraView::Wide => 0,
        };
        if full {
            pixels.fill(0);
        }
        let latest = self
            .camera
            .as_ref()
            .and_then(|(_, camera)| camera.frame.lock().ok());
        let picture = latest.as_deref().filter(|frame| !frame.is_empty());
        for (y, row) in pixels
            .chunks_exact_mut(self.stride)
            .take(height.min(self.height))
            .enumerate()
        {
            let row = &mut row[left * 4..(left + width).min(self.width) * 4];
            match picture {
                Some(frame) => row.copy_from_slice(&frame[y * width * 4..][..row.len()]),
                None => row.fill(53),
            }
        }
        match view {
            CameraView::Button if !full => vec![self.rectangle(left, width)],
            _ => Vec::new(),
        }
    }

    /// Scene label on the left (the next-scene zone), stats on the right. Returns true
    /// when the stats text changed since the last frame.
    fn overlay(&mut self, pixels: &mut [u8]) -> bool {
        let glyph_height = 7 * GLYPH_SCALE;
        self.darken(pixels, 0, LABEL_WIDTH);
        let label = match &self.sweep {
            Some(sweep) => format!("A{}/{}", sweep.step + 1, sweep.steps.len()),
            None => format!("{}/{}", self.mode + 1, SWEEP_MODE + 1),
        };
        self.text(
            pixels,
            12,
            self.height.saturating_sub(glyph_height) / 2,
            &label,
        );

        let left = self.width.saturating_sub(HUD_WIDTH);
        self.darken(pixels, left, HUD_WIDTH);
        let mut text = String::new();
        let _ = write!(text, "{:.1} FPS {:.1} MS", self.hud.fps, self.hud.render_ms);
        let line_two = format!(
            "CPU {} HW {} {}BUF",
            percent_label(self.hud.cpu),
            percent_label(self.hud.hw_cpu),
            self.buffers()
        );
        let gap = self.height.saturating_sub(2 * glyph_height) / 3;
        self.text(pixels, left + 18, gap, &text);
        self.text(pixels, left + 18, 2 * gap + glyph_height, &line_two);
        text.push_str(&line_two);
        let changed = text != self.hud_text;
        self.hud_text = text;
        changed
    }

    fn darken(&self, pixels: &mut [u8], left: usize, width: usize) {
        let right = (left + width).min(self.width);
        for row in pixels.chunks_exact_mut(self.stride).take(self.height) {
            for pixel in row[left * 4..right * 4].as_chunks_mut::<4>().0 {
                for channel in &mut pixel[..3] {
                    *channel /= 4;
                }
            }
        }
    }

    fn text(&self, pixels: &mut [u8], left: usize, top: usize, text: &str) {
        for (index, character) in text.chars().enumerate() {
            let Some(rows) = glyph(character) else {
                continue;
            };
            let glyph_left = left + index * GLYPH_ADVANCE;
            for (glyph_y, bits) in rows.iter().enumerate() {
                for glyph_x in 0..5 {
                    if bits & (0x10 >> glyph_x) == 0 {
                        continue;
                    }
                    for dy in 0..GLYPH_SCALE {
                        for dx in 0..GLYPH_SCALE {
                            let x = glyph_left + glyph_x * GLYPH_SCALE + dx;
                            let y = top + glyph_y * GLYPH_SCALE + dy;
                            if x < self.width && y < self.height {
                                let offset = y * self.stride + x * 4;
                                pixels[offset..offset + 4].copy_from_slice(&[242, 242, 242, 0]);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn add_pixel(pixels: &mut [u8], stride: usize, x: usize, y: usize, colour: [f32; 3], amount: f32) {
    let offset = y * stride + x * 4;
    for (channel, value) in pixels[offset..offset + 3].iter_mut().zip(colour) {
        *channel = (f32::from(*channel) + value * amount).min(255.0) as u8;
    }
}

/// Wraps any value onto the palette (period 1.0).
fn palette_index(value: f32) -> usize {
    ((value - value.floor()) * PALETTE_SIZE as f32) as usize % PALETTE_SIZE
}

/// HSV (all 0–1) to an XRGB8888 pixel (bytes B, G, R, X).
fn hsv(hue: f32, saturation: f32, value: f32) -> [u8; 4] {
    let channel = |offset: f32| {
        let k = (offset + hue * 6.0).rem_euclid(6.0);
        let ramp = (k.min(4.0 - k)).clamp(0.0, 1.0);
        ((value - value * saturation * ramp) * 255.0).round() as u8
    };
    [channel(1.0), channel(3.0), channel(5.0), 0]
}

/// Black → deep red → orange → yellow → white.
fn flame_colour(heat: usize) -> [u8; 4] {
    let heat = heat as f32 / 255.0;
    let red = (heat * 3.0).min(1.0);
    let green = (heat * 3.0 - 1.0).clamp(0.0, 1.0);
    let blue = (heat * 3.0 - 2.0).clamp(0.0, 1.0);
    [
        (blue * 255.0) as u8,
        (green * 255.0) as u8,
        (red * 255.0) as u8,
        0,
    ]
}

fn next_random(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

fn unit(state: &mut u32) -> f32 {
    (next_random(state) >> 8) as f32 / (1 << 24) as f32
}

fn results_table(results: &[(Scene, usize, Summary)]) -> String {
    let mut table = format!(
        "{} sweep results\n\n| scene | buffers | fps | render ms | submit->release ms | renderer cpu | hw cpu |\n|---|---|---|---|---|---|---|\n",
        timestamp()
    );
    for (scene, buffers, summary) in results {
        let _ = writeln!(
            table,
            "| {} | {buffers} | {:.1} | {:.2} | {:.2} | {} | {} |",
            scene.name(),
            summary.fps,
            summary.render_ms,
            summary.present_ms,
            percent_label(summary.cpu),
            percent_label(summary.hw_cpu),
        );
    }
    table
}

fn log_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("fnstrip/screensaver.log")
}

fn append_log(line: &str) {
    if cfg!(test) {
        return;
    }
    let path = log_path();
    if let Some(directory) = path.parent() {
        let _ = fs::create_dir_all(directory);
    }
    let written = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(error) = written {
        eprintln!(
            "fnstrip screensaver: could not write {}: {error}",
            path.display()
        );
    }
}

/// Local wall-clock time for the log (std has no time zones, so ask `date`).
fn timestamp() -> String {
    Command::new("date")
        .arg("+%F %T %Z")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(|| "?".to_owned(), |text| text.trim_end().to_owned())
}

/// User + system CPU time of a process, in ticks, from `/proc/<pid>/stat`.
fn process_ticks(pid: &str) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the `(comm)`: state is field 3, utime 14 and stime 15.
    let mut fields = stat
        .get(stat.rfind(')')? + 2..)?
        .split_whitespace()
        .skip(11);
    let user: u64 = fields.next()?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    Some(user + system)
}

fn find_process(name: &str) -> Option<u32> {
    fs::read_dir("/proc").ok()?.flatten().find_map(|entry| {
        let pid = entry.file_name().to_str()?.parse().ok()?;
        let comm = fs::read_to_string(entry.path().join("comm")).ok()?;
        (comm.trim_end() == name).then_some(pid)
    })
}

fn cpu_percent(before: Option<u64>, after: Option<u64>, seconds: f32) -> Option<u32> {
    let ticks = after?.checked_sub(before?)?;
    Some((ticks as f32 / USER_HZ / seconds * 100.0).round() as u32)
}

fn percent_label(percent: Option<u32>) -> String {
    percent.map_or_else(|| "-".to_owned(), |percent| format!("{percent}%"))
}

fn average_ms(total: Duration, count: u32) -> f32 {
    if count == 0 {
        0.0
    } else {
        total.as_secs_f32() * 1000.0 / count as f32
    }
}

/// 5x7 bitmap glyphs for the HUD (bit 4 is the leftmost column).
const fn glyph(character: char) -> Option<[u8; 7]> {
    Some(match character {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1E, 0x01, 0x01, 0x0E, 0x01, 0x01, 0x1E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saver() -> Screensaver {
        Screensaver::new(2170, 60, 3)
    }

    #[test]
    fn every_scene_renders_a_full_bar_frame() {
        let mut saver = saver();
        let mut pixels = vec![0_u8; 2170 * 60 * 4];
        for scene in SCENES {
            saver.full_frame = true;
            saver.mode = SCENES.iter().position(|s| *s == scene).unwrap();
            assert_eq!(saver.render(&mut pixels), [], "{}", scene.name());
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[..3] != [0, 0, 0])
            );
        }
    }

    #[test]
    fn button_scenes_damage_only_their_plates_after_the_first_frame() {
        let mut saver = saver();
        saver.mode = 5; // 2 buttons
        let mut pixels = vec![0_u8; 2170 * 60 * 4];
        assert_eq!(saver.render(&mut pixels), []);
        let damage = saver.render(&mut pixels);
        assert_eq!(
            damage
                .iter()
                .map(|rect| (rect.x, rect.width))
                .collect::<Vec<_>>(),
            [(194, 182), (384, 182)]
        );
    }

    #[test]
    fn camera_button_damages_only_its_plate_and_is_grey_without_a_picture() {
        let mut saver = saver();
        saver.mode = SCENES
            .iter()
            .position(|scene| *scene == Scene::Camera(CameraView::Button))
            .unwrap();
        let mut pixels = vec![0_u8; 2170 * 60 * 4];
        assert_eq!(saver.render(&mut pixels), []);
        let damage = saver.render(&mut pixels);
        assert_eq!(
            damage
                .iter()
                .map(|rect| (rect.x, rect.width))
                .collect::<Vec<_>>(),
            [(194, 182)]
        );
        let middle = (30 * 2170 + 194 + 91) * 4;
        assert_eq!(pixels[middle..middle + 3], [53, 53, 53]);
    }

    #[test]
    fn zones_and_buffer_cycling() {
        let mut saver = saver();
        assert_eq!(saver.zone(10), Zone::NextScene);
        assert_eq!(saver.zone(1000), Zone::Exit);
        assert_eq!(saver.zone(2100), Zone::NextBuffering);
        let counts: Vec<_> = (0..4)
            .map(|_| {
                saver.next_buffering();
                saver.buffers()
            })
            .collect();
        assert_eq!(counts, [2, 3, 1, 2]);
    }

    #[test]
    fn sweep_covers_every_non_audio_scene_at_every_buffer_count() {
        let mut saver = saver();
        saver.mode = SWEEP_MODE - 1;
        saver.next_scene();
        let steps = &saver.sweep.as_ref().expect("sweep").steps;
        assert_eq!(steps.len(), 7 * 3);
        assert!(
            steps
                .iter()
                .all(|&(scene, _)| !matches!(SCENES[scene], Scene::Spectrum(_) | Scene::Camera(_)))
        );
        saver.next_scene();
        assert!(saver.sweep.is_none());
        assert_eq!(saver.mode, 0);
    }
}
