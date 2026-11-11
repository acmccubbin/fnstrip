//! System meters for the extras row: CPU, memory, disk and network (and the VPN's
//! traffic), sampled from `/proc` and drawn as a scrolling two-tone graph under the icon,
//! the battery's charge (from `/sys/class/power_supply`) and the CPU package temperature
//! and fan speeds (both from `/sys/class/hwmon`).
//!
//! Each sample has two parts, stacked: CPU user + system time, memory in use + page cache,
//! disk read + write, network received + sent. The battery has one: its charge, so its
//! plate fills like a gauge, coloured by power state. The temperature has its part up to
//! [`HOT_BELOW_CRITICAL`] under the critical limit, and the part above it (red). CPU and memory are fractions of the whole;
//! rates are scaled to the peak of the shown history (with a floor so idle noise stays
//! low), and the scale eases back down when a peak scrolls off.

use crate::theme::FaceTint;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::stock_labels::AlphaMask;
use crate::stock_slice::{ExtraIcon, LIVE_ICON_WIDTH};
use crate::value_glyphs;

/// Width in pixels of one sample's column; the newest sample is at the right.
const COLUMN_WIDTH: usize = 2;
/// Lowest full scale for rates, in bytes per second.
const NETWORK_FLOOR: f64 = 32.0 * 1024.0;
const DISK_FLOOR: f64 = 256.0 * 1024.0;
/// Per-sample factor the scale may shrink by when the peak scrolls off.
const SCALE_EASE: f64 = 0.96;
const SECTOR_BYTES: u64 = 512;
const DEFAULT_VPN: &str = "wg0";
/// The printed value is the mean of this many latest samples (1 s), so it stays readable.
const VALUE_SAMPLES: usize = 4;
/// Gap between the value's right end and the plate's.
const VALUE_RIGHT_MARGIN: usize = 8;
/// Gap between the two lines of a two-part value.
const LINE_GAP: usize = 6;
/// Outline under the value, so it reads over the graph.
const HALO_RADIUS: usize = 2;
const HALO: [u8; 4] = [0, 0, 0, 0];
const WHITE: [u8; 4] = [242, 242, 242, 0];
const POWER_SUPPLIES: &str = "/sys/class/power_supply";
/// Charge at or below which the battery shows amber, and red.
const BATTERY_LOW: f64 = 0.2;
const BATTERY_CRITICAL: f64 = 0.1;
/// Time-to-empty or -full estimates beyond this many hours aren't printed (idle noise).
const BATTERY_MAX_HOURS: f64 = 48.0;
/// The temperature graph is a fraction of the sensor's critical limit (or this, if it
/// has none, in °C), drawn red from this many degrees below it.
const DEFAULT_CRITICAL: f64 = 100.0;
const HOT_BELOW_CRITICAL: f64 = 20.0;
/// CPU package sensors: hwmon driver and its label (Intel, AMD).
const PACKAGE_SENSORS: [(&str, &str); 2] = [("coretemp", "Package id 0"), ("k10temp", "Tctl")];
const THERMAL_ZONE_TYPE: &str = "x86_pkg_temp";
/// hwmon drivers with fan speeds (Apple's SMC).
const FAN_DRIVERS: [&str; 1] = ["applesmc"];
/// The fan graph's bottom, as a fraction of the lowest fan minimum.
const FAN_FLOOR: f64 = 0.9;
/// Fan line thickness in pixels.
const FAN_LINE: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Cpu,
    Memory,
    Disk,
    Network,
    Battery,
    Temperature,
    Fan,
}

/// Graph colours as XRGB8888 bytes (B, G, R, X): background, lower part, upper part.
struct Palette([[u8; 4]; 3]);

const fn bgrx(red: u8, green: u8, blue: u8) -> [u8; 4] {
    [blue, green, red, 0]
}

impl Kind {
    const fn palette(self, vpn: bool) -> Palette {
        Palette(match (self, vpn) {
            (Self::Cpu, _) => [bgrx(16, 30, 54), bgrx(70, 150, 235), bgrx(235, 110, 90)],
            (Self::Memory, _) => [bgrx(38, 22, 52), bgrx(180, 110, 240), bgrx(100, 70, 130)],
            (Self::Disk, _) => [bgrx(50, 36, 12), bgrx(240, 190, 70), bgrx(240, 110, 60)],
            (Self::Network, false) => [bgrx(12, 40, 46), bgrx(70, 215, 220), bgrx(235, 140, 200)],
            // The lit VPN plate's green.
            (Self::Network, true) => [bgrx(16, 56, 30), bgrx(100, 220, 130), bgrx(225, 225, 120)],
            // Right fan (lower), left fan (upper).
            (Self::Fan, _) => [bgrx(14, 34, 50), bgrx(80, 180, 240), bgrx(160, 125, 245)],
            (Self::Temperature, _) => [bgrx(48, 26, 14), bgrx(240, 150, 60), bgrx(240, 60, 50)],
            // Discharging; see `Battery::palette` for the other states.
            (Self::Battery, _) => [bgrx(30, 33, 38), bgrx(125, 133, 146), bgrx(125, 133, 146)],
        })
    }
}

/// The battery's last reading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Battery {
    /// Charge, 0 to 1.
    level: f64,
    /// On external power (charging, full, or held at a charge limit).
    powered: bool,
    /// Estimated hours to empty (discharging) or to full (charging), if known.
    hours: Option<f64>,
}

impl Battery {
    /// Green on power, then grey, amber when low and red when critical.
    const fn palette(self) -> Palette {
        let (background, fill) = if self.powered {
            (bgrx(16, 56, 30), bgrx(100, 220, 130))
        } else if self.level <= BATTERY_CRITICAL {
            (bgrx(60, 16, 14), bgrx(235, 75, 60))
        } else if self.level <= BATTERY_LOW {
            (bgrx(56, 40, 10), bgrx(240, 175, 50))
        } else {
            return Kind::Battery.palette(false);
        };
        Palette([background, fill, fill])
    }
}

/// Cumulative counters from the previous sample, to turn into rates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Counters {
    first: u64,
    second: u64,
    /// CPU: total jiffies. Rates: unused (time comes from the clock).
    total: u64,
}

pub(crate) struct Meter {
    kind: Kind,
    /// Network: only this interface; `None`: every interface of a device that is up.
    interface: Option<String>,
    vpn: bool,
    history: VecDeque<[f64; 2]>,
    capacity: usize,
    previous: Option<(Instant, Counters)>,
    scale: f64,
    battery: Option<Battery>,
    /// Temperature: the sensor file (millidegrees) and its critical limit in °C, found at
    /// the first sample.
    sensor: Option<(PathBuf, f64)>,
    /// Fan: up to two fans (first, second), found at the first sample.
    fans: Vec<Fan>,
    /// Temperature: print °F instead of °C (`temperature_unit = "f"`).
    fahrenheit: bool,
    /// The theme's recolouring of its face.
    tint: FaceTint,
}

/// One fan's speed file and its range in rpm.
#[derive(Clone, Debug, PartialEq)]
struct Fan {
    input: PathBuf,
    min: f64,
    max: f64,
}

/// The fan graph's shared rpm range: from a little under the lowest minimum (so idle
/// lines sit just off the bottom) to the highest maximum.
fn fan_range(fans: &[Fan]) -> (f64, f64) {
    let low = fans.iter().map(|fan| fan.min).fold(f64::INFINITY, f64::min);
    let high = fans.iter().map(|fan| fan.max).fold(0.0, f64::max);
    if fans.is_empty() || high <= low {
        return (0.0, 6000.0);
    }
    (low * FAN_FLOOR, high)
}

impl Meter {
    /// A meter for this button, or `None` if the icon isn't one (the VPN graphs its
    /// interface). `width` is the plate width in pixels.
    pub(crate) fn new(icon: ExtraIcon, interface: Option<&str>, width: usize) -> Option<Self> {
        let (kind, interface, vpn) = match icon {
            ExtraIcon::Cpu => (Kind::Cpu, None, false),
            ExtraIcon::Memory => (Kind::Memory, None, false),
            ExtraIcon::Disk => (Kind::Disk, None, false),
            ExtraIcon::Network => (Kind::Network, interface, false),
            ExtraIcon::Vpn => (Kind::Network, Some(interface.unwrap_or(DEFAULT_VPN)), true),
            ExtraIcon::Battery => (Kind::Battery, None, false),
            ExtraIcon::Temperature => (Kind::Temperature, None, false),
            ExtraIcon::Fan => (Kind::Fan, None, false),
            ExtraIcon::Camera | ExtraIcon::Mic => return None,
        };
        let capacity = width.div_ceil(COLUMN_WIDTH);
        Some(Self {
            kind,
            interface: interface.map(str::to_owned),
            vpn,
            history: VecDeque::with_capacity(capacity),
            capacity,
            previous: None,
            scale: 1.0,
            battery: None,
            sensor: None,
            fans: Vec::new(),
            fahrenheit: false,
            tint: FaceTint::None,
        })
    }

    /// Temperature meters print °F instead of °C.
    #[must_use]
    pub(crate) const fn in_fahrenheit(mut self, fahrenheit: bool) -> Self {
        self.fahrenheit = fahrenheit;
        self
    }

    /// Reads the counters and appends a sample (rates need two reads, so the first read
    /// of a rate meter only primes it).
    #[allow(clippy::cast_precision_loss)]
    pub(crate) fn sample(&mut self) {
        let now = Instant::now();
        let sample = match self.kind {
            Kind::Memory => read_memory(),
            Kind::Fan => {
                if self.fans.is_empty() {
                    self.fans = find_fans();
                }
                // rpm of the first (left) and second (right) fan; 0 for a missing second.
                let rpm = |index: usize| read_number(&self.fans.get(index)?.input);
                rpm(0).map(|left| [left, rpm(1).unwrap_or(0.0)])
            }
            Kind::Temperature => {
                if self.sensor.is_none() {
                    self.sensor = find_package_sensor();
                }
                self.sensor.as_ref().and_then(|(input, critical)| {
                    let celsius = read_millidegrees(input)?;
                    let hot = critical - HOT_BELOW_CRITICAL;
                    Some([
                        celsius.min(hot).max(0.0) / critical,
                        (celsius - hot).max(0.0) / critical,
                    ])
                })
            }
            Kind::Battery => {
                self.battery = read_battery();
                self.battery.map(|battery| [battery.level, 0.0])
            }
            Kind::Cpu => read_cpu().and_then(|counters| {
                let previous = self.previous.replace((now, counters))?.1;
                let total = counters.total.saturating_sub(previous.total).max(1) as f64;
                Some([
                    counters.first.saturating_sub(previous.first) as f64 / total,
                    counters.second.saturating_sub(previous.second) as f64 / total,
                ])
            }),
            Kind::Disk | Kind::Network => {
                let counters = if self.kind == Kind::Disk {
                    read_disk()
                } else {
                    read_network(self.interface.as_deref())
                };
                counters.and_then(|counters| {
                    let (then, previous) = self.previous.replace((now, counters))?;
                    let seconds = now.duration_since(then).as_secs_f64().max(1e-3);
                    Some([
                        counters.first.saturating_sub(previous.first) as f64 / seconds,
                        counters.second.saturating_sub(previous.second) as f64 / seconds,
                    ])
                })
            }
        };
        let Some(sample) = sample else {
            return;
        };
        if self.history.len() == self.capacity {
            self.history.pop_front();
        }
        self.history.push_back(sample);
        self.rescale();
    }

    /// Adds a made-up sample, for the documentation's pictures.
    #[cfg(test)]
    pub(crate) fn push_sample(&mut self, sample: [f64; 2]) {
        if self.history.len() == self.capacity {
            self.history.pop_front();
        }
        self.history.push_back(sample);
        self.rescale();
    }

    /// Made-up hardware, for the documentation's pictures: a battery at `level` on power
    /// with `hours` to full, a sensor with the default critical limit, and two fans with
    /// the ranges of the 2016 laptop it was tested on.
    #[cfg(test)]
    pub(crate) fn pretend_hardware(&mut self, level: f64, hours: f64) {
        self.battery = Some(Battery {
            level,
            powered: true,
            hours: Some(hours),
        });
        self.sensor = Some((PathBuf::new(), DEFAULT_CRITICAL));
        let fan = |min, max| Fan {
            input: PathBuf::new(),
            min,
            max,
        };
        self.fans = vec![fan(2160.0, 5927.0), fan(2000.0, 5489.0)];
    }

    fn rescale(&mut self) {
        let floor = match self.kind {
            Kind::Cpu | Kind::Memory | Kind::Battery | Kind::Temperature | Kind::Fan => return,
            Kind::Disk => DISK_FLOOR,
            Kind::Network => NETWORK_FLOOR,
        };
        let peak = self
            .history
            .iter()
            .map(|[first, second]| first + second)
            .fold(floor, f64::max)
            * 1.1;
        self.scale = peak.max(self.scale * SCALE_EASE);
    }

    /// Draws the graph into plate rows (XRGB8888, `width` pixels each).
    // Pixel maths is f64; the casts are of small in-range values.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub(crate) fn draw<'a>(
        &self,
        rows: impl Iterator<Item = &'a mut [u8]>,
        width: usize,
        height: usize,
    ) {
        let mut rows: Vec<&mut [u8]> = rows.collect();
        if self.kind == Kind::Fan {
            self.draw_fan_lines(&mut rows, width, height);
        } else {
            self.draw_graph(&mut rows, width, height);
        }
        self.draw_value(&mut rows, width, height);
    }

    // Pixel maths is f64; the casts are of small in-range values.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn draw_graph(&self, rows: &mut [&mut [u8]], width: usize, height: usize) {
        let Palette([background, lower, upper]) = self.palette();
        let full = height as f64;
        // Pixel heights of each column's two parts, newest at the right.
        let mut columns = vec![(0_usize, 0_usize); width];
        for (column, [first, second]) in self.history.iter().rev().enumerate() {
            let first = (first / self.scale).clamp(0.0, 1.0);
            let second = (second / self.scale).clamp(0.0, 1.0 - first);
            let low = (first * full).round() as usize;
            let high = ((first + second) * full).round() as usize;
            let right = width.saturating_sub(column * COLUMN_WIDTH);
            for slot in &mut columns[right.saturating_sub(COLUMN_WIDTH)..right] {
                *slot = (low, high);
            }
        }
        for (y, row) in rows.iter_mut().enumerate() {
            let from_bottom = height - y;
            for (pixel, &(low, high)) in row.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
                *pixel = if from_bottom <= low {
                    lower
                } else if from_bottom <= high {
                    upper
                } else {
                    background
                };
            }
        }
    }

    /// Battery: the charge in quarters (0 to 4), rounded, for the icon's bars.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub(crate) fn battery_bars(&self) -> Option<u8> {
        self.battery
            .map(|battery| (battery.level * 4.0).round().clamp(0.0, 4.0) as u8)
    }

    /// Fans: a line per fan on the shared rpm range, the second (right) under the first.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn draw_fan_lines(&self, rows: &mut [&mut [u8]], width: usize, height: usize) {
        let Palette([background, right_colour, left_colour]) = self.palette();
        for row in rows.iter_mut() {
            for pixel in row.as_chunks_mut::<4>().0 {
                *pixel = background;
            }
        }
        let (low, high) = fan_range(&self.fans);
        // Row of the line's top for a speed: 0 is the top, the bottom keeps a line's height.
        let row_of = |rpm: f64| {
            let fraction = ((rpm - low) / (high - low)).clamp(0.0, 1.0);
            ((1.0 - fraction) * (height - FAN_LINE) as f64).round() as usize
        };
        for (part, colour) in [(1, right_colour), (0, left_colour)] {
            if part >= self.fans.len() {
                continue;
            }
            let mut previous: Option<usize> = None;
            // Oldest first, so each column joins the one to its left.
            let skipped = self.capacity.saturating_sub(self.history.len());
            for (column, sample) in self.history.iter().enumerate() {
                let y = row_of(sample[part]);
                let (top, bottom) = previous.map_or((y, y), |last| (last.min(y), last.max(y)));
                previous = Some(y);
                let left = (skipped + column) * COLUMN_WIDTH;
                let left = left.saturating_sub(self.capacity * COLUMN_WIDTH - width);
                for row in &mut rows[top..(bottom + FAN_LINE).min(height)] {
                    for pixel in row
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .skip(left)
                        .take(COLUMN_WIDTH)
                    {
                        *pixel = colour;
                    }
                }
            }
        }
    }

    /// Its colours, in the theme's tint.
    fn palette(&self) -> Palette {
        let Palette(colours) = match self.battery {
            Some(battery) if self.kind == Kind::Battery => battery.palette(),
            _ => self.kind.palette(self.vpn),
        };
        Palette(colours.map(|colour| self.tint.apply(colour)))
    }

    /// Colours it in the theme's tint from the next draw.
    pub(crate) const fn set_tint(&mut self, tint: FaceTint) {
        self.tint = tint;
    }

    /// The value right of the icon: one line for CPU and memory (percent of the whole), two
    /// for rates (sent or written above, received or read below, in their graph colours),
    /// and for the battery its charge, over the time to empty or full when known.
    #[allow(clippy::cast_precision_loss)]
    fn draw_value(&self, rows: &mut [&mut [u8]], width: usize, height: usize) {
        let recent = self.history.len().min(VALUE_SAMPLES);
        if recent == 0 {
            return;
        }
        let mean = |part: usize| {
            self.history
                .iter()
                .rev()
                .take(recent)
                .map(|sample| sample[part])
                .sum::<f64>()
                / recent as f64
        };
        let right = width.saturating_sub(VALUE_RIGHT_MARGIN);
        let lines = match self.kind {
            Kind::Cpu => vec![(format_percent(mean(0) + mean(1)), WHITE)],
            Kind::Memory => vec![(format_percent(mean(0)), WHITE)],
            Kind::Fan => {
                let Palette([_, lower, upper]) = self.palette();
                let mut lines = vec![(format_rpm(mean(0)), upper)];
                if self.fans.len() > 1 {
                    lines.push((format_rpm(mean(1)), lower));
                }
                lines
            }
            Kind::Temperature => {
                let critical = self
                    .sensor
                    .as_ref()
                    .map_or(DEFAULT_CRITICAL, |sensor| sensor.1);
                let celsius = (mean(0) + mean(1)) * critical;
                vec![(format_temperature(celsius, self.fahrenheit), WHITE)]
            }
            Kind::Battery => {
                let Palette([_, fill, _]) = self.palette();
                let percent = (format_percent(mean(0)), WHITE);
                match self.battery.and_then(|battery| battery.hours) {
                    Some(hours) => vec![percent, (format_hours(hours), fill)],
                    None => vec![percent],
                }
            }
            Kind::Disk | Kind::Network => {
                let Palette([_, lower, upper]) = self.palette();
                vec![(format_rate(mean(1)), upper), (format_rate(mean(0)), lower)]
            }
        };
        let glyphs: &[(char, AlphaMask)] = if lines.len() == 1 {
            &value_glyphs::LARGE
        } else {
            &value_glyphs::SMALL
        };
        let line_height = glyphs[0].1.height as usize;
        let block = lines.len() * line_height + (lines.len() - 1) * LINE_GAP;
        let mut top = height.saturating_sub(block) / 2;
        for (text, colour) in lines {
            let masks: Vec<AlphaMask> = text
                .chars()
                .filter_map(|wanted| {
                    glyphs
                        .iter()
                        .find(|(char, _)| *char == wanted)
                        .map(|(_, mask)| *mask)
                })
                .collect();
            let text_width: usize = masks.iter().map(|mask| mask.width as usize).sum();
            let mut x = right
                .saturating_sub(text_width)
                .max(LIVE_ICON_WIDTH as usize);
            for mask in masks {
                blit(rows, x, top, mask, colour);
                x += mask.width as usize;
            }
            top += line_height + LINE_GAP;
        }
    }
}

/// Draws `mask` at (`left`, `top`) in `colour` over a dark outline, clipped to `rows`.
fn blit(rows: &mut [&mut [u8]], left: usize, top: usize, mask: AlphaMask, colour: [u8; 4]) {
    let (width, height) = (mask.width as usize, mask.height as usize);
    // Coordinates below are shifted by the halo radius: (r, r) is the mask's corner.
    let r = HALO_RADIUS;
    let coverage = |x: usize, y: usize| -> u8 {
        match (x.checked_sub(r), y.checked_sub(r)) {
            (Some(x), Some(y)) if x < width && y < height => mask.pixels[y * width + x],
            _ => 0,
        }
    };
    for y in 0..height + 2 * r {
        let Some(row) = (top + y).checked_sub(r).and_then(|row| rows.get_mut(row)) else {
            continue;
        };
        for x in 0..width + 2 * r {
            let Some(pixel) = (left + x)
                .checked_sub(r)
                .and_then(|column| row.as_chunks_mut::<4>().0.get_mut(column))
            else {
                continue;
            };
            let mut halo = 0;
            for dy in 0..=2 * r {
                for dx in 0..=2 * r {
                    let (ox, oy) = (dx.abs_diff(r), dy.abs_diff(r));
                    if ox * ox + oy * oy <= r * r {
                        halo =
                            halo.max(coverage((x + dx).wrapping_sub(r), (y + dy).wrapping_sub(r)));
                    }
                }
            }
            blend(pixel, HALO, halo);
            blend(pixel, colour, coverage(x, y));
        }
    }
}

fn blend(pixel: &mut [u8; 4], colour: [u8; 4], alpha: u8) {
    if alpha == 0 {
        return;
    }
    let alpha = u16::from(alpha);
    for (channel, source) in pixel.iter_mut().zip(colour).take(3) {
        let mixed = (u16::from(source) * alpha + u16::from(*channel) * (255 - alpha) + 127) / 255;
        *channel = u8::try_from(mixed).unwrap_or(u8::MAX);
    }
}

/// A fraction of the whole as a whole percentage: "7%", "42%", "100%".
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn format_percent(fraction: f64) -> String {
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0).round() as u32)
}

/// Bytes per second in at most three digits with SI units (powers of 1000):
/// "0B", "512B", "1.23kB", "45.6MB", "789GB".
fn format_rate(bytes: f64) -> String {
    let mut value = bytes.max(0.0);
    let mut unit = 0;
    while value.round() >= 1000.0 && unit < 3 {
        value /= 1000.0;
        unit += 1;
    }
    let suffix = ["B", "kB", "MB", "GB"][unit];
    if unit == 0 || value >= 99.95 {
        format!("{value:.0}{suffix}")
    } else if value >= 9.995 {
        format!("{value:.1}{suffix}")
    } else {
        format!("{value:.2}{suffix}")
    }
}

/// Whole degrees: "54°C", or "130°F".
#[allow(clippy::cast_possible_truncation)]
fn format_temperature(celsius: f64, fahrenheit: bool) -> String {
    if fahrenheit {
        format!("{}°F", celsius.mul_add(1.8, 32.0).round() as i32)
    } else {
        format!("{}°C", celsius.round() as i32)
    }
}

/// Whole rpm: "2153".
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn format_rpm(rpm: f64) -> String {
    format!("{}", rpm.max(0.0).round() as u32)
}

/// The first two fans of a known driver, in its order (on Apple's SMC, `fan1` is the left fan).
fn find_fans() -> Vec<Fan> {
    let Ok(entries) = fs::read_dir("/sys/class/hwmon") else {
        return Vec::new();
    };
    for hwmon in entries.flatten().map(|entry| entry.path()) {
        let name = fs::read_to_string(hwmon.join("name")).unwrap_or_default();
        if !FAN_DRIVERS.contains(&name.trim()) {
            continue;
        }
        return (1..=2)
            .filter_map(|index| {
                let path = |suffix: &str| hwmon.join(format!("fan{index}_{suffix}"));
                let input = path("input");
                read_number(&input)?;
                let min = read_number(&path("min")).unwrap_or(0.0);
                let max = read_number(&path("max")).unwrap_or(6000.0);
                Some(Fan { input, min, max })
            })
            .collect();
    }
    Vec::new()
}

/// A number from a sysfs file.
fn read_number(path: &Path) -> Option<f64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The CPU package sensor's input file and critical limit (°C): a known hwmon driver's
/// package reading, or else the package thermal zone.
fn find_package_sensor() -> Option<(PathBuf, f64)> {
    let entries = |directory: &str| {
        fs::read_dir(directory)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
    };
    let read = |path: PathBuf| fs::read_to_string(path).ok();
    for hwmon in entries("/sys/class/hwmon") {
        let Some(name) = read(hwmon.join("name")) else {
            continue;
        };
        let Some(&(_, label)) = PACKAGE_SENSORS
            .iter()
            .find(|(driver, _)| *driver == name.trim())
        else {
            continue;
        };
        for index in 1..=64 {
            let base = |suffix: &str| hwmon.join(format!("temp{index}_{suffix}"));
            if read(base("label")).is_some_and(|text| text.trim() == label) {
                let critical = read_millidegrees(&base("crit")).unwrap_or(DEFAULT_CRITICAL);
                return Some((base("input"), critical));
            }
        }
    }
    entries("/sys/class/thermal")
        .find(|zone| read(zone.join("type")).is_some_and(|kind| kind.trim() == THERMAL_ZONE_TYPE))
        .map(|zone| (zone.join("temp"), DEFAULT_CRITICAL))
}

/// A sysfs temperature in millidegrees, as °C.
fn read_millidegrees(path: &Path) -> Option<f64> {
    Some(read_number(path)? / 1000.0)
}

/// Hours as "h:mm": "0:07", "2:45", "13:00".
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn format_hours(hours: f64) -> String {
    let minutes = (hours.max(0.0) * 60.0).round() as u32;
    format!("{}:{:02}", minutes / 60, minutes % 60)
}

/// True when a mains or USB supply is online. Used to light the battery button.
pub(crate) fn on_external_power() -> bool {
    supplies().any(|values| {
        matches!(
            field(&values, "TYPE"),
            Some("Mains" | "USB" | "USB_C" | "USB_PD")
        ) && field(&values, "ONLINE") == Some("1")
    })
}

/// The first system battery (peripherals such as a mouse have scope `Device`).
fn read_battery() -> Option<Battery> {
    let values = supplies().find(|values| {
        field(values, "TYPE") == Some("Battery")
            && field(values, "SCOPE") != Some("Device")
            && field(values, "PRESENT") != Some("0")
    })?;
    parse_battery(&values, on_external_power())
}

/// A battery from its `uevent` values: the charge as now over full (charge or energy,
/// whichever it reports), as `upower` and the desktop show it, else `CAPACITY` (some
/// batteries report that against the design capacity, so a worn one reads low); the
/// estimate from charge and current (or energy and power).
#[allow(clippy::cast_precision_loss)]
fn parse_battery(values: &str, mains: bool) -> Option<Battery> {
    let number = |name: &str| field(values, name)?.parse::<f64>().ok();
    let status = field(values, "STATUS").unwrap_or("Unknown");
    let now_over_full =
        |now: &str, full: &str| Some(number(now)? / number(full).filter(|full| *full > 0.0)?);
    let level = now_over_full("CHARGE_NOW", "CHARGE_FULL")
        .or_else(|| now_over_full("ENERGY_NOW", "ENERGY_FULL"))
        .or_else(|| Some(number("CAPACITY")? / 100.0))?
        .clamp(0.0, 1.0);
    let charging = status == "Charging";
    let powered = mains || charging || status == "Full";
    let (now, full, rate) = match number("CHARGE_NOW") {
        Some(now) => (
            Some(now),
            number("CHARGE_FULL"),
            number("CURRENT_AVG")
                .filter(|current| *current != 0.0)
                .or_else(|| number("CURRENT_NOW")),
        ),
        None => (
            number("ENERGY_NOW"),
            number("ENERGY_FULL"),
            number("POWER_AVG")
                .filter(|power| *power != 0.0)
                .or_else(|| number("POWER_NOW")),
        ),
    };
    let rate = rate.map(f64::abs).filter(|rate| *rate > 0.0);
    let hours = match (status, now, full, rate) {
        ("Discharging", Some(now), _, Some(rate)) if !mains => Some(now / rate),
        ("Charging", Some(now), Some(full), Some(rate)) => Some((full - now).max(0.0) / rate),
        _ => None,
    }
    .filter(|hours| *hours <= BATTERY_MAX_HOURS);
    Some(Battery {
        level,
        powered,
        hours,
    })
}

/// Every power supply's `uevent` text.
fn supplies() -> impl Iterator<Item = String> {
    fs::read_dir(POWER_SUPPLIES)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path().join("uevent")).ok())
}

/// `POWER_SUPPLY_<name>`'s value in a `uevent` text.
fn field<'a>(values: &'a str, name: &str) -> Option<&'a str> {
    values.lines().find_map(|line| {
        line.strip_prefix("POWER_SUPPLY_")?
            .strip_prefix(name)?
            .strip_prefix('=')
    })
}

/// `/proc/stat` totals: user + nice, system + irq + softirq + steal, and all jiffies.
fn read_cpu() -> Option<Counters> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let fields: Vec<u64> = stat
        .lines()
        .next()?
        .strip_prefix("cpu ")?
        .split_whitespace()
        .map(|field| field.parse().unwrap_or(0))
        .collect();
    let field = |index: usize| fields.get(index).copied().unwrap_or(0);
    Some(Counters {
        first: field(0) + field(1),
        second: field(2) + field(5) + field(6) + field(7),
        // user nice system idle iowait irq softirq steal (guest time is inside user).
        total: (0..8).map(field).sum(),
    })
}

/// Fractions of `MemTotal`: in use (not reclaimable), and buffers + page cache.
#[allow(clippy::cast_precision_loss)]
fn read_memory() -> Option<[f64; 2]> {
    let info = fs::read_to_string("/proc/meminfo").ok()?;
    let value = |name: &str| {
        info.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let total = value("MemTotal");
    if total == 0 {
        return None;
    }
    let cache = value("Buffers") + value("Cached") + value("SReclaimable");
    let used = total.saturating_sub(value("MemFree") + cache);
    let total = total as f64;
    Some([used as f64 / total, cache as f64 / total])
}

/// Bytes read and written by whole physical disks (`/sys/block/*/device`), from
/// `/proc/diskstats` sectors.
fn read_disk() -> Option<Counters> {
    let stats = fs::read_to_string("/proc/diskstats").ok()?;
    let mut counters = Counters::default();
    for line in stats.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(name) = fields.get(2) else { continue };
        if !Path::new("/sys/block").join(name).join("device").exists() {
            continue;
        }
        let field = |index: usize| -> u64 {
            fields
                .get(index)
                .and_then(|field| field.parse().ok())
                .unwrap_or(0)
        };
        counters.first += field(5);
        counters.second += field(9);
    }
    counters.first *= SECTOR_BYTES;
    counters.second *= SECTOR_BYTES;
    Some(counters)
}

/// Bytes received and sent, from `/proc/net/dev`: by `interface`, or by every interface
/// of a device that is up (so loopback, the T1's own link and tunnels don't count twice).
fn read_network(interface: Option<&str>) -> Option<Counters> {
    let stats = fs::read_to_string("/proc/net/dev").ok()?;
    let mut counters = Counters::default();
    for line in stats.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let counted = match interface {
            Some(interface) => name == interface,
            None => is_physical_and_up(name),
        };
        if !counted {
            continue;
        }
        let fields: Vec<u64> = rest
            .split_whitespace()
            .map(|field| field.parse().unwrap_or(0))
            .collect();
        counters.first += fields.first().copied().unwrap_or(0);
        counters.second += fields.get(8).copied().unwrap_or(0);
    }
    Some(counters)
}

fn is_physical_and_up(name: &str) -> bool {
    let base = Path::new("/sys/class/net").join(name);
    base.join("device").exists()
        && fs::read_to_string(base.join("operstate")).is_ok_and(|state| state.trim() == "up")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_stacks_the_newest_sample_at_the_right() {
        let mut meter = Meter::new(ExtraIcon::Memory, None, 6).unwrap();
        meter.history.push_back([0.5, 0.25]);
        let (width, height) = (6, 4);
        let mut pixels = vec![9_u8; width * height * 4];
        meter.draw(pixels.chunks_exact_mut(width * 4), width, height);
        let Palette([background, lower, upper]) = Kind::Memory.palette(false);
        let at = |x: usize, y: usize| -> [u8; 4] {
            pixels[(y * width + x) * 4..][..4].try_into().unwrap()
        };
        // Rightmost column (2 px): half lower, a quarter upper, the top row background.
        for x in 4..6 {
            assert_eq!(
                [at(x, 0), at(x, 1), at(x, 2), at(x, 3)],
                [background, upper, lower, lower]
            );
        }
        // No history yet on the left.
        assert!((0..height).all(|y| at(0, y) == background));
    }

    #[test]
    fn rate_scale_follows_the_peak_down_slowly() {
        let mut meter = Meter::new(ExtraIcon::Network, None, 4).unwrap();
        for sample in [[1e6, 0.0], [0.0, 0.0], [0.0, 0.0]] {
            meter.history.push_back(sample);
            if meter.history.len() > meter.capacity {
                meter.history.pop_front();
            }
            meter.rescale();
        }
        // The 1 MB/s peak has scrolled off (capacity 2), but the scale eases down.
        assert!(meter.scale < 1.1e6 && meter.scale > NETWORK_FLOOR * 1.1);
    }

    #[test]
    fn values_print_in_three_digits_at_most() {
        let cases = [
            (0.0, "0B"),
            (512.0, "512B"),
            (999.4, "999B"),
            (999.6, "1.00kB"),
            (1234.0, "1.23kB"),
            (45_678.0, "45.7kB"),
            (99_960.0, "100kB"),
            (789_000_000_000.0, "789GB"),
        ];
        for (bytes, text) in cases {
            assert_eq!(format_rate(bytes), text, "{bytes}");
        }
        assert_eq!(format_percent(0.423), "42%");
        assert_eq!(format_percent(1.2), "100%");
        assert_eq!(format_temperature(54.4, false), "54°C");
        assert_eq!(format_temperature(54.4, true), "130°F");
        assert_eq!(format_temperature(100.0, true), "212°F");
        assert_eq!(format_rpm(2153.4), "2153");
        assert_eq!(format_hours(0.12), "0:07");
        assert_eq!(format_hours(2.75), "2:45");
        assert_eq!(format_hours(12.999), "13:00");
    }

    #[test]
    fn battery_reads_charge_power_and_estimate() {
        // This MacBook Pro's BAT0, discharging.
        let uevent = "POWER_SUPPLY_NAME=BAT0\nPOWER_SUPPLY_TYPE=Battery\n\
            POWER_SUPPLY_STATUS=Discharging\nPOWER_SUPPLY_PRESENT=1\n\
            POWER_SUPPLY_CURRENT_NOW=1721000\nPOWER_SUPPLY_CURRENT_AVG=1776000\n\
            POWER_SUPPLY_CHARGE_FULL=5147000\nPOWER_SUPPLY_CHARGE_NOW=4882000\n\
            POWER_SUPPLY_CAPACITY=74\n";
        let battery = parse_battery(uevent, false).unwrap();
        // Now over full (94.9%), not CAPACITY (74%, against the design capacity).
        assert!((battery.level - 4_882_000.0 / 5_147_000.0).abs() < 1e-9);
        assert!(!battery.powered);
        assert_eq!(battery.hours.map(format_hours).as_deref(), Some("2:45"));
        // Plugged in: powered, and no time to empty.
        let battery = parse_battery(uevent, true).unwrap();
        assert!(battery.powered && battery.hours.is_none());
        // Charging, reported as energy and power.
        let uevent = "POWER_SUPPLY_STATUS=Charging\nPOWER_SUPPLY_ENERGY_NOW=30000000\n\
            POWER_SUPPLY_ENERGY_FULL=50000000\nPOWER_SUPPLY_POWER_NOW=20000000\n\
            POWER_SUPPLY_CAPACITY=60\n";
        let battery = parse_battery(uevent, true).unwrap();
        assert_eq!(battery.hours.map(format_hours).as_deref(), Some("1:00"));
        assert!((battery.level - 0.6).abs() < 1e-9);
        // Only CAPACITY: used as is. Charge a little over full (as now, plugged in): 100%.
        let level = |uevent| parse_battery(uevent, true).unwrap().level;
        assert!((level("POWER_SUPPLY_CAPACITY=42\n") - 0.42).abs() < 1e-9);
        assert!(
            (level("POWER_SUPPLY_CHARGE_NOW=5174000\nPOWER_SUPPLY_CHARGE_FULL=5147000\n") - 1.0)
                .abs()
                < 1e-9
        );
        // `CAPACITY_LEVEL` doesn't pass for `CAPACITY`.
        assert_eq!(
            field("POWER_SUPPLY_CAPACITY_LEVEL=Normal\n", "CAPACITY"),
            None
        );
    }

    #[test]
    fn fan_lines_share_one_range() {
        let fan = |min, max| Fan {
            input: PathBuf::new(),
            min,
            max,
        };
        let fans = [fan(2160.0, 5927.0), fan(2000.0, 5489.0)];
        assert_eq!(fan_range(&fans), (1800.0, 5927.0));
        assert_eq!(fan_range(&[]), (0.0, 6000.0));
        // Idle sits near the bottom, both lines drawn, the left over the right.
        let mut meter = Meter::new(ExtraIcon::Fan, None, 10).unwrap();
        meter.fans = fans.to_vec();
        meter.history.push_back([5927.0, 1800.0]);
        let (width, height) = (10, 20);
        let mut pixels = vec![0_u8; width * height * 4];
        meter.draw_fan_lines(
            &mut pixels.chunks_exact_mut(width * 4).collect::<Vec<_>>(),
            width,
            height,
        );
        let Palette([_, right, left]) = Kind::Fan.palette(false);
        let at = |x: usize, y: usize| -> [u8; 4] {
            pixels[(y * width + x) * 4..][..4].try_into().unwrap()
        };
        assert_eq!(at(9, 0), left);
        assert_eq!(at(9, height - 1), right);
    }

    #[test]
    fn battery_colours_follow_power_and_charge() {
        let battery = |level, powered| Battery {
            level,
            powered,
            hours: None,
        };
        let fill = |battery: Battery| battery.palette().0[1];
        assert_eq!(fill(battery(0.05, true)), bgrx(100, 220, 130));
        assert_eq!(fill(battery(0.5, false)), Kind::Battery.palette(false).0[1]);
        assert_eq!(fill(battery(0.2, false)), bgrx(240, 175, 50));
        assert_eq!(fill(battery(0.1, false)), bgrx(235, 75, 60));
    }

    #[test]
    fn every_printed_character_has_a_glyph() {
        for glyphs in [&value_glyphs::LARGE, &value_glyphs::SMALL] {
            for char in "0123456789.%kMGB:°CF".chars() {
                assert!(glyphs.iter().any(|(glyph, _)| *glyph == char), "{char}");
            }
        }
    }

    #[test]
    fn reads_this_machine() {
        assert!(read_cpu().is_some_and(|counters| counters.total > 0));
        assert!(read_memory().is_some_and(|[used, _]| used > 0.0 && used < 1.0));
        assert!(read_disk().is_some());
        assert!(read_network(None).is_some());
        for fan in find_fans() {
            assert!(fan.max > fan.min);
            assert!(read_number(&fan.input).is_some());
        }
        // Machines without a known package sensor are fine; check the reading when found.
        if let Some((input, critical)) = find_package_sensor() {
            assert!(critical > 50.0);
            assert!(read_millidegrees(&input).is_some_and(|celsius| celsius > 10.0));
        }
    }
}
