//! Antialiased mask composition for the built-in Boot Camp-style surface.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::framebuffer::{FramebufferError, Rectangle, Rgb, Xrgb8888Frame};
use crate::stock_icons;
use crate::stock_labels::AlphaMask;

const SAMPLE_GRID: u32 = 4;
const SAMPLE_COUNT: u32 = SAMPLE_GRID * SAMPLE_GRID;
const NATIVE_PLATE_HEIGHT: u32 = 60;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Symbol {
    DisplayUp,
    KeyboardUp,
    Previous,
    PlayPause,
    Next,
    Mute,
    VolumeUp,
    CameraOn,
    CameraOff,
    MicOn,
    MicOff,
    Globe,
    GlobeShield,
    Chip,
    Memory,
    Disk,
    Arrows,
    /// Charge bars filled (0 to 4), and a bolt over them while on power.
    Battery {
        bars: u8,
        charging: bool,
    },
    Thermometer,
    Fan,
}

/// What a cached mask is of (with its pixel size). Every frame draws the same few plates
/// and symbols, and supersampling them each time cost several ms per frame.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum MaskKey {
    Plate {
        width: u32,
        height: u32,
        radius: u32,
    },
    Corners {
        width: u32,
        height: u32,
        radius: u32,
    },
    Shape {
        symbol: Symbol,
        width: u32,
        height: u32,
    },
    Halo {
        symbol: Symbol,
        width: u32,
        height: u32,
    },
}

thread_local! {
    static MASKS: RefCell<HashMap<MaskKey, Rc<[u8]>>> = RefCell::new(HashMap::new());
}

/// The mask for `key`, made once by `make`.
fn cached(key: MaskKey, make: impl FnOnce() -> Vec<u8>) -> Rc<[u8]> {
    if let Some(mask) = MASKS.with(|masks| masks.borrow().get(&key).cloned()) {
        return mask;
    }
    let mask: Rc<[u8]> = make().into();
    MASKS.with(|masks| masks.borrow_mut().insert(key, Rc::clone(&mask)));
    mask
}

fn plate_mask(width: u32, height: u32, radius: u32) -> Rc<[u8]> {
    cached(
        MaskKey::Plate {
            width,
            height,
            radius,
        },
        || rasterize(width, height, |x, y| in_plate(x, y, width, height, radius)),
    )
}

/// Native (60-high plate) size of the drawn extras symbols.
const CAMERA_SIZE: (f64, f64) = (38.0, 24.0);
const MIC_SIZE: (f64, f64) = (22.0, 30.0);
const GLOBE_SIZE: (f64, f64) = (28.0, 28.0);
const GLOBE_SHIELD_SIZE: (f64, f64) = (40.0, 34.0);
const CHIP_SIZE: (f64, f64) = (30.0, 30.0);
const MEMORY_SIZE: (f64, f64) = (40.0, 22.0);
const DISK_SIZE: (f64, f64) = (26.0, 30.0);
const ARROWS_SIZE: (f64, f64) = (28.0, 28.0);
const BATTERY_SIZE: (f64, f64) = (52.0, 22.0);
const THERMOMETER_SIZE: (f64, f64) = (24.0, 30.0);
const FAN_SIZE: (f64, f64) = (30.0, 30.0);

pub(crate) fn draw_rounded_plate(
    frame: &mut Xrgb8888Frame<'_>,
    bounds: Rectangle,
    radius: u32,
    color: Rgb,
) -> Result<(), FramebufferError> {
    frame.blend_mask(
        bounds,
        &plate_mask(bounds.width, bounds.height, radius),
        color,
    )
}

/// The left `filled` pixels of a rounded plate, keeping its rounded corners.
pub(crate) fn draw_plate_fill(
    frame: &mut Xrgb8888Frame<'_>,
    bounds: Rectangle,
    radius: u32,
    filled: u32,
    color: Rgb,
) -> Result<(), FramebufferError> {
    let filled = filled.min(bounds.width) as usize;
    if filled == 0 {
        return Ok(());
    }
    let mut mask = plate_mask(bounds.width, bounds.height, radius).to_vec();
    for row in mask.chunks_exact_mut(bounds.width as usize) {
        row[filled..].fill(0);
    }
    frame.blend_mask(bounds, &mask, color)
}

/// Paints what lies outside a rounded plate's corners, to round off a picture drawn
/// over the whole plate rectangle.
pub(crate) fn draw_plate_corners(
    frame: &mut Xrgb8888Frame<'_>,
    bounds: Rectangle,
    radius: u32,
    color: Rgb,
) -> Result<(), FramebufferError> {
    let (width, height) = (bounds.width, bounds.height);
    let mask = cached(
        MaskKey::Corners {
            width,
            height,
            radius,
        },
        || rasterize(width, height, |x, y| !in_plate(x, y, width, height, radius)),
    );
    frame.blend_mask(bounds, &mask, color)
}

fn in_plate(x: f64, y: f64, width: u32, height: u32, radius: u32) -> bool {
    let radius = f64::from(radius.min(width / 2).min(height / 2));
    let left = radius;
    let right = f64::from(width) - radius;
    let top = radius;
    let bottom = f64::from(height) - radius;
    if (x >= left && x <= right) || (y >= top && y <= bottom) {
        return true;
    }
    let center_x = if x < left { left } else { right };
    let center_y = if y < top { top } else { bottom };
    let dx = x - center_x;
    let dy = y - center_y;
    dx * dx + dy * dy <= radius * radius
}

pub(crate) fn draw_label(
    frame: &mut Xrgb8888Frame<'_>,
    bounds: Rectangle,
    label: AlphaMask,
    color: Rgb,
) -> Result<(), FramebufferError> {
    draw_label_with_opacity(frame, bounds, label, color, u8::MAX)
}

pub(crate) fn draw_label_with_opacity(
    frame: &mut Xrgb8888Frame<'_>,
    bounds: Rectangle,
    label: AlphaMask,
    color: Rgb,
    opacity: u8,
) -> Result<(), FramebufferError> {
    let width = scaled_dimension(label.width, bounds.height, bounds.width);
    let height = scaled_dimension(label.height, bounds.height, bounds.height);
    let mut pixels = resize_mask(label, width, height);
    if opacity != u8::MAX {
        for alpha in &mut pixels {
            *alpha = u8::try_from((u16::from(*alpha) * u16::from(opacity) + 127) / 255)
                .unwrap_or(u8::MAX);
        }
    }
    frame.blend_mask(
        Rectangle {
            x: bounds.x + (bounds.width - width) / 2,
            y: bounds.y + (bounds.height - height) / 2,
            width,
            height,
        },
        &pixels,
        color,
    )
}

pub(crate) fn draw_symbol(
    frame: &mut Xrgb8888Frame<'_>,
    bounds: Rectangle,
    symbol: Symbol,
    color: Rgb,
) -> Result<(), FramebufferError> {
    draw_symbol_with_halo(frame, bounds, symbol, color, None)
}

/// [`draw_symbol`] with an optional halo: a 2 px outline in another colour under the
/// drawn extras symbols, so they read over a picture. Bitmap symbols get no halo.
pub(crate) fn draw_symbol_with_halo(
    frame: &mut Xrgb8888Frame<'_>,
    bounds: Rectangle,
    symbol: Symbol,
    color: Rgb,
    halo: Option<Rgb>,
) -> Result<(), FramebufferError> {
    let color = (color, halo);
    let mask = match symbol {
        Symbol::CameraOn => return draw_shape(frame, bounds, symbol, CAMERA_SIZE, color, camera),
        Symbol::CameraOff => {
            let struck = struck_through(camera, (3.0, 24.0), (35.0, 0.0));
            return draw_shape(frame, bounds, symbol, CAMERA_SIZE, color, struck);
        }
        Symbol::MicOn => return draw_shape(frame, bounds, symbol, MIC_SIZE, color, microphone),
        Symbol::MicOff => {
            let struck = struck_through(microphone, (1.0, 29.0), (21.0, 1.0));
            return draw_shape(frame, bounds, symbol, MIC_SIZE, color, struck);
        }
        Symbol::Globe => return draw_shape(frame, bounds, symbol, GLOBE_SIZE, color, globe),
        Symbol::GlobeShield => {
            return draw_shape(
                frame,
                bounds,
                symbol,
                GLOBE_SHIELD_SIZE,
                color,
                globe_shield,
            );
        }
        Symbol::Chip => return draw_shape(frame, bounds, symbol, CHIP_SIZE, color, chip),
        Symbol::Memory => return draw_shape(frame, bounds, symbol, MEMORY_SIZE, color, memory),
        Symbol::Disk => return draw_shape(frame, bounds, symbol, DISK_SIZE, color, disk),
        Symbol::Arrows => return draw_shape(frame, bounds, symbol, ARROWS_SIZE, color, arrows),
        Symbol::Battery { bars, charging } => {
            let shape = move |x, y| battery(x, y, bars, charging);
            return draw_shape(frame, bounds, symbol, BATTERY_SIZE, color, shape);
        }
        Symbol::Fan => return draw_shape(frame, bounds, symbol, FAN_SIZE, color, fan),
        Symbol::Thermometer => {
            return draw_shape(frame, bounds, symbol, THERMOMETER_SIZE, color, thermometer);
        }
        Symbol::DisplayUp => stock_icons::DISPLAY_UP,
        Symbol::KeyboardUp => stock_icons::KEYBOARD_UP,
        Symbol::Previous => stock_icons::PREVIOUS,
        Symbol::PlayPause => stock_icons::PLAY_PAUSE,
        Symbol::Next => stock_icons::NEXT,
        Symbol::Mute => stock_icons::MUTE,
        Symbol::VolumeUp => stock_icons::VOLUME_UP,
    };
    draw_label(frame, bounds, mask, color.0)
}

/// Rasterizes a shape given in native units (`size`) centred in `bounds`.
/// `color` is the shape's colour and an optional halo colour.
fn draw_shape(
    frame: &mut Xrgb8888Frame<'_>,
    bounds: Rectangle,
    symbol: Symbol,
    size: (f64, f64),
    (color, halo): (Rgb, Option<Rgb>),
    contains: impl Fn(f64, f64) -> bool,
) -> Result<(), FramebufferError> {
    const HALO: u32 = 2;
    let scale = f64::from(bounds.height) / f64::from(NATIVE_PLATE_HEIGHT);
    let width = pixel_extent(size.0 * scale, bounds.width);
    let height = pixel_extent(size.1 * scale, bounds.height);
    let mask = cached(
        MaskKey::Shape {
            symbol,
            width,
            height,
        },
        || rasterize(width, height, |x, y| contains(x / scale, y / scale)),
    );
    let place = Rectangle {
        x: bounds.x + (bounds.width - width) / 2,
        y: bounds.y + (bounds.height - height) / 2,
        width,
        height,
    };
    if let Some(halo) = halo
        && place.x >= HALO
        && place.y >= HALO
        && place.x + width + HALO <= frame.width()
        && place.y + height + HALO <= frame.height()
    {
        let outer = Rectangle {
            x: place.x - HALO,
            y: place.y - HALO,
            width: width + 2 * HALO,
            height: height + 2 * HALO,
        };
        let grown = cached(
            MaskKey::Halo {
                symbol,
                width,
                height,
            },
            || dilate(&mask, width, height, HALO),
        );
        frame.blend_mask(outer, &grown, halo)?;
    }
    frame.blend_mask(place, &mask, color)
}

/// Grows a `width`x`height` coverage mask by `radius` pixels on every side (a disc).
fn dilate(mask: &[u8], width: u32, height: u32, radius: u32) -> Vec<u8> {
    let (width, height, radius) = (width as usize, height as usize, radius as usize);
    let outer = width + 2 * radius;
    let mut grown = vec![0_u8; outer * (height + 2 * radius)];
    for (y, row) in mask.chunks_exact(width).enumerate() {
        for (x, &coverage) in row.iter().enumerate() {
            if coverage == 0 {
                continue;
            }
            for dy in 0..=2 * radius {
                for dx in 0..=2 * radius {
                    let (ox, oy) = (dx.abs_diff(radius), dy.abs_diff(radius));
                    if ox * ox + oy * oy > radius * radius {
                        continue;
                    }
                    let cell = &mut grown[(y + dy) * outer + x + dx];
                    *cell = (*cell).max(coverage);
                }
            }
        }
    }
    grown
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn pixel_extent(value: f64, maximum: u32) -> u32 {
    (value.ceil() as u32).clamp(1, maximum)
}

/// Video camera: rounded body plus lens wedge; `slashed` adds a struck-through line.
fn camera(x: f64, y: f64) -> bool {
    let body = in_rounded_rect(x, y, (0.0, 2.0, 26.0, 22.0), 4.0);
    let lens = (28.0..=38.0).contains(&x) && {
        let spread = 6.0 * (x - 28.0) / 10.0;
        y >= 9.0 - spread && y <= 15.0 + spread
    };
    body || lens
}

/// Microphone (22x30): capsule, U-shaped holder, stem and base.
fn microphone(x: f64, y: f64) -> bool {
    let capsule = in_rounded_rect(x, y, (6.0, 0.0, 16.0, 18.0), 5.0);
    let (dx, dy) = (x - 11.0, y - 12.0);
    let ring = dx * dx + dy * dy;
    let holder = y >= 12.0 && (6.8 * 6.8..=9.2 * 9.2).contains(&ring);
    let stem = (9.9..=12.1).contains(&x) && (20.0..=27.0).contains(&y);
    let base = in_rounded_rect(x, y, (5.0, 26.5, 17.0, 29.5), 1.5);
    capsule || holder || stem || base
}

/// Adds a diagonal strike from `start` to `end`, with a gap cut on either side of it.
fn struck_through(
    shape: impl Fn(f64, f64) -> bool,
    start: (f64, f64),
    end: (f64, f64),
) -> impl Fn(f64, f64) -> bool {
    move |x, y| {
        let distance = segment_distance(x, y, start, end);
        distance <= 1.6 || (shape(x, y) && distance > 3.6)
    }
}

/// Solid globe (28x28) with the continents cut out.
fn globe(x: f64, y: f64) -> bool {
    globe_at(x, y, (14.0, 14.0), 13.5)
}

/// Globe with a shield and padlock over its lower right (VPN up).
fn globe_shield(x: f64, y: f64) -> bool {
    let shield = |x: f64, y: f64, grow: f64| {
        in_shield(x, y, (30.0, 10.0 - grow), 9.5 + grow, 23.0 + 2.0 * grow)
    };
    if shield(x, y, 0.0) {
        // Padlock cut out of the shield, mapped from padlock's 24x32 units (9 wide).
        let scale = 24.0 / 9.0;
        return !padlock((x - 25.5) * scale, (y - 13.5) * scale);
    }
    // Keep a gap between the globe and the shield.
    !shield(x, y, 2.2) && globe_at(x, y, (14.0, 15.0), 13.5)
}

/// Processor chip (30x30): a framed die with three pins per side.
fn chip(x: f64, y: f64) -> bool {
    let frame = in_rounded_rect(x, y, (5.0, 5.0, 25.0, 25.0), 3.0)
        && !in_rounded_rect(x, y, (8.5, 8.5, 21.5, 21.5), 1.0);
    let die = in_rounded_rect(x, y, (11.0, 11.0, 19.0, 19.0), 1.0);
    let on_pin = |along: f64| {
        [9.5, 15.0, 20.5]
            .iter()
            .any(|pin| (along - pin).abs() <= 1.3)
    };
    let pins = (on_pin(x) && (y <= 5.0 || y >= 25.0)) || (on_pin(y) && (x <= 5.0 || x >= 25.0));
    frame || die || pins
}

/// Memory module (40x22): a board with four chips cut out and contacts below.
fn memory(x: f64, y: f64) -> bool {
    if in_rounded_rect(x, y, (0.0, 0.0, 40.0, 15.0), 2.0) {
        let chip = (0..4).any(|index| {
            let left = 3.5 + f64::from(index) * 8.8;
            (left..=left + 6.0).contains(&x) && (3.5..=11.5).contains(&y)
        });
        return !chip;
    }
    let contact = (x - 2.0).rem_euclid(3.0) <= 1.8 && !(18.5..=21.5).contains(&x);
    (17.0..=22.0).contains(&y) && (2.0..=38.0).contains(&x) && contact
}

/// Disk stack (26x30): a cylinder with three rims, like stacked platters.
fn disk(x: f64, y: f64) -> bool {
    let (half, rim) = (13.0, 4.0);
    let dx = (x - half) / half;
    if dx.abs() > 1.0 {
        return false;
    }
    let curve = rim * (1.0 - dx * dx).sqrt();
    if y < rim - curve || y > 26.0 + curve {
        return false;
    }
    ![rim, 11.5, 19.0]
        .iter()
        .any(|centre| (y - (centre + curve)).abs() <= 0.9)
}

/// Down and up arrows side by side (28x28): received and sent.
fn arrows(x: f64, y: f64) -> bool {
    let arrow = |x: f64, y: f64| {
        let head = y <= 9.0 && (x - 6.0).abs() <= 6.0 * y / 9.0;
        let shaft = y >= 8.0 && (x - 6.0).abs() <= 1.7;
        head || shaft
    };
    arrow(x - 1.0, 28.0 - y) || arrow(x - 15.0, y)
}

/// Battery (52x22): a rounded outline with a terminal nub, `bars` of four vertical
/// charge bars filled from the left, and while `charging` a lightning bolt right of the nub
/// (the space stays empty otherwise, so the battery doesn't move).
fn battery(x: f64, y: f64, bars: u8, charging: bool) -> bool {
    let outline = (in_rounded_rect(x, y, (0.0, 2.0, 35.0, 20.0), 4.0)
        && !in_rounded_rect(x, y, (2.6, 4.6, 32.4, 17.4), 1.6))
        || in_rounded_rect(x, y, (36.5, 7.5, 40.0, 14.5), 1.5);
    // Four bars across the inside (4.2 to 30.8), 1.4 apart.
    let (left, pitch, width) = (4.2, 6.75, 5.35);
    let bar = (0..bars.min(4)).any(|index| {
        let start = left + f64::from(index) * pitch;
        in_rounded_rect(x, y, (start, 6.2, start + width, 15.8), 0.8)
    });
    outline || bar || (charging && in_polygon(x, y, &BOLT))
}

/// Lightning bolt right of the battery, in the battery's units.
const BOLT: [(f64, f64); 6] = [
    (50.0, 0.5),
    (42.0, 12.6),
    (46.6, 12.6),
    (44.4, 21.5),
    (52.0, 9.4),
    (47.4, 9.4),
];

/// Fan (30x30): a hub and three swept blades.
fn fan(x: f64, y: f64) -> bool {
    use std::f64::consts::TAU;
    let (dx, dy) = (x - 15.0, y - 15.0);
    let radius = dx.hypot(dy);
    if radius <= 3.4 {
        return true;
    }
    if !(4.8..=14.5).contains(&radius) {
        return false;
    }
    let along = (radius - 4.8) / (14.5 - 4.8);
    // Each blade sweeps round as it goes out, and widens towards the tip.
    let angle = dy.atan2(dx) - 1.1 * along;
    let half_width = 0.30 + 0.45 * (along * std::f64::consts::PI).sin();
    (0..3).any(|blade| {
        let centre = f64::from(blade) * TAU / 3.0;
        let offset = (angle - centre).rem_euclid(TAU);
        offset.min(TAU - offset) <= half_width
    })
}

/// Thermometer (24x30): a hollow tube, half filled, over a solid bulb, with ticks.
fn thermometer(x: f64, y: f64) -> bool {
    let (dx, dy) = (x - 9.0, y - 23.5);
    let bulb = dx * dx + dy * dy <= 6.3 * 6.3;
    let tube = in_rounded_rect(x, y, (5.0, 0.0, 13.0, 22.0), 4.0)
        && !in_rounded_rect(x, y, (7.4, 2.4, 10.6, 22.0), 1.6);
    let column = (7.9..=10.1).contains(&x) && (9.0..=22.0).contains(&y);
    let ticks =
        (16.0..=22.0).contains(&x) && [4.0, 9.0, 14.0].iter().any(|tick| (y - tick).abs() <= 0.8);
    bulb || tube || column || ticks
}

/// Stylised Americas (plus Greenland and the edge of Africa) in unit-circle coordinates,
/// x right and y down.
const CONTINENTS: [&[(f64, f64)]; 4] = [
    &[
        (-0.80, -0.30),
        (-0.75, -0.55),
        (-0.45, -0.80),
        (-0.05, -0.85),
        (0.15, -0.65),
        (0.05, -0.45),
        (-0.10, -0.32),
        (-0.04, -0.14),
        (0.06, 0.04),
        (-0.06, 0.08),
        (-0.24, -0.08),
        (-0.32, -0.20),
        (-0.55, -0.25),
    ],
    &[
        (-0.02, 0.14),
        (0.30, 0.14),
        (0.48, 0.30),
        (0.38, 0.55),
        (0.16, 0.86),
        (0.06, 0.90),
        (0.06, 0.60),
        (-0.10, 0.32),
    ],
    &[(0.28, -0.92), (0.52, -0.86), (0.44, -0.68), (0.26, -0.72)],
    &[
        (0.66, -0.40),
        (1.00, -0.40),
        (1.00, 0.20),
        (0.86, 0.48),
        (0.74, 0.18),
        (0.62, -0.08),
    ],
];

fn globe_at(x: f64, y: f64, center: (f64, f64), radius: f64) -> bool {
    let (u, v) = ((x - center.0) / radius, (y - center.1) / radius);
    u * u + v * v <= 1.0 && !CONTINENTS.iter().any(|polygon| in_polygon(u, v, polygon))
}

/// Even-odd point-in-polygon test.
fn in_polygon(x: f64, y: f64, points: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let mut previous = points[points.len() - 1];
    for &point in points {
        if (point.1 > y) != (previous.1 > y)
            && x < (previous.0 - point.0) * (y - point.1) / (previous.1 - point.1) + point.0
        {
            inside = !inside;
        }
        previous = point;
    }
    inside
}

/// Shield: flat top, straight sides for 35% of its height, then curving in to a point.
/// `top` is the centre of its top edge.
fn in_shield(x: f64, y: f64, top: (f64, f64), half_width: f64, height: f64) -> bool {
    let t = (y - top.1) / height;
    if !(0.0..=1.0).contains(&t) {
        return false;
    }
    let taper = ((t - 0.35) / 0.65).max(0.0);
    (x - top.0).abs() <= half_width * (1.0 - taper).powf(0.7)
}

/// Closed padlock in 24x32 units: body plus a seated shackle.
fn padlock(x: f64, y: f64) -> bool {
    if in_rounded_rect(x, y, (0.0, 15.0, 24.0, 32.0), 3.0) {
        return true;
    }
    let (center_x, center_y) = (12.0, 12.0);
    let (outer, inner) = (9.0, 5.5);
    let dx = x - center_x;
    let dy = y - center_y;
    let ring = dx * dx + dy * dy;
    if y <= center_y {
        return ring <= outer * outer && ring >= inner * inner;
    }
    let leg = (center_x + inner..=center_x + outer).contains(&x)
        || (center_x - outer..=center_x - inner).contains(&x);
    leg && y <= 15.0
}

fn in_rounded_rect(
    x: f64,
    y: f64,
    (left, top, right, bottom): (f64, f64, f64, f64),
    radius: f64,
) -> bool {
    if x < left || x > right || y < top || y > bottom {
        return false;
    }
    let nearest_x = x.clamp(left + radius, right - radius);
    let nearest_y = y.clamp(top + radius, bottom - radius);
    let dx = x - nearest_x;
    let dy = y - nearest_y;
    dx * dx + dy * dy <= radius * radius
}

fn segment_distance(x: f64, y: f64, start: (f64, f64), end: (f64, f64)) -> f64 {
    let (ex, ey) = (end.0 - start.0, end.1 - start.1);
    let t = (((x - start.0) * ex + (y - start.1) * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
    let dx = x - (start.0 + t * ex);
    let dy = y - (start.1 + t * ey);
    (dx * dx + dy * dy).sqrt()
}

fn scaled_dimension(native: u32, plate_height: u32, maximum: u32) -> u32 {
    u32::try_from(
        (u64::from(native) * u64::from(plate_height)).div_ceil(u64::from(NATIVE_PLATE_HEIGHT)),
    )
    .unwrap_or(maximum)
    .clamp(1, maximum)
}

fn resize_mask(label: AlphaMask, width: u32, height: u32) -> Vec<u8> {
    let mut output = vec![0_u8; usize::try_from(u64::from(width) * u64::from(height)).unwrap_or(0)];
    for y in 0..height {
        let source_y = (u64::from(y) * u64::from(label.height) / u64::from(height))
            .min(u64::from(label.height - 1));
        for x in 0..width {
            let source_x = (u64::from(x) * u64::from(label.width) / u64::from(width))
                .min(u64::from(label.width - 1));
            let source = usize::try_from(source_y * u64::from(label.width) + source_x).unwrap_or(0);
            let destination =
                usize::try_from(u64::from(y) * u64::from(width) + u64::from(x)).unwrap_or(0);
            output[destination] = label.pixels[source];
        }
    }
    output
}

fn rasterize(width: u32, height: u32, contains: impl Fn(f64, f64) -> bool) -> Vec<u8> {
    let mut mask = vec![0_u8; usize::try_from(u64::from(width) * u64::from(height)).unwrap_or(0)];
    for y in 0..height {
        for x in 0..width {
            let mut covered = 0_u32;
            for sample_y in 0..SAMPLE_GRID {
                for sample_x in 0..SAMPLE_GRID {
                    let sample_x =
                        f64::from(x) + (f64::from(sample_x) + 0.5) / f64::from(SAMPLE_GRID);
                    let sample_y =
                        f64::from(y) + (f64::from(sample_y) + 0.5) / f64::from(SAMPLE_GRID);
                    covered += u32::from(contains(sample_x, sample_y));
                }
            }
            let index =
                usize::try_from(u64::from(y) * u64::from(width) + u64::from(x)).unwrap_or(0);
            mask[index] =
                u8::try_from((covered * 255 + SAMPLE_COUNT / 2) / SAMPLE_COUNT).unwrap_or(255);
        }
    }
    mask
}
