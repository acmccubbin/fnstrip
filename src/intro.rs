//! The startup animation (`startup_animation = "on"`). Two beams of light race in from the
//! ends of the bar and collide in a flash in the middle; the "fnstrip" wordmark glows out
//! of the flash, white-hot and then in colour, with a shimmer across it; then two
//! wavefronts sweep from the middle back out to the ends and uncover the real bar.
//!
//! It is drawn over a freshly rendered stock frame, so the reveal shows the bar as it is
//! right then, and a touch or Fn can drop straight to it at any point. About 1.8 s, once
//! per start of the renderer (not on reconnects, such as after sleep).

// Pixel maths is f32; every float-to-int cast is clamped into range first.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::time::Duration;

use crate::wordmark::WORDMARK;

pub(crate) const DURATION: Duration = Duration::from_millis(1_800);

// The timeline, in seconds.
/// The beams meet in the middle (and flash).
const COLLISION: f32 = 0.42;
/// The wordmark fades in out of the flash, cools from white to colour, gets a shimmer,
/// and fades out once the bar is mostly uncovered.
const LOGO_IN: (f32, f32) = (0.42, 0.62);
const LOGO_COOL: (f32, f32) = (0.45, 0.95);
const SHIMMER: (f32, f32) = (0.7, 1.05);
/// It flares white and fades as the wavefronts leave its edges; the middle of the bar
/// shows once it has gone.
const LOGO_OUT: (f32, f32) = (1.1, 1.4);
/// The wavefronts run from the wordmark's edges to past the ends.
const REVEAL: (f32, f32) = (1.05, 1.65);
/// The flash's afterglow is gone by the end of this.
const AFTERGLOW_OUT: (f32, f32) = (1.1, 1.6);

const CYAN: [f32; 3] = [0.0, 0.78, 1.0];
const MAGENTA: [f32; 3] = [1.0, 0.24, 0.78];
const WARM_WHITE: [f32; 3] = [1.0, 0.95, 0.88];
/// The wordmark's glow reaches this far past its letters.
const HALO_PAD: usize = 12;
/// The reveal's soft edge, in pixels.
const REVEAL_FEATHER: f32 = 30.0;

pub(crate) struct Intro {
    width: usize,
    height: usize,
    /// The wordmark blurred, `HALO_PAD` wider on every side, 0–1.
    halo: Vec<f32>,
    /// When the first frame drew; the timeline starts there.
    start: Option<Duration>,
    /// Frames drawn so far.
    frames: u32,
}

impl Intro {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            width: width as usize,
            height: height as usize,
            halo: blurred_wordmark(),
            start: None,
            frames: 0,
        }
    }

    /// Draws the frame for `now` over the stock frame in `pixels` (XRGB8888, packed rows).
    pub(crate) fn draw(&mut self, pixels: &mut [u8], now: Duration) {
        let start = *self.start.get_or_insert(now);
        self.frames += 1;
        self.draw_at(pixels, now.saturating_sub(start));
    }

    /// When the first frame drew (session clock), if one has.
    pub(crate) const fn started(&self) -> Option<Duration> {
        self.start
    }

    /// Frames drawn so far.
    pub(crate) const fn frames(&self) -> u32 {
        self.frames
    }

    /// Whether the animation has played out (the plain stock frame is next).
    pub(crate) fn finished(&self, now: Duration) -> bool {
        self.start
            .is_some_and(|start| now.saturating_sub(start) >= DURATION)
    }

    /// Draws the frame `elapsed` into the animation over the stock frame in `pixels`.
    pub(crate) fn draw_at(&self, pixels: &mut [u8], elapsed: Duration) {
        let (width, height) = (self.width, self.height);
        if pixels.len() < width * height * 4 || width == 0 || height == 0 {
            return;
        }
        let t = elapsed.as_secs_f32();
        let center = width as f32 / 2.0;
        let middle = (height as f32 - 1.0) / 2.0;

        // Light comes in two vertical shapes: a thin streak along the middle of the bar,
        // and a broad wash over its full height. Each column gets an amount of each.
        let thin_profile: Vec<f32> = (0..height)
            .map(|y| {
                let off = y as f32 - middle;
                gauss(off, 3.5) + 0.35 * gauss(off, 14.0)
            })
            .collect();
        let broad_profile: Vec<f32> = (0..height)
            .map(|y| 0.7 + 0.3 * gauss(y as f32 - middle, 18.0))
            .collect();
        let mut thin = vec![[0.0_f32; 3]; width];
        let mut broad = vec![[0.0_f32; 3]; width];
        let mut uncovered = vec![0.0_f32; width];

        let afterglow = 1.0 - smoothstep(AFTERGLOW_OUT, t);
        let since_collision = t - COLLISION;
        let reveal = smoothstep(REVEAL, t);
        let logo_half = (WORDMARK.width as f32 / 2.0) + 10.0;
        let radius = logo_half + (1.0 - (1.0 - reveal).powi(3)) * (center + 80.0 - logo_half);
        let logo_gone = smoothstep((LOGO_OUT.1 - 0.05, LOGO_OUT.1 + 0.15), t);
        let fronts = if t < REVEAL.0 {
            0.0
        } else {
            smoothstep((REVEAL.0, REVEAL.0 + 0.05), t)
                * (1.0 - smoothstep((REVEAL.1 - 0.12, REVEAL.1), t))
        };

        for x in 0..width {
            let column = x as f32 + 0.5;
            let distance = (column - center).abs();
            let side = if column < center { CYAN } else { MAGENTA };

            // 1. The beams, accelerating in from both ends until they meet.
            if t < COLLISION {
                let progress = t / COLLISION;
                let head_distance = (1.0 - progress * progress) * (center + 60.0);
                let fade_in = (progress * 4.0).min(1.0);
                // Each beam's head points at the middle, its tail trails behind it.
                let past_head = head_distance - distance;
                let beam = if past_head >= 0.0 {
                    (-past_head / 10.0).exp()
                } else {
                    (past_head / 260.0).exp()
                };
                add_hot(&mut thin[x], side, 1.4 * fade_in * beam);
            }

            // 2. The flash where they meet, spreading out and fading, and the line of
            // light it leaves along the whole bar.
            if since_collision >= 0.0 {
                let strength = (1.0 - (-since_collision / 0.03).exp())
                    * (-since_collision / 0.22).exp()
                    * afterglow;
                let spread = 60.0 + 1_400.0 * since_collision;
                let flash = 1.3 * strength * gauss(distance, spread) + 0.12 * strength;
                add_hot(&mut broad[x], WARM_WHITE, flash);
                let line = 0.55 * (-since_collision / 0.35).exp() * afterglow;
                add(
                    &mut thin[x],
                    lerp3(CYAN, MAGENTA, column / width as f32),
                    line,
                );
            }

            // 3. The wavefronts, uncovering the bar behind them.
            if t >= REVEAL.0 {
                let to_front = distance - radius;
                let front = gauss(to_front, 26.0)
                    + if to_front < 0.0 {
                        0.35 * (to_front / 120.0).exp()
                    } else {
                        0.0
                    };
                add_hot(&mut broad[x], side, 1.3 * fronts * front);
                uncovered[x] = (0.5 - to_front / REVEAL_FEATHER).clamp(0.0, 1.0);
                if distance < logo_half + REVEAL_FEATHER {
                    uncovered[x] *= logo_gone;
                }
            }
        }

        let row_bytes = width * 4;
        for y in 0..height {
            let row = &mut pixels[y * row_bytes..(y + 1) * row_bytes];
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let keep = uncovered[x];
                for channel in 0..3 {
                    // XRGB8888 in memory: blue, green, red, unused.
                    let stock = f32::from(pixel[2 - channel]) / 255.0;
                    let light =
                        thin[x][channel] * thin_profile[y] + broad[x][channel] * broad_profile[y];
                    pixel[2 - channel] = to_byte(stock * keep + light);
                }
            }
        }

        self.draw_wordmark(pixels, t);
    }

    /// The wordmark in the middle of the bar, with its glow.
    fn draw_wordmark(&self, pixels: &mut [u8], t: f32) {
        let opacity = smoothstep(LOGO_IN, t) * (1.0 - smoothstep(LOGO_OUT, t));
        if opacity <= 0.0 {
            return;
        }
        let (mask_width, mask_height) = (WORDMARK.width as usize, WORDMARK.height as usize);
        let (halo_width, halo_height) = (mask_width + 2 * HALO_PAD, mask_height + 2 * HALO_PAD);
        let left = self.width.saturating_sub(mask_width) / 2;
        let top = self.height.saturating_sub(mask_height) / 2;
        let white_hot = (1.0 - smoothstep(LOGO_COOL, t)).max(smoothstep(LOGO_OUT, t).sqrt());
        let glow = opacity * (0.45 + 0.75 * white_hot);
        let shimmering = t > SHIMMER.0 && t < SHIMMER.1;
        let shimmer_at = -0.2 + 1.4 * smoothstep(SHIMMER, t);

        for halo_y in 0..halo_height {
            let Some(y) = (top + halo_y).checked_sub(HALO_PAD) else {
                continue;
            };
            if y >= self.height {
                continue;
            }
            for halo_x in 0..halo_width {
                let Some(x) = (left + halo_x).checked_sub(HALO_PAD) else {
                    continue;
                };
                if x >= self.width {
                    continue;
                }
                // Across the wordmark, 0 at its left edge to 1 at its right.
                let across = (halo_x as f32 - HALO_PAD as f32) / mask_width as f32;
                let tint = lerp3(CYAN, MAGENTA, across.clamp(0.0, 1.0));
                let halo = self.halo[halo_y * halo_width + halo_x] * glow;
                let ink = halo_x
                    .checked_sub(HALO_PAD)
                    .zip(halo_y.checked_sub(HALO_PAD))
                    .filter(|&(mask_x, mask_y)| mask_x < mask_width && mask_y < mask_height)
                    .map_or(0.0, |(mask_x, mask_y)| {
                        f32::from(WORDMARK.pixels[mask_y * mask_width + mask_x]) / 255.0
                    })
                    * opacity;
                let mut letters = lerp3(tint, [1.0; 3], 0.25 + 0.75 * white_hot);
                if shimmering {
                    let slant = across + 0.15 * (halo_y as f32 / halo_height as f32);
                    let shine = gauss(slant - shimmer_at, 0.06);
                    letters = lerp3(letters, [1.0; 3], shine);
                }
                let offset = (y * self.width + x) * 4;
                let pixel = &mut pixels[offset..offset + 4];
                for channel in 0..3 {
                    let below = f32::from(pixel[2 - channel]) / 255.0 + tint[channel] * halo;
                    pixel[2 - channel] = to_byte(below * (1.0 - ink) + letters[channel] * ink);
                }
            }
        }
    }
}

/// The wordmark mask, padded by `HALO_PAD` and blurred (three box passes each way, close
/// to a Gaussian), scaled so its brightest point is 1.
fn blurred_wordmark() -> Vec<f32> {
    let (mask_width, mask_height) = (WORDMARK.width as usize, WORDMARK.height as usize);
    let (width, height) = (mask_width + 2 * HALO_PAD, mask_height + 2 * HALO_PAD);
    let mut halo = vec![0.0_f32; width * height];
    for y in 0..mask_height {
        for x in 0..mask_width {
            halo[(y + HALO_PAD) * width + x + HALO_PAD] =
                f32::from(WORDMARK.pixels[y * mask_width + x]) / 255.0;
        }
    }
    let radius = 3_usize;
    let mut scratch = vec![0.0_f32; width.max(height)];
    for _ in 0..3 {
        for y in 0..height {
            box_blur(&mut halo, y * width, 1, width, radius, &mut scratch);
        }
        for x in 0..width {
            box_blur(&mut halo, x, width, height, radius, &mut scratch);
        }
    }
    let brightest = halo.iter().copied().fold(0.0_f32, f32::max);
    if brightest > 0.0 {
        for value in &mut halo {
            *value /= brightest;
        }
    }
    halo
}

/// Blurs the `count` values at `start`, `start + step`, …, in place.
fn box_blur(
    values: &mut [f32],
    start: usize,
    step: usize,
    count: usize,
    radius: usize,
    scratch: &mut [f32],
) {
    for (index, blurred) in scratch.iter_mut().take(count).enumerate() {
        let from = index.saturating_sub(radius);
        let to = (index + radius).min(count - 1);
        let sum: f32 = (from..=to).map(|i| values[start + i * step]).sum();
        *blurred = sum / (2 * radius + 1) as f32;
    }
    for (index, value) in scratch.iter().take(count).enumerate() {
        values[start + index * step] = *value;
    }
}

fn gauss(offset: f32, width: f32) -> f32 {
    (-(offset / width).powi(2)).exp()
}

/// 0 before `range.0`, 1 after `range.1`, a smooth S-curve between.
fn smoothstep(range: (f32, f32), t: f32) -> f32 {
    let x = ((t - range.0) / (range.1 - range.0)).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn lerp3(from: [f32; 3], to: [f32; 3], amount: f32) -> [f32; 3] {
    [0, 1, 2].map(|channel| from[channel] + (to[channel] - from[channel]) * amount)
}

fn add(light: &mut [f32; 3], color: [f32; 3], amount: f32) {
    for channel in 0..3 {
        light[channel] += color[channel] * amount;
    }
}

/// Coloured light that burns white where it is brightest.
fn add_hot(light: &mut [f32; 3], color: [f32; 3], amount: f32) {
    add(light, color, amount);
    let white = (amount - 0.6).max(0.0) * 1.2;
    add(light, [1.0; 3], white);
}

fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: u32 = 2_170;
    const HEIGHT: u32 = 60;

    /// A mid-grey stand-in for the stock bar.
    fn grey() -> Vec<u8> {
        vec![128; (WIDTH * HEIGHT * 4) as usize]
    }

    fn brightness(pixels: &[u8], x: u32, y: u32) -> u32 {
        let offset = ((y * WIDTH + x) * 4) as usize;
        pixels[offset..offset + 3]
            .iter()
            .map(|&v| u32::from(v))
            .sum()
    }

    fn at(seconds: f32) -> Vec<u8> {
        let mut pixels = grey();
        Intro::new(WIDTH, HEIGHT).draw_at(&mut pixels, Duration::from_secs_f32(seconds));
        pixels
    }

    #[test]
    fn starts_dark_and_hides_the_bar() {
        let pixels = at(0.0);
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[..3] == [0, 0, 0])
        );
    }

    #[test]
    fn beams_come_in_from_the_ends() {
        let pixels = at(0.15);
        assert!(brightness(&pixels, 60, 30) > 200);
        assert!(brightness(&pixels, WIDTH - 60, 30) > 200);
        assert_eq!(brightness(&pixels, WIDTH / 2, 30), 0);
    }

    #[test]
    fn the_wordmark_shows_in_the_middle_of_the_flash() {
        let pixels = at(0.8);
        let left = (WIDTH - WORDMARK.width) / 2;
        let top = (HEIGHT - WORDMARK.height) / 2;
        let (x, y) = (0..WORDMARK.width)
            .flat_map(|x| (0..WORDMARK.height).map(move |y| (x, y)))
            .find(|&(x, y)| WORDMARK.pixels[(y * WORDMARK.width + x) as usize] == 255)
            .unwrap();
        assert!(brightness(&pixels, left + x, top + y) > 500);
    }

    #[test]
    fn ends_on_the_bar_unchanged() {
        assert_eq!(at(DURATION.as_secs_f32()), grey());
    }

    #[test]
    fn the_clock_starts_at_the_first_frame() {
        let mut intro = Intro::new(WIDTH, HEIGHT);
        let start = Duration::from_secs(5);
        assert!(!intro.finished(start + DURATION));
        intro.draw(&mut grey(), start);
        assert!(!intro.finished(start + DURATION / 2));
        assert!(intro.finished(start + DURATION));
    }
}
