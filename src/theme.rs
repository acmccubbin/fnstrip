//! Colour themes for the whole bar, picked with `theme = "mono"`, with single colours
//! overridden in a `[colors]` section of the config.
//!
//! A theme sets the background, the plates (normal, pressed and lit), the labels and the
//! slider fills, and how the live faces (meters, spectra, the camera picture) are coloured:
//! as they are, in grey, or in the button's own hue. Pastel and rainbow give every button
//! its own hue along the bar; all plates are plain colours, no textures. The Touch ID
//! prompt, the test screen and the startup animation keep their own colours.

use crate::framebuffer::Rgb;

/// How colours vary along the bar.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Hues {
    /// Plates in the theme's colours; live faces in their own colours.
    Plain,
    /// Plates in the theme's colours; live faces in grey (the camera too).
    Grey,
    /// A soft hue per button, for plates and live faces.
    Pastel,
    /// A strong hue per button, red at the left to violet at the right.
    Rainbow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Theme {
    pub background: Rgb,
    pub plate: Rgb,
    pub pressed: Rgb,
    pub label: Rgb,
    /// Icons and percentages of a muted volume or an off keyboard backlight.
    pub dim: Rgb,
    /// Plates whose thing is on (extras toggles, play/pause while playing).
    pub lit: Rgb,
    pub lit_pressed: Rgb,
    /// Slider fills, and the fill of a muted volume.
    pub fill: Rgb,
    pub muted: Rgb,
    pub hues: Hues,
}

const fn rgb(red: u8, green: u8, blue: u8) -> Rgb {
    Rgb { red, green, blue }
}

impl Default for Theme {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Theme {
    pub const NAMES: [&str; 4] = ["default", "mono", "pastel", "rainbow"];
    /// The colour names a `[colors]` section takes.
    pub const COLOR_NAMES: [&str; 9] = [
        "background",
        "plate",
        "pressed",
        "label",
        "dim",
        "lit",
        "lit_pressed",
        "fill",
        "muted",
    ];

    pub const DEFAULT: Self = Self {
        background: rgb(0, 0, 0),
        plate: rgb(53, 53, 53),
        pressed: rgb(76, 76, 76),
        label: rgb(242, 242, 242),
        dim: rgb(150, 150, 150),
        lit: rgb(32, 128, 64),
        lit_pressed: rgb(58, 164, 92),
        fill: rgb(110, 110, 110),
        muted: rgb(72, 72, 72),
        hues: Hues::Plain,
    };

    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        let base = Self::DEFAULT;
        Some(match name {
            "default" => base,
            // Greys all the way across, meters and camera included.
            "mono" => Self {
                plate: rgb(26, 26, 26),
                pressed: rgb(70, 70, 70),
                label: rgb(255, 255, 255),
                dim: rgb(110, 110, 110),
                lit: rgb(118, 118, 118),
                lit_pressed: rgb(150, 150, 150),
                fill: rgb(88, 88, 88),
                muted: rgb(44, 44, 44),
                hues: Hues::Grey,
                ..base
            },
            "pastel" => Self {
                background: rgb(26, 24, 32),
                plate: rgb(205, 205, 205),
                pressed: rgb(236, 236, 236),
                label: rgb(46, 40, 60),
                dim: rgb(120, 112, 132),
                lit: rgb(96, 178, 136),
                lit_pressed: rgb(128, 200, 162),
                fill: rgb(160, 160, 160),
                muted: rgb(190, 190, 190),
                hues: Hues::Pastel,
            },
            "rainbow" => Self {
                plate: rgb(66, 66, 66),
                pressed: rgb(100, 100, 100),
                fill: rgb(108, 108, 108),
                muted: rgb(80, 80, 80),
                lit: rgb(225, 30, 120),
                lit_pressed: rgb(245, 80, 155),
                hues: Hues::Rainbow,
                ..base
            },
            _ => return None,
        })
    }

    /// Sets one `[colors]` entry: a colour name and `#rrggbb`.
    ///
    /// # Errors
    ///
    /// An unknown name or a malformed value, as a message for the config error.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let colour =
            parse_hex(value).ok_or_else(|| format!("`{key}` must be a colour like \"#3a5f2b\""))?;
        let slot = match key {
            "background" => &mut self.background,
            "plate" => &mut self.plate,
            "pressed" => &mut self.pressed,
            "label" => &mut self.label,
            "dim" => &mut self.dim,
            "lit" => &mut self.lit,
            "lit_pressed" => &mut self.lit_pressed,
            "fill" => &mut self.fill,
            "muted" => &mut self.muted,
            _ => {
                return Err(format!(
                    "unknown colour `{key}` (known: {})",
                    Self::COLOR_NAMES.join(", ")
                ));
            }
        };
        *slot = colour;
        Ok(())
    }

    /// The colour of surface `base` (plate, pressed or a fill) on the plate starting at
    /// `plate_x` of a bar `width` pixels wide: `base` itself, or in the button's hue at
    /// `base`'s brightness (so a pressed plate stays lighter).
    #[must_use]
    pub fn plate_colour(&self, base: Rgb, plate_x: u32, width: u32) -> Rgb {
        let level = luma(base) / 255.0;
        match self.hues {
            Hues::Plain | Hues::Grey => base,
            Hues::Pastel => hsl(pastel_hue(plate_x, width), 0.6, level),
            Hues::Rainbow => hsv(rainbow_hue(plate_x, width), 0.72, (level * 1.9).min(1.0)),
        }
    }

    /// How the live face of the plate starting at `plate_x` is coloured.
    #[must_use]
    pub fn face_tint(&self, plate_x: u32, width: u32) -> FaceTint {
        match self.hues {
            Hues::Plain => FaceTint::None,
            Hues::Grey => FaceTint::Grey,
            Hues::Pastel => FaceTint::Hue(pastel_hue(plate_x, width), 0.5),
            Hues::Rainbow => FaceTint::Hue(rainbow_hue(plate_x, width), 1.0),
        }
    }
}

/// Recolouring of a live face (meter, spectrum, camera picture).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FaceTint {
    /// Its own colours.
    None,
    /// Grey at the same brightness.
    Grey,
    /// This hue (degrees), the saturation scaled by the factor, the brightness kept.
    Hue(f32, f32),
}

impl FaceTint {
    /// One XRGB8888 pixel (B, G, R, X), recoloured.
    #[must_use]
    pub fn apply(self, bgrx: [u8; 4]) -> [u8; 4] {
        let colour = rgb(bgrx[2], bgrx[1], bgrx[0]);
        let tinted = match self {
            Self::None => return bgrx,
            Self::Grey => {
                let level = byte(luma(colour) / 255.0);
                rgb(level, level, level)
            }
            Self::Hue(hue, saturation_scale) => {
                let (_, saturation, value) = to_hsv(colour);
                hsv(hue, (saturation * saturation_scale).min(1.0), value)
            }
        };
        [tinted.blue, tinted.green, tinted.red, bgrx[3]]
    }

    /// Whether it changes nothing (to skip per-pixel work).
    #[must_use]
    pub const fn is_none(self) -> bool {
        matches!(self, Self::None)
    }
}

/// A soft colour per button, from where it starts along the bar (about one step per
/// button: the extras pitch is 190 of 2170 native px).
fn pastel_hue(plate_x: u32, width: u32) -> f32 {
    let slot = plate_x * 2170 / width.max(1) / 95;
    f32_from(slot * 67 % 360)
}

/// Red at the left end to violet at the right, by where the button starts.
fn rainbow_hue(plate_x: u32, width: u32) -> f32 {
    f32_from(plate_x) / f32_from(width.max(1)) * 300.0
}

fn parse_hex(value: &str) -> Option<Rgb> {
    let digits = value.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(rgb(channel(0)?, channel(2)?, channel(4)?))
}

#[allow(clippy::cast_precision_loss)]
const fn f32_from(value: u32) -> f32 {
    value as f32
}

fn luma(colour: Rgb) -> f32 {
    0.2126f32.mul_add(
        f32::from(colour.red),
        0.7152f32.mul_add(f32::from(colour.green), 0.0722 * f32::from(colour.blue)),
    )
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Hue (degrees), saturation and value (0 to 1).
fn to_hsv(colour: Rgb) -> (f32, f32, f32) {
    let [red, green, blue] =
        [colour.red, colour.green, colour.blue].map(|channel| f32::from(channel) / 255.0);
    let max = red.max(green).max(blue);
    let chroma = max - red.min(green).min(blue);
    let hue = if chroma <= 0.0 {
        0.0
    } else if (max - red).abs() < f32::EPSILON {
        60.0 * ((green - blue) / chroma).rem_euclid(6.0)
    } else if (max - green).abs() < f32::EPSILON {
        60.0 * ((blue - red) / chroma + 2.0)
    } else {
        60.0 * ((red - green) / chroma + 4.0)
    };
    let saturation = if max <= 0.0 { 0.0 } else { chroma / max };
    (hue, saturation, max)
}

fn hsv(hue: f32, saturation: f32, value: f32) -> Rgb {
    let chroma = value * saturation;
    from_hue(hue, chroma, value - chroma)
}

fn hsl(hue: f32, saturation: f32, lightness: f32) -> Rgb {
    let chroma = (1.0 - 2.0f32.mul_add(lightness, -1.0).abs()) * saturation;
    from_hue(hue, chroma, lightness - chroma / 2.0)
}

fn from_hue(hue: f32, chroma: f32, offset: f32) -> Rgb {
    let sector = (hue / 60.0).rem_euclid(6.0);
    let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (red, green, blue) = match sector {
        s if s < 1.0 => (chroma, second, 0.0),
        s if s < 2.0 => (second, chroma, 0.0),
        s if s < 3.0 => (0.0, chroma, second),
        s if s < 4.0 => (0.0, second, chroma),
        s if s < 5.0 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    rgb(
        byte(red + offset),
        byte(green + offset),
        byte(blue + offset),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_named_theme_exists_and_default_is_the_classic_look() {
        for name in Theme::NAMES {
            assert!(Theme::named(name).is_some(), "{name}");
        }
        assert_eq!(Theme::named("default"), Some(Theme::default()));
        assert_eq!(Theme::named("camo"), None);
    }

    #[test]
    fn colours_can_be_overridden() {
        let mut theme = Theme::named("mono").unwrap();
        theme.set("plate", "#3a5f2b").unwrap();
        assert_eq!(theme.plate, rgb(0x3a, 0x5f, 0x2b));
        assert!(theme.set("plate", "3a5f2b").is_err());
        assert!(theme.set("plate", "#3a5f2").is_err());
        assert!(theme.set("plaid", "#3a5f2b").is_err());
    }

    #[test]
    fn hued_plates_keep_pressed_lighter_and_each_button_one_colour() {
        for name in ["pastel", "rainbow"] {
            let theme = Theme::named(name).unwrap();
            for x in [0, 400, 1_200, 2_000] {
                let plate = theme.plate_colour(theme.plate, x, 2_170);
                let pressed = theme.plate_colour(theme.pressed, x, 2_170);
                assert!(luma(pressed) > luma(plate), "{name} at {x}");
            }
        }
        let rainbow = Theme::named("rainbow").unwrap();
        let left = rainbow.plate_colour(rainbow.plate, 0, 2_170);
        let right = rainbow.plate_colour(rainbow.plate, 2_000, 2_170);
        assert!(left.red > left.blue);
        assert!(right.blue > right.green);
    }

    #[test]
    fn face_tints_grey_or_rehue_and_keep_brightness() {
        let cpu_blue = [235, 150, 70, 0];
        assert_eq!(FaceTint::None.apply(cpu_blue), cpu_blue);
        let grey = FaceTint::Grey.apply(cpu_blue);
        assert!(grey[0] == grey[1] && grey[1] == grey[2]);
        let red = FaceTint::Hue(0.0, 1.0).apply(cpu_blue);
        assert!(red[2] > red[1] && red[2] > red[0]);
        assert_eq!(red[2], cpu_blue[0]); // the brightest channel keeps its value
    }
}
