//! Extras layer (double-tap Fn; stays until the next Fn press): runs each configured button's command and
//! keeps its on/off state.
//!
//! Commands run as the user with `sh -c` (this renderer is a user process; the hardware
//! service has no command action). Status commands only run while the extras row is
//! showing (every second), plus once at startup and after each tap, so the bar costs
//! nothing otherwise.
//!
//! [`LiveFaces`]: while the row shows, a lit camera button shows the camera picture (the
//! webcam service's preview file), a lit mic button a spectrum of the microphone, a lit
//! VPN button a graph of its traffic, and the meters (CPU, memory, disk, network, battery)
//! their graphs, under their icons. The battery button is lit while on external power
//! (unless it has its own status command).

use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::ExtraConfig;
use crate::framebuffer::Rectangle;
use crate::live::{AudioFeed, AudioSource, CameraPreview, Spectrum};
use crate::meters::{self, Meter};
use crate::stock_slice::ExtraIcon;

const VISIBLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// A live feed whose process died (e.g. the webcam loopback had no picture yet) is
/// restarted after this long.
const FEED_RETRY: Duration = Duration::from_secs(1);
/// How often the meters sample (each sample is a 2 px column, so ~23 s fit a plate).
const METER_INTERVAL: Duration = Duration::from_millis(250);
/// Mic spectrum bars: pitch and width in pixels, and the inset from the plate's ends.
const MIC_BAR_PITCH: usize = 7;
const MIC_BAR_WIDTH: usize = 5;
const MIC_INSET: usize = 6;
/// Mic face colours as XRGB8888 bytes (B, G, R, X): a darker shade of the lit plate's
/// green, bars from that green (bottom) to pale green (top), and pale peak markers.
const MIC_BACKGROUND: [u8; 4] = [30, 56, 16, 0];
const MIC_BAR_BOTTOM: [f32; 3] = [92.0, 164.0, 58.0];
const MIC_BAR_TOP: [f32; 3] = [170.0, 235.0, 150.0];
const MIC_PEAK: [u8; 4] = [225, 255, 220, 0];

enum Request {
    Visible(bool),
    Run(usize),
    Refresh,
}

/// Handle to the extras worker thread. Cheap to clone.
#[derive(Clone)]
pub struct Extras {
    buttons: Arc<[ExtraConfig]>,
    icons: Arc<[ExtraIcon]>,
    requests: Sender<Request>,
    /// Bit `i` set: button `i` is lit.
    latest: Arc<Mutex<u16>>,
}

impl Extras {
    #[must_use]
    pub fn start(buttons: Vec<ExtraConfig>) -> Self {
        let (requests, receiver) = mpsc::channel();
        let latest = Arc::new(Mutex::new(0));
        let icons = buttons.iter().map(|button| button.icon).collect();
        let buttons: Arc<[ExtraConfig]> = buttons.into();
        let worker = Worker {
            buttons: Arc::clone(&buttons),
            requests: requests.clone(),
            latest: Arc::clone(&latest),
        };
        thread::spawn(move || worker.run(&receiver));
        Self {
            buttons,
            icons,
            requests,
            latest,
        }
    }

    #[must_use]
    pub fn icons(&self) -> &[ExtraIcon] {
        &self.icons
    }

    #[must_use]
    pub fn buttons(&self) -> &[ExtraConfig] {
        &self.buttons
    }

    pub fn set_visible(&self, visible: bool) {
        let _ = self.requests.send(Request::Visible(visible));
    }

    pub fn run(&self, index: usize) {
        let _ = self.requests.send(Request::Run(index));
    }

    /// The last probed state, bit `i` for button `i`.
    #[must_use]
    pub fn status(&self) -> u16 {
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Worker {
    buttons: Arc<[ExtraConfig]>,
    requests: Sender<Request>,
    latest: Arc<Mutex<u16>>,
}

impl Worker {
    fn run(self, receiver: &mpsc::Receiver<Request>) {
        let mut visible = false;
        let mut last = self.probe();
        self.publish(last);
        loop {
            let request = if visible {
                receiver.recv_timeout(VISIBLE_POLL_INTERVAL)
            } else {
                receiver.recv().map_err(|_| RecvTimeoutError::Disconnected)
            };
            match request {
                Ok(Request::Visible(now_visible)) => {
                    visible = now_visible;
                    if !visible {
                        continue;
                    }
                }
                Ok(Request::Run(index)) => {
                    let Some(button) = self.buttons.get(index) else {
                        continue;
                    };
                    if button.command.is_empty() {
                        continue;
                    }
                    let command = button.command.clone();
                    let requests = self.requests.clone();
                    thread::spawn(move || {
                        run_command(&command);
                        let _ = requests.send(Request::Refresh);
                    });
                    continue;
                }
                Ok(Request::Refresh) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            let status = self.probe();
            if status != last {
                last = status;
                self.publish(status);
            }
        }
    }

    fn probe(&self) -> u16 {
        (0..)
            .zip(self.buttons.iter())
            .filter(|(_, button)| match button.status.as_deref() {
                Some(status) => succeeds(status),
                None => button.icon == ExtraIcon::Battery && meters::on_external_power(),
            })
            .fold(0, |bits, (index, _)| bits | (1 << index))
    }

    fn publish(&self, status: u16) {
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }
}

fn shell(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command).stdin(Stdio::null());
    shell
}

fn succeeds(command: &str) -> bool {
    shell(command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Runs a button's command; its stderr and any failure go to the journal.
fn run_command(command: &str) {
    match shell(command).stdout(Stdio::null()).status() {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("fnstrip: `{command}` exited with {status}"),
        Err(error) => eprintln!("fnstrip: could not run `{command}`: {error}"),
    }
}

/// Feeds behind the live faces of lit camera, mic and VPN buttons and of the meters.
/// Started when the extras row shows (with such a button lit), stopped when it hides or
/// the button goes dark.
#[derive(Default)]
pub(crate) struct LiveFaces {
    camera: Option<(u8, CameraPreview)>,
    mic: Option<Slot<(AudioFeed, Spectrum)>>,
    meters: Vec<(u8, Meter)>,
    meters_sampled: Option<Instant>,
    /// A meter took a sample since the last [`LiveFaces::take_fresh`].
    fresh: bool,
}

/// One button's feed; `None` after a failed start, retried after [`FEED_RETRY`].
struct Slot<T> {
    index: u8,
    feed: Option<T>,
    started: Instant,
}

impl LiveFaces {
    /// Starts or stops the feeds for this row state (`on`: bit `i` = button `i` lit) and
    /// returns the buttons that have a live face to draw (bit `i` = button `i`).
    pub(crate) fn sync(
        &mut self,
        visible: bool,
        buttons: &[ExtraConfig],
        on: u16,
        plate: (usize, usize),
    ) -> u16 {
        let lit = |wanted: ExtraIcon| {
            (0_u8..)
                .zip(buttons)
                .find(|&(index, button)| visible && button.icon == wanted && on & (1 << index) != 0)
                .map(|(index, _)| index)
        };
        let camera = lit(ExtraIcon::Camera);
        if self.camera.as_ref().map(|(index, _)| *index) != camera {
            self.camera = camera.map(|index| (index, CameraPreview::new()));
        }
        if let Some((_, preview)) = &mut self.camera {
            preview.refresh(plate.0 * plate.1 * 4);
        }
        let bars = plate.0.saturating_sub(2 * MIC_INSET) / MIC_BAR_PITCH;
        keep(
            &mut self.mic,
            lit(ExtraIcon::Mic),
            || AudioFeed::start(AudioSource::Input).map(|feed| (feed, Spectrum::new(bars))),
            |(feed, _)| feed.ended(),
        );
        self.sync_meters(visible, buttons, on, plate.0);
        let mut live = self
            .meters
            .iter()
            .fold(0, |bits, (index, _)| bits | (1 << index));
        if let Some((index, preview)) = &self.camera
            && !preview.frame.is_empty()
        {
            live |= 1 << index;
        }
        if let Some(Slot {
            index,
            feed: Some(_),
            ..
        }) = &self.mic
        {
            live |= 1 << index;
        }
        live
    }

    /// Keeps a meter for every meter button and the lit VPN button, and samples them all
    /// every [`METER_INTERVAL`].
    fn sync_meters(&mut self, visible: bool, buttons: &[ExtraConfig], on: u16, width: usize) {
        let wanted = (0_u8..).zip(buttons).filter(|&(index, button)| {
            visible
                && (button.icon.is_meter()
                    || (button.icon == ExtraIcon::Vpn && on & (1 << index) != 0))
        });
        if !wanted
            .clone()
            .map(|(index, _)| index)
            .eq(self.meters.iter().map(|(index, _)| *index))
        {
            let mut old = std::mem::take(&mut self.meters);
            self.meters = wanted
                .filter_map(|(index, button)| {
                    let kept = old
                        .iter()
                        .position(|(old_index, _)| *old_index == index)
                        .map(|position| old.swap_remove(position).1);
                    kept.or_else(|| Meter::new(button.icon, button.interface.as_deref(), width))
                        .map(|meter| (index, meter))
                })
                .collect();
            self.meters_sampled = None;
        }
        if self.meters.is_empty()
            || self
                .meters_sampled
                .is_some_and(|sampled| sampled.elapsed() < METER_INTERVAL)
        {
            return;
        }
        self.meters_sampled = Some(Instant::now());
        for (_, meter) in &mut self.meters {
            meter.sample();
        }
        self.fresh = true;
    }

    /// The battery meter's charge in quarters, once it has a reading.
    pub(crate) fn battery_bars(&self) -> Option<u8> {
        self.meters
            .iter()
            .find_map(|(_, meter)| meter.battery_bars())
    }

    /// True once after the meters took a sample, so their faces need a redraw.
    pub(crate) const fn take_fresh(&mut self) -> bool {
        let fresh = self.fresh;
        self.fresh = false;
        fresh
    }

    /// True while a face changes continuously (camera picture, mic spectrum).
    pub(crate) const fn animating(&self) -> bool {
        self.camera.is_some() || matches!(self.mic, Some(Slot { feed: Some(_), .. }))
    }

    /// Draws button `index`'s live face into its plate rectangle of an XRGB8888 frame.
    pub(crate) fn draw(&mut self, pixels: &mut [u8], stride: usize, index: u8, bounds: Rectangle) {
        let (left, top) = (bounds.x as usize, bounds.y as usize);
        let (width, height) = (bounds.width as usize, bounds.height as usize);
        let rows = pixels
            .chunks_exact_mut(stride)
            .skip(top)
            .take(height)
            .map(|row| &mut row[left * 4..(left + width) * 4]);
        if let Some((camera_index, preview)) = &self.camera
            && *camera_index == index
        {
            let frame = &preview.frame;
            if frame.len() != width * height * 4 {
                return;
            }
            for (row, source) in rows.zip(frame.chunks_exact(width * 4)) {
                row.copy_from_slice(source);
            }
        } else if let Some(Slot {
            index: mic_index,
            feed: Some((feed, spectrum)),
            ..
        }) = &mut self.mic
            && *mic_index == index
        {
            if let Ok(samples) = feed.samples.lock() {
                spectrum.analyse(&samples);
            }
            spectrum.settle();
            draw_mic_spectrum(rows, spectrum, width, height);
        } else if let Some((_, meter)) = self.meters.iter().find(|(i, _)| *i == index) {
            meter.draw(rows, width, height);
        }
    }
}

/// Keeps `slot` on button `wanted` (or empty), restarting a dead feed after a pause.
fn keep<T>(
    slot: &mut Option<Slot<T>>,
    wanted: Option<u8>,
    start: impl Fn() -> Option<T>,
    ended: impl Fn(&mut T) -> bool,
) {
    if slot.as_ref().map(|slot| slot.index) != wanted {
        // Drop (and stop) the old feed before starting a new one.
        *slot = None;
        *slot = wanted.map(|index| Slot {
            index,
            feed: start(),
            started: Instant::now(),
        });
        return;
    }
    if let Some(slot) = slot
        && slot.feed.as_mut().is_none_or(&ended)
        && slot.started.elapsed() >= FEED_RETRY
    {
        slot.feed = None;
        slot.feed = start();
        slot.started = Instant::now();
    }
}

// Pixel maths is f32; the casts are of small in-range values.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub(crate) fn draw_mic_spectrum<'a>(
    rows: impl Iterator<Item = &'a mut [u8]>,
    spectrum: &Spectrum,
    width: usize,
    height: usize,
) {
    let bars = spectrum.levels.len();
    let first = (width - bars * MIC_BAR_PITCH) / 2 + (MIC_BAR_PITCH - MIC_BAR_WIDTH) / 2;
    let full = height as f32;
    for (y, row) in rows.enumerate() {
        let from_bottom = (height - y) as f32;
        let shade = from_bottom / full;
        let bar_colour = std::array::from_fn::<f32, 3, _>(|channel| {
            MIC_BAR_BOTTOM[channel] + (MIC_BAR_TOP[channel] - MIC_BAR_BOTTOM[channel]) * shade
        });
        let bar_colour = [
            bar_colour[0] as u8,
            bar_colour[1] as u8,
            bar_colour[2] as u8,
            0,
        ];
        for pixel in row.as_chunks_mut::<4>().0 {
            *pixel = MIC_BACKGROUND;
        }
        for (bar, (&level, &peak)) in spectrum.levels.iter().zip(&spectrum.peaks).enumerate() {
            let peak_row = (peak * full).round();
            let colour = if (from_bottom - peak_row).abs() < 1.0 && peak > 0.02 {
                MIC_PEAK
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
