//! fnstrip's own drawing code for the project page: the page calls these functions and
//! shows the pixels, so its bar is drawn exactly as the renderer draws the real one.
//!
//! Exported with C names for WebAssembly, so the page needs no generated glue. The page
//! sets the state (theme, colours, extras buttons, levels, the pressed button) and feeds
//! the live faces (meter samples, audio, the camera picture), then asks for a row; each
//! `row` or `intro` call returns where its RGBA frame (2170x60) starts.

use std::cell::RefCell;
use std::time::Duration;

use fnstrip::framebuffer::{Rectangle, Rgb, Xrgb8888Frame};
use fnstrip::intro::Intro;
use fnstrip::meters::Meter;
use fnstrip::screensaver::{Screensaver, Zone};
use fnstrip::spectrum::{AudioSource, FFT_SIZE, Spectrum, draw_mic_spectrum, face_bars};
use fnstrip::stock_slice::{
    ExtraIcon, FunctionKey, Layer, StockAction, StockBar, StockCapabilities, StockLevels,
};
use fnstrip::theme::{FaceTint, Theme};

const WIDTH: u32 = 2_170;
const HEIGHT: u32 = 60;
const BYTES: usize = (WIDTH * HEIGHT * 4) as usize;
/// An extras plate (and the play/pause button): 182x60.
const PLATE: usize = 182;
/// `ExtraIcon::NAMES` indexes the page feeds by name.
const BATTERY: usize = 7;
const TEMPERATURE: usize = 8;
const FAN: usize = 9;

struct Page {
    theme: Theme,
    extras: Vec<ExtraIcon>,
    levels: StockLevels,
    pressed: Option<StockAction>,
    /// One meter per meter icon (and the VPN), by `ExtraIcon::NAMES` index, fed by the page.
    meters: Vec<Option<Meter>>,
    /// Whether the battery is on power (for its lit look).
    powered: bool,
    /// The mic and the speakers (what's playing): their latest samples and spectra.
    audio: [(Vec<f32>, Spectrum); 2],
    /// The camera picture for its button, RGBA from the page, and whether there is one.
    camera: Vec<u8>,
    camera_ready: bool,
    /// The test screen while it runs, its frame (XRGB8888, kept between frames: some scenes
    /// redraw only parts), the last frame id handed out, and the camera picture it wants.
    test: Option<Screensaver>,
    test_pixels: Vec<u8>,
    test_frame: u64,
    test_camera: Vec<u8>,
    /// The last row drawn (XRGB8888), for the startup animation to play over.
    row: Vec<u8>,
    /// What the page reads: RGBA.
    out: Vec<u8>,
    intro: Intro,
}

thread_local! {
    static PAGE: RefCell<Page> = RefCell::new(Page {
        theme: Theme::DEFAULT,
        extras: Vec::new(),
        levels: StockLevels::default(),
        pressed: None,
        meters: (0..ExtraIcon::NAMES.len())
            .map(|index| icon_at(index).and_then(|icon| Meter::new(icon, None, PLATE)))
            .collect(),
        powered: false,
        audio: [
            (vec![0.0; FFT_SIZE], Spectrum::new(face_bars(PLATE))),
            (vec![0.0; FFT_SIZE], Spectrum::new(face_bars(PLATE))),
        ],
        camera: vec![0; PLATE * HEIGHT as usize * 4],
        camera_ready: false,
        test: None,
        test_pixels: vec![0; BYTES],
        test_frame: 0,
        test_camera: Vec::new(),
        row: vec![0; BYTES],
        out: vec![0; BYTES],
        intro: Intro::new(WIDTH, HEIGHT),
    });
}

// The page's clock (`performance.now()`, imported as `env.now_ms`): WebAssembly has none.
#[link(wasm_import_module = "env")]
unsafe extern "C" {
    safe fn now_ms() -> f64;
}

fn page_now() -> f64 {
    now_ms()
}

/// Call once after loading: gives fnstrip's code the page's clock.
#[unsafe(no_mangle)]
pub extern "C" fn start() {
    fnstrip::clock::set_source(page_now);
}

/// Theme `index` (in `Theme::NAMES` order), with no colour changes.
#[unsafe(no_mangle)]
pub extern "C" fn theme(index: u32) {
    let name = Theme::NAMES
        .get(index as usize)
        .copied()
        .unwrap_or("default");
    PAGE.with_borrow_mut(|page| page.theme = Theme::named(name).unwrap_or_default());
}

/// One `[colors]` change on top of the theme: `key` in `Theme::COLOR_NAMES` order, the
/// colour as 0xRRGGBB.
#[unsafe(no_mangle)]
pub extern "C" fn color(key: u32, rgb: u32) {
    let Some(name) = Theme::COLOR_NAMES.get(key as usize) else {
        return;
    };
    PAGE.with_borrow_mut(|page| {
        let _ = page.theme.set(name, &format!("#{:06x}", rgb & 0x00ff_ffff));
    });
}

/// The extras buttons, left to right in config order: `count` icons, each a 4-bit index
/// into `ExtraIcon::NAMES`, the first five in `first` (lowest bits first), the rest in
/// `rest`.
#[unsafe(no_mangle)]
pub extern "C" fn extras(count: u32, first: u32, rest: u32) {
    let icons = (0..count.min(10))
        .filter_map(|index| {
            let packed = if index < 5 { first } else { rest };
            icon_at(((packed >> ((index % 5) * 4)) & 0xf) as usize)
        })
        .collect();
    PAGE.with_borrow_mut(|page| page.extras = icons);
}

/// The levels, in percent (above 100: none). `flags`: bit 0 muted, bit 1 media playing,
/// bit 2 play/pause shows the speakers' spectrum.
#[unsafe(no_mangle)]
pub extern "C" fn levels(brightness: u32, keyboard: u32, volume: u32, flags: u32) {
    let percent = |value: u32| u8::try_from(value).ok().filter(|value| *value <= 100);
    PAGE.with_borrow_mut(|page| {
        page.levels.display_brightness = percent(brightness);
        page.levels.keyboard_backlight = percent(keyboard);
        page.levels.volume = percent(volume);
        page.levels.muted = flags & 1 != 0;
        page.levels.playing = flags & 2 != 0;
        page.levels.media_live = flags & 4 != 0;
    });
}

/// The extras row's state: bit `i` of `on` lights button `i` (the battery lights itself on
/// power); bit `i` of `live` shows its live face (meters, a lit VPN, camera, mic).
#[unsafe(no_mangle)]
pub extern "C" fn extras_state(on: u32, live: u32) {
    PAGE.with_borrow_mut(|page| {
        page.levels.extras_on = u16::try_from(on & 0x3ff).unwrap_or(0);
        page.levels.extras_live = u16::try_from(live & 0x3ff).unwrap_or(0);
    });
}

/// The button being touched: 0 none, 1 Esc, 2 previous, 3 play/pause, 4 next, 10 + n for
/// Fn key n (1 to 12), 100 + i for extras button i.
#[unsafe(no_mangle)]
pub extern "C" fn pressed(code: u32) {
    const KEYS: [FunctionKey; 12] = [
        FunctionKey::F1,
        FunctionKey::F2,
        FunctionKey::F3,
        FunctionKey::F4,
        FunctionKey::F5,
        FunctionKey::F6,
        FunctionKey::F7,
        FunctionKey::F8,
        FunctionKey::F9,
        FunctionKey::F10,
        FunctionKey::F11,
        FunctionKey::F12,
    ];
    let action = match code {
        1 => Some(StockAction::Escape),
        2 => Some(StockAction::MediaPrevious),
        3 => Some(StockAction::MediaPlayPause),
        4 => Some(StockAction::MediaNext),
        11..=22 => Some(StockAction::Function(KEYS[(code - 11) as usize])),
        100..=109 => u8::try_from(code - 100).ok().map(StockAction::Extra),
        _ => None,
    };
    PAGE.with_borrow_mut(|page| page.pressed = action);
}

// ---- Feeding the live faces ---------------------------------------------------------------

/// A sample for meter `icon` (`ExtraIcon::NAMES` index), in the meter's own units (see
/// `Meter::push_sample`).
#[unsafe(no_mangle)]
pub extern "C" fn meter_sample(icon: u32, first: f64, second: f64) {
    with_meter(icon as usize, |meter| meter.push_sample([first, second]));
}

/// A temperature sample in °C.
#[unsafe(no_mangle)]
pub extern "C" fn meter_celsius(celsius: f64) {
    with_meter(TEMPERATURE, |meter| meter.push_celsius(celsius));
}

/// The battery: charge 0 to 1, on power (0 or 1), hours to empty or full (below 0: not
/// known). Also adds the charge as a sample.
#[unsafe(no_mangle)]
pub extern "C" fn battery(level: f64, powered: u32, hours: f64) {
    with_meter(BATTERY, |meter| {
        meter.set_battery(level, powered != 0, (hours >= 0.0).then_some(hours));
        meter.push_sample([level, 0.0]);
    });
    PAGE.with_borrow_mut(|page| page.powered = powered != 0);
}

/// The fans' ranges in rpm.
#[unsafe(no_mangle)]
pub extern "C" fn fan_ranges(min1: f64, max1: f64, min2: f64, max2: f64) {
    with_meter(FAN, |meter| {
        meter.set_fan_ranges(&[(min1, max1), (min2, max2)]);
    });
}

/// The temperature in °F (1) or °C (0).
#[unsafe(no_mangle)]
pub extern "C" fn fahrenheit(on: u32) {
    PAGE.with_borrow_mut(|page| {
        if let Some(slot) = page.meters.get_mut(TEMPERATURE)
            && let Some(meter) = slot.take()
        {
            *slot = Some(meter.in_fahrenheit(on != 0));
        }
    });
}

/// Where the page writes the latest `FFT_SIZE` samples (mono, 48 kHz) of `which` audio:
/// 0 the mic, 1 the speakers.
#[unsafe(no_mangle)]
pub extern "C" fn audio_buffer(which: u32) -> *mut f32 {
    PAGE.with_borrow_mut(|page| page.audio[(which as usize).min(1)].0.as_mut_ptr())
}

/// Analyses `which` audio's samples, `dt` seconds after the last time.
#[unsafe(no_mangle)]
pub extern "C" fn audio_update(which: u32, dt: f32) {
    PAGE.with_borrow_mut(|page| {
        let which = (which as usize).min(1);
        let (samples, spectrum) = &mut page.audio[which];
        spectrum.analyse(samples);
        spectrum.settle_by(dt);
        // The test screen's spectrum scenes listen to the same audio.
        let source = [AudioSource::Input, AudioSource::Speakers][which];
        fnstrip::page_feeds::set_samples(source, samples);
    });
}

/// Where the page writes the camera picture for its button: RGBA, 182x60.
#[unsafe(no_mangle)]
pub extern "C" fn camera_buffer() -> *mut u8 {
    PAGE.with_borrow_mut(|page| page.camera.as_mut_ptr())
}

/// Whether the camera buffer holds a picture.
#[unsafe(no_mangle)]
pub extern "C" fn camera_ready(ready: u32) {
    PAGE.with_borrow_mut(|page| page.camera_ready = ready != 0);
}

// ---- Drawing ------------------------------------------------------------------------------

/// Draws row `layer` (0 controls, 1 F-keys, 2 extras), live faces included, and returns its
/// RGBA frame.
#[unsafe(no_mangle)]
pub extern "C" fn row(layer: u32) -> *const u8 {
    let layer = match layer {
        1 => Layer::Function,
        2 => Layer::Extras,
        _ => Layer::Controls,
    };
    PAGE.with_borrow_mut(|page| {
        let all = StockCapabilities::DISPLAY_BRIGHTNESS
            | StockCapabilities::KEYBOARD_BACKLIGHT
            | StockCapabilities::AUDIO
            | StockCapabilities::MEDIA;
        let bar = StockBar::new(WIDTH, HEIGHT, all)
            .expect("bar size")
            .with_theme(page.theme)
            .with_extras(&page.extras);
        let mut levels = page.levels;
        // The battery shows its charge in its icon, and lights itself on power.
        if let Some(bars) = page.meters[BATTERY].as_ref().and_then(Meter::battery_bars) {
            levels.battery_bars = bars;
            if page.powered
                && let Some(index) = page.extras.iter().position(|i| *i == ExtraIcon::Battery)
            {
                levels.extras_on |= 1 << index;
            }
        }
        if layer != Layer::Extras {
            levels.extras_live = 0;
        }
        let pressed = page.pressed;
        draw(&mut page.row, |frame| {
            bar.render_pressed(frame, layer, levels, pressed)
        });
        if levels.extras_live != 0 {
            for (index, icon) in page.extras.clone().into_iter().enumerate() {
                let index = u8::try_from(index).unwrap_or(u8::MAX);
                if levels.extras_live & (1 << index) != 0
                    && let Some(bounds) = bar.extra_bounds(index)
                {
                    page.draw_face(icon, bounds);
                }
            }
            draw(&mut page.row, |frame| {
                bar.render_live_extras(frame, levels, pressed)
            });
        }
        if layer == Layer::Controls
            && levels.media_live
            && let Some(bounds) = bar.play_pause_bounds()
        {
            let tint = page.theme.face_tint(bounds.x, WIDTH);
            let (width, height) = (bounds.width as usize, bounds.height as usize);
            let spectrum = &page.audio[1].1;
            draw_mic_spectrum(
                plate_rows(&mut page.row, bounds),
                spectrum,
                width,
                height,
                tint,
            );
            draw(&mut page.row, |frame| bar.render_live_media(frame, pressed));
        }
        to_rgba(&page.row, &mut page.out);
        page.out.as_ptr()
    })
}

impl Page {
    /// Extras button `icon`'s live face into its plate, as the renderer's live faces do.
    fn draw_face(&mut self, icon: ExtraIcon, bounds: Rectangle) {
        let tint = self.theme.face_tint(bounds.x, WIDTH);
        let (width, height) = (bounds.width as usize, bounds.height as usize);
        match icon {
            ExtraIcon::Camera => {
                if !self.camera_ready || width != PLATE {
                    return;
                }
                let rows = plate_rows(&mut self.row, bounds);
                for (row, source) in rows.zip(self.camera.as_chunks::<{ PLATE * 4 }>().0) {
                    for (pixel, rgba) in row
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(source.as_chunks::<4>().0)
                    {
                        let xrgb = [rgba[2], rgba[1], rgba[0], 0];
                        // A picture keeps its colours, except grey for mono.
                        *pixel = if tint == FaceTint::Grey {
                            tint.apply(xrgb)
                        } else {
                            xrgb
                        };
                    }
                }
            }
            ExtraIcon::Mic => {
                let spectrum = &self.audio[0].1;
                let rows = plate_rows(&mut self.row, bounds);
                draw_mic_spectrum(rows, spectrum, width, height, tint);
            }
            _ => {
                let Some(meter) = self
                    .meters
                    .get_mut(icon_index(icon))
                    .and_then(Option::as_mut)
                else {
                    return;
                };
                meter.set_tint(tint);
                meter.draw(plate_rows(&mut self.row, bounds), width, height);
            }
        }
    }
}

// ---- The test screen (triple-tap Fn and hold): the renderer's own -------------------------

/// Starts the test screen (as the bar does: up to three frames in flight).
#[unsafe(no_mangle)]
pub extern "C" fn test_start() {
    PAGE.with_borrow_mut(|page| {
        page.test = Some(Screensaver::new(WIDTH, HEIGHT, 3));
        page.test_pixels.fill(0);
    });
}

/// The test screen's next frame (RGBA), or null when it isn't running.
#[unsafe(no_mangle)]
pub extern "C" fn test_frame() -> *const u8 {
    PAGE.with_borrow_mut(|page| {
        let Some(test) = page.test.as_mut() else {
            return std::ptr::null();
        };
        // The browser shows a frame at the next paint: the previous one is "released" now.
        if page.test_frame > 0 {
            test.released(page.test_frame);
        }
        test.render(&mut page.test_pixels);
        page.test_frame += 1;
        test.submitted(page.test_frame);
        to_rgba(&page.test_pixels, &mut page.out);
        page.out.as_ptr()
    })
}

/// Advances the test screen's clock work (warm-up, the stats window).
#[unsafe(no_mangle)]
pub extern "C" fn test_tick() {
    PAGE.with_borrow_mut(|page| {
        if let Some(test) = page.test.as_mut() {
            test.tick();
        }
    });
}

/// What a tap at `x` does: 0 next scene, 1 next buffer count, 2 leave.
#[unsafe(no_mangle)]
pub extern "C" fn test_zone(x: u32) -> u32 {
    PAGE.with_borrow(|page| match page.test.as_ref().map(|test| test.zone(x)) {
        Some(Zone::NextScene) => 0,
        Some(Zone::NextBuffering) => 1,
        _ => 2,
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn test_next_scene() {
    PAGE.with_borrow_mut(|page| page.test.as_mut().map(Screensaver::next_scene));
}

#[unsafe(no_mangle)]
pub extern "C" fn test_next_buffering() {
    PAGE.with_borrow_mut(|page| page.test.as_mut().map(Screensaver::next_buffering));
}

/// Closes the test screen.
#[unsafe(no_mangle)]
pub extern "C" fn test_stop() {
    PAGE.with_borrow_mut(|page| {
        if let Some(mut test) = page.test.take() {
            test.finish();
        }
    });
}

/// What the current scene wants from the page: bit 0 the mic, bit 1 the speakers (what's
/// playing), bit 2 the camera (at `test_camera_size`).
#[unsafe(no_mangle)]
pub extern "C" fn test_wants() -> u32 {
    PAGE.with_borrow(|page| {
        let audio = match page.test.as_ref().and_then(Screensaver::listening_to) {
            Some(AudioSource::Input) => 1,
            Some(AudioSource::Speakers) => 2,
            None => 0,
        };
        audio
            | if fnstrip::page_feeds::camera_size().is_some() {
                4
            } else {
                0
            }
    })
}

/// The camera picture size the test screen wants, as width << 16 | height (0: none).
#[unsafe(no_mangle)]
pub extern "C" fn test_camera_size() -> u32 {
    fnstrip::page_feeds::camera_size().map_or(0, |(width, height)| {
        u32::try_from(width).unwrap_or(0) << 16 | u32::try_from(height).unwrap_or(0)
    })
}

/// Where the page writes the test screen's camera picture (RGBA at `test_camera_size`).
#[unsafe(no_mangle)]
pub extern "C" fn test_camera_buffer() -> *mut u8 {
    PAGE.with_borrow_mut(|page| {
        let size =
            fnstrip::page_feeds::camera_size().map_or(0, |(width, height)| width * height * 4);
        page.test_camera.resize(size, 0);
        page.test_camera.as_mut_ptr()
    })
}

/// The camera picture is written: hand it to the test screen.
#[unsafe(no_mangle)]
pub extern "C" fn test_camera_commit() {
    PAGE.with_borrow(|page| fnstrip::page_feeds::set_camera_frame(&page.test_camera));
}

/// The startup animation at `ms` milliseconds, over the last row drawn; RGBA frame.
#[unsafe(no_mangle)]
pub extern "C" fn intro(ms: u32) -> *const u8 {
    PAGE.with_borrow_mut(|page| {
        let mut pixels = page.row.clone();
        page.intro
            .draw_at(&mut pixels, Duration::from_millis(u64::from(ms)));
        to_rgba(&pixels, &mut page.out);
        page.out.as_ptr()
    })
}

/// The startup animation's length, in milliseconds.
#[unsafe(no_mangle)]
pub extern "C" fn intro_ms() -> u32 {
    u32::try_from(fnstrip::intro::DURATION.as_millis()).unwrap_or(1_800)
}

/// The background colour of the current theme, as 0xRRGGBB (for the page's own fills).
#[unsafe(no_mangle)]
pub extern "C" fn background() -> u32 {
    PAGE.with_borrow(|page| {
        let Rgb { red, green, blue } = page.theme.background;
        u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue)
    })
}

/// Runs a stock bar drawing call on the whole frame.
fn draw<E: std::fmt::Debug>(
    pixels: &mut [u8],
    call: impl FnOnce(&mut Xrgb8888Frame<'_>) -> Result<(), E>,
) {
    let mut frame = Xrgb8888Frame::new(WIDTH, HEIGHT, WIDTH * 4, pixels).expect("frame");
    call(&mut frame).expect("draw");
}

fn icon_at(index: usize) -> Option<ExtraIcon> {
    ExtraIcon::NAMES
        .get(index)
        .and_then(|name| ExtraIcon::from_name(name))
}

fn icon_index(icon: ExtraIcon) -> usize {
    (0..ExtraIcon::NAMES.len())
        .find(|&index| icon_at(index) == Some(icon))
        .unwrap_or(usize::MAX)
}

fn with_meter(index: usize, change: impl FnOnce(&mut Meter)) {
    PAGE.with_borrow_mut(|page| {
        if let Some(meter) = page.meters.get_mut(index).and_then(Option::as_mut) {
            change(meter);
        }
    });
}

/// The rows of `bounds` in an XRGB8888 frame `WIDTH` pixels wide.
fn plate_rows(pixels: &mut [u8], bounds: Rectangle) -> impl Iterator<Item = &mut [u8]> {
    let (left, top) = (bounds.x as usize, bounds.y as usize);
    let (width, height) = (bounds.width as usize, bounds.height as usize);
    pixels
        .as_chunks_mut::<{ WIDTH as usize * 4 }>()
        .0
        .iter_mut()
        .skip(top)
        .take(height)
        .map(move |row| &mut row[left * 4..(left + width) * 4])
}

/// XRGB8888 (B, G, R, X) to the browser's RGBA.
fn to_rgba(xrgb: &[u8], rgba: &mut [u8]) {
    for (from, to) in xrgb
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rgba.as_chunks_mut::<4>().0)
    {
        *to = [from[2], from[1], from[0], 255];
    }
}
