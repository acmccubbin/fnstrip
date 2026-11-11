//! Renders the frames behind the pictures in `docs/media`, with the real drawing code and made-up
//! data (no real camera picture, levels or traffic). Run through `tools/make_demo_media.py`.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::fs::{self, File};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use crate::extras::draw_mic_spectrum;
use crate::framebuffer::Xrgb8888Frame;
use crate::intro::{self, Intro};
use crate::live::Spectrum;
use crate::meters::Meter;
use crate::screensaver::Screensaver;
use crate::stock_slice::{ExtraIcon, Layer, StockBar, StockCapabilities, StockLevels};
use crate::theme::{FaceTint, Theme};

const WIDTH: u32 = 2_170;
const HEIGHT: u32 = 60;
const FRAME_BYTES: usize = (WIDTH * HEIGHT * 4) as usize;
const FRAME_TIME: Duration = Duration::from_millis(66);

fn output() -> PathBuf {
    PathBuf::from(std::env::var_os("FNSTRIP_DEMO_DIR").expect("FNSTRIP_DEMO_DIR"))
}

fn bar() -> StockBar {
    let all = StockCapabilities::DISPLAY_BRIGHTNESS
        | StockCapabilities::KEYBOARD_BACKLIGHT
        | StockCapabilities::AUDIO
        | StockCapabilities::MEDIA;
    StockBar::new(WIDTH, HEIGHT, all).unwrap()
}

fn draw_row(layer: Layer, levels: StockLevels) -> Vec<u8> {
    let mut pixels = vec![0; FRAME_BYTES];
    let mut frame = Xrgb8888Frame::new(WIDTH, HEIGHT, WIDTH * 4, &mut pixels).unwrap();
    bar()
        .render_pressed(&mut frame, layer, levels, None)
        .unwrap();
    pixels
}

fn levels(brightness: u8, keyboard: u8, volume: u8, muted: bool) -> StockLevels {
    StockLevels {
        display_brightness: Some(brightness),
        keyboard_backlight: Some(keyboard),
        volume: Some(volume),
        muted,
        ..StockLevels::default()
    }
}

/// `from` to `to` in `frames` steps.
fn sweep(from: f64, to: f64, frames: u32) -> impl Iterator<Item = u8> {
    (0..frames).map(move |frame| {
        let eased = f64::from(frame) / f64::from(frames.max(2) - 1);
        u8::try_from((from + (to - from) * eased).round() as i64).unwrap()
    })
}

/// Plate x and width (native) of the slider that `kind` numbers: 0 brightness, 1 keyboard, 2 volume.
const SLIDERS: [(f64, f64); 3] = [(194.0, 372.0), (602.0, 372.0), (1_608.0, 562.0)];

/// The sliders being used; `sliders.txt` holds the finger's x per frame ("-" for none).
fn render_sliders() {
    let mut pixels = File::create(output().join("sliders.raw")).unwrap();
    let mut fingers = File::create(output().join("sliders.txt")).unwrap();
    let mut state = (35_u8, 50_u8, 30_u8, false);
    let mut emit = |state: (u8, u8, u8, bool), finger: Option<f64>| {
        pixels
            .write_all(&draw_row(
                Layer::Controls,
                levels(state.0, state.1, state.2, state.3),
            ))
            .unwrap();
        match finger {
            Some(x) => writeln!(fingers, "{x:.0}").unwrap(),
            None => writeln!(fingers, "-").unwrap(),
        }
    };
    let at =
        |slider: usize, level: u8| SLIDERS[slider].0 + SLIDERS[slider].1 * f64::from(level) / 100.0;
    for _ in 0..6 {
        emit(state, None);
    }
    for (from, to) in [(30.0, 85.0), (85.0, 55.0)] {
        for level in sweep(from, to, 18) {
            state.2 = level;
            emit(state, Some(at(2, level)));
        }
    }
    for _ in 0..4 {
        emit(state, None);
    }
    // Tap the speaker icon: mute, then unmute.
    for muted in [true, false] {
        for _ in 0..3 {
            emit(state, Some(1_640.0));
        }
        state.3 = muted;
        for _ in 0..10 {
            emit(state, None);
        }
    }
    for (from, to) in [(35.0, 80.0), (80.0, 60.0)] {
        for level in sweep(from, to, 15) {
            state.0 = level;
            emit(state, Some(at(0, level)));
        }
    }
    for _ in 0..4 {
        emit(state, None);
    }
    for (from, to) in [(50.0, 10.0), (10.0, 70.0)] {
        for level in sweep(from, to, 15) {
            state.1 = level;
            emit(state, Some(at(1, level)));
        }
    }
    for _ in 0..8 {
        emit(state, None);
    }
}

/// Sample data for a meter, varying with `step`.
fn sample(icon: ExtraIcon, step: u32) -> [f64; 2] {
    let t = f64::from(step);
    let wave = |period: f64, phase: f64| f64::midpoint((t / period + phase).sin(), 1.0);
    match icon {
        ExtraIcon::Cpu => [0.12 + 0.3 * wave(9.0, 0.0), 0.04 + 0.1 * wave(5.0, 1.0)],
        ExtraIcon::Memory => [0.38 + 0.03 * wave(40.0, 0.0), 0.27],
        // Fractions of the 100 °C limit, the part above 80 °C second: 50 to 64 °C.
        ExtraIcon::Temperature => [0.5 + 0.14 * wave(9.0, 0.0).powi(2), 0.0],
        // rpm, left and right fan.
        ExtraIcon::Fan => [
            2_160.0 + 900.0 * wave(23.0, 4.0).powi(2),
            2_000.0 + 700.0 * wave(23.0, 3.6).powi(2),
        ],
        ExtraIcon::Battery => [BATTERY_LEVEL, 0.0],
        ExtraIcon::Disk => [
            2.0e6 * wave(13.0, 2.0).powi(6) * 30.0,
            1.0e6 * wave(7.0, 0.5).powi(8) * 20.0,
        ],
        ExtraIcon::Network => [
            4.0e6 * wave(11.0, 1.0).powi(4) + 2.0e4,
            5.0e5 * wave(6.0, 3.0).powi(4) + 8.0e3,
        ],
        _ => [
            1.5e6 * wave(17.0, 0.0).powi(4) + 1.0e4,
            1.5e5 * wave(8.0, 2.0).powi(4) + 4.0e3,
        ],
    }
}

/// The rows of extras button `index`'s plate.
fn plate(bar: StockBar, pixels: &mut [u8], index: u8) -> Vec<&mut [u8]> {
    let bounds = bar.extra_bounds(index).unwrap();
    let (left, width) = (bounds.x as usize, bounds.width as usize);
    pixels
        .as_chunks_mut::<{ WIDTH as usize * 4 }>()
        .0
        .iter_mut()
        .map(|row| &mut row[left * 4..(left + width) * 4])
        .collect()
}

/// The pretend battery: charge, and hours until full (on power).
const BATTERY_LEVEL: f64 = 0.68;
const BATTERY_HOURS: f64 = 0.75;

/// The extras row, all ten buttons live.
const EXTRAS: [ExtraIcon; 10] = [
    ExtraIcon::Camera,
    ExtraIcon::Mic,
    ExtraIcon::Vpn,
    ExtraIcon::Cpu,
    ExtraIcon::Temperature,
    ExtraIcon::Fan,
    ExtraIcon::Memory,
    ExtraIcon::Disk,
    ExtraIcon::Network,
    ExtraIcon::Battery,
];

/// The extras row's meters (the VPN and everything right of it), each with a full graph
/// of sample data.
fn demo_meters() -> Vec<(u8, Meter)> {
    (2_u8..)
        .zip(&EXTRAS[2..])
        .map(|(index, icon)| {
            let mut meter = Meter::new(*icon, None, 182).unwrap();
            meter.pretend_hardware(BATTERY_LEVEL, BATTERY_HOURS);
            for step in 0..91 {
                meter.push_sample(sample(*icon, step));
            }
            (index, meter)
        })
        .collect()
}

/// A voice-like mic spectrum at time `t` (in frames).
fn demo_spectrum(spectrum: &mut Spectrum, t: f64) {
    for (bar_index, (level, peak)) in spectrum
        .levels
        .iter_mut()
        .zip(&mut spectrum.peaks)
        .enumerate()
    {
        let b = bar_index as f64;
        let voice = (-(b - 6.0).powi(2) / 60.0).exp();
        let wobble = ((t / 2.5 + b / 2.0).sin() + (t / 4.0 - b / 3.0).sin() + 2.0) / 4.0;
        *level = (voice * wobble * 0.95) as f32;
        *peak = (*peak - 0.01).max(*level);
    }
}

fn render_extras() {
    const ICONS: [ExtraIcon; 10] = EXTRAS;
    let bar = bar().with_extras(&ICONS);
    let mut meters = demo_meters();
    let mut spectrum = Spectrum::new(24);
    let battery_bars = meters
        .iter()
        .find_map(|(_, meter)| meter.battery_bars())
        .unwrap();
    // Camera, mic and VPN on; the battery lit (on power).
    let shown = StockLevels {
        extras_on: 0b10_0000_0111,
        extras_live: 0b11_1111_1111,
        battery_bars,
        ..StockLevels::default()
    };
    let mut out = File::create(output().join("extras.raw")).unwrap();
    for frame in 0_u32..120 {
        let t = f64::from(frame);
        let mut pixels = vec![0; FRAME_BYTES];
        let mut row = Xrgb8888Frame::new(WIDTH, HEIGHT, WIDTH * 4, &mut pixels).unwrap();
        bar.render_pressed(&mut row, Layer::Extras, shown, None)
            .unwrap();
        // Pretend camera picture: colour drifting over a gradient (not a real picture).
        for (y, line) in plate(bar, &mut pixels, 0).into_iter().enumerate() {
            for (x, pixel) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let dx = x as f64 - (91.0 + 60.0 * (t / 18.0).sin());
                let dy = y as f64 - 30.0;
                let glow = (-(dx * dx + 4.0 * dy * dy) / 1_800.0).exp();
                *pixel = [
                    (70.0 + 110.0 * glow) as u8,
                    (45.0 + y as f64 + 90.0 * glow) as u8,
                    (35.0 + x as f64 / 4.0 + 150.0 * glow) as u8,
                    0,
                ];
            }
        }
        demo_spectrum(&mut spectrum, t);
        let lines = plate(bar, &mut pixels, 1);
        draw_mic_spectrum(lines.into_iter(), &spectrum, 182, 60, FaceTint::None);
        if frame % 4 == 0 {
            for (index, meter) in &mut meters {
                meter.push_sample(sample(ICONS[usize::from(*index)], 91 + frame / 4));
            }
        }
        for (index, meter) in &meters {
            meter.draw(plate(bar, &mut pixels, *index).into_iter(), 182, 60);
        }
        let mut row = Xrgb8888Frame::new(WIDTH, HEIGHT, WIDTH * 4, &mut pixels).unwrap();
        bar.render_live_extras(&mut row, shown, None).unwrap();
        out.write_all(&pixels).unwrap();
    }
}

/// Plain rows for the web page's icon sprites (`tools/make_demo_media.py` cuts the icons out):
/// empty sliders, muted, the extras row unlit and with its toggles and battery lit.
fn render_sprite_rows() {
    let mut out = File::create(output().join("sprites.raw")).unwrap();
    // No keyboard level: at 0 its icon would be drawn dimmed (backlight off).
    let empty = StockLevels {
        keyboard_backlight: None,
        ..levels(0, 0, 0, false)
    };
    out.write_all(&draw_row(Layer::Controls, empty)).unwrap();
    let muted = StockLevels {
        muted: true,
        ..empty
    };
    out.write_all(&draw_row(Layer::Controls, muted)).unwrap();
    out.write_all(&draw_row(Layer::Function, empty)).unwrap();
    let bar = bar().with_extras(&EXTRAS);
    for (extras_on, battery_bars) in [(0, 4), (0b10_0000_0111, 4)] {
        let mut pixels = vec![0; FRAME_BYTES];
        let mut row = Xrgb8888Frame::new(WIDTH, HEIGHT, WIDTH * 4, &mut pixels).unwrap();
        let shown = StockLevels {
            extras_on,
            battery_bars,
            ..StockLevels::default()
        };
        bar.render_pressed(&mut row, Layer::Extras, shown, None)
            .unwrap();
        out.write_all(&pixels).unwrap();
    }
}

/// Every built-in theme: its controls row (media playing, so play/pause is lit), then its
/// extras row (mic and VPN on, with the meters and the mic spectrum live, in the theme's
/// tint), two frames per theme in `Theme::NAMES` order.
fn render_themes(levels: StockLevels) {
    let mut out = File::create(output().join("themes.raw")).unwrap();
    let mut meters = demo_meters();
    let battery_bars = meters
        .iter()
        .find_map(|(_, meter)| meter.battery_bars())
        .unwrap();
    let mut spectrum = Spectrum::new(24);
    for step in 0..40 {
        demo_spectrum(&mut spectrum, f64::from(step));
    }
    for name in Theme::NAMES {
        let theme = Theme::named(name).unwrap();
        let controls = bar().with_theme(theme);
        let mut pixels = vec![0; FRAME_BYTES];
        let mut frame = Xrgb8888Frame::new(WIDTH, HEIGHT, WIDTH * 4, &mut pixels).unwrap();
        let playing = StockLevels {
            playing: true,
            ..levels
        };
        controls
            .render_pressed(&mut frame, Layer::Controls, playing, None)
            .unwrap();
        out.write_all(&pixels).unwrap();

        let extras = controls.with_extras(&EXTRAS);
        // Mic, VPN and the battery (on power) lit; every face but the camera's live.
        let shown = StockLevels {
            extras_on: 0b10_0000_0110,
            extras_live: 0b11_1111_1110,
            battery_bars,
            ..levels
        };
        let mut pixels = vec![0; FRAME_BYTES];
        let mut frame = Xrgb8888Frame::new(WIDTH, HEIGHT, WIDTH * 4, &mut pixels).unwrap();
        extras
            .render_pressed(&mut frame, Layer::Extras, shown, None)
            .unwrap();
        let tint = |index: u8| theme.face_tint(extras.extra_bounds(index).unwrap().x, WIDTH);
        draw_mic_spectrum(
            plate(extras, &mut pixels, 1).into_iter(),
            &spectrum,
            182,
            60,
            tint(1),
        );
        for (index, meter) in &mut meters {
            meter.set_tint(tint(*index));
            meter.draw(plate(extras, &mut pixels, *index).into_iter(), 182, 60);
        }
        let mut frame = Xrgb8888Frame::new(WIDTH, HEIGHT, WIDTH * 4, &mut pixels).unwrap();
        extras.render_live_extras(&mut frame, shown, None).unwrap();
        out.write_all(&pixels).unwrap();
    }
}

/// The startup animation over the controls row, at 30 fps, then a second of the bar.
fn render_intro(levels: StockLevels) {
    let row = draw_row(Layer::Controls, levels);
    let intro = Intro::new(WIDTH, HEIGHT);
    let mut out = File::create(output().join("intro.raw")).unwrap();
    let step = Duration::from_millis(33);
    let mut elapsed = Duration::ZERO;
    while elapsed <= intro::DURATION + Duration::from_secs(1) {
        let mut pixels = row.clone();
        intro.draw_at(&mut pixels, elapsed);
        out.write_all(&pixels).unwrap();
        elapsed += step;
    }
}

/// The test screen's animations, in real time so they run at their own pace.
fn render_test_screen() {
    let mut saver = Screensaver::new(WIDTH, HEIGHT, 1);
    let mut out = File::create(output().join("test-screen.raw")).unwrap();
    let mut pixels = vec![0; FRAME_BYTES];
    let mut id = 0;
    for scene in 0..4 {
        if scene > 0 {
            saver.next_scene();
        }
        for _ in 0..38 {
            std::thread::sleep(FRAME_TIME);
            saver.tick();
            saver.render(&mut pixels);
            saver.submitted(id);
            saver.released(id);
            id += 1;
            out.write_all(&pixels).unwrap();
        }
    }
    saver.finish();
}

#[test]
#[ignore = "writes the demo frames; run through tools/make_demo_media.py"]
fn render_demo_frames() {
    fs::create_dir_all(output()).unwrap();
    let levels = levels(35, 50, 30, false);
    fs::write(
        output().join("controls.raw"),
        draw_row(Layer::Controls, levels),
    )
    .unwrap();
    fs::write(
        output().join("function.raw"),
        draw_row(Layer::Function, levels),
    )
    .unwrap();
    render_intro(levels);
    render_themes(levels);
    render_sprite_rows();
    render_sliders();
    render_extras();
    render_test_screen();
}
