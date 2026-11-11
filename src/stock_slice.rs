//! Dependency-free Boot Camp-style default Touch Bar surface.

use std::{error::Error, fmt};

use crate::framebuffer::{FramebufferError, Rectangle, Rgb, Xrgb8888Frame};
use crate::gesture::{DisplayContact, Gesture};
use crate::stock_art::{self, Symbol};
use crate::stock_labels::{self, AlphaMask};
use crate::theme::Theme;
use crate::value_glyphs;

const NATIVE_WIDTH: u32 = 2_170;
const NATIVE_HEIGHT: u32 = 60;
const ACTIVE_RIGHT: u32 = NATIVE_WIDTH;
const FUNCTION_SLOT_COUNT: u32 = 13;
const PLATE_Y: u32 = 0;
const PLATE_HEIGHT: u32 = 60;
const PLATE_RADIUS: u32 = 8;
/// Icons over live faces (meters, camera, spectra), which keep their own colours in every
/// theme: light, with a dark halo.
const LIVE_LABEL: Rgb = Theme::DEFAULT.label;
const LIVE_HALO: Rgb = Theme::DEFAULT.background;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FunctionKey {
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// Which row is showing: the default row (the `default_row` setting: controls, or Esc +
/// F1–F12), the other while Fn is held, and our extras after a double-tap of Fn (until the
/// next Fn press).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layer {
    Controls,
    Function,
    Extras,
}

impl Layer {
    #[must_use]
    pub const fn from_fn(fn_held: bool, double_tapped: bool, function_row_first: bool) -> Self {
        match (fn_held, double_tapped) {
            (_, true) if fn_held => Self::Extras,
            (held, _) if held == function_row_first => Self::Controls,
            _ => Self::Function,
        }
    }
}

/// Most buttons the extras row fits (plates of the controls row, from x 194 on).
pub const MAX_EXTRAS: usize = 10;

/// Icons a configured extras button can use. Each has an on and an off look.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtraIcon {
    Camera,
    Mic,
    Vpn,
    /// Meters: always live while the row shows, a scrolling graph under the icon.
    Cpu,
    Memory,
    Disk,
    Network,
    /// Charge level; lit (a bolt on the icon) while on external power.
    Battery,
    /// CPU package temperature.
    Temperature,
    /// Fan speeds (the first two fans).
    Fan,
}

impl ExtraIcon {
    pub const NAMES: [&str; 10] = [
        "camera", "mic", "vpn", "cpu", "mem", "disk", "net", "battery", "temp", "fan",
    ];

    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "camera" => Self::Camera,
            "mic" => Self::Mic,
            "vpn" => Self::Vpn,
            "cpu" => Self::Cpu,
            "mem" => Self::Memory,
            "disk" => Self::Disk,
            "net" => Self::Network,
            "battery" => Self::Battery,
            "temp" => Self::Temperature,
            "fan" => Self::Fan,
            _ => return None,
        })
    }

    /// A system meter, which needs no command or status.
    #[must_use]
    pub const fn is_meter(self) -> bool {
        matches!(
            self,
            Self::Cpu
                | Self::Memory
                | Self::Disk
                | Self::Network
                | Self::Battery
                | Self::Temperature
                | Self::Fan
        )
    }

    /// The icon, lit or not; `battery_bars` is the battery's charge in quarters.
    const fn symbol(self, on: bool, battery_bars: u8) -> Symbol {
        match (self, on) {
            (Self::Camera, true) => Symbol::CameraOn,
            (Self::Camera, false) => Symbol::CameraOff,
            (Self::Mic, true) => Symbol::MicOn,
            (Self::Mic, false) => Symbol::MicOff,
            (Self::Vpn, true) => Symbol::GlobeShield,
            (Self::Vpn, false) => Symbol::Globe,
            (Self::Cpu, _) => Symbol::Chip,
            (Self::Memory, _) => Symbol::Memory,
            (Self::Disk, _) => Symbol::Disk,
            (Self::Network, _) => Symbol::Arrows,
            (Self::Battery, charging) => Symbol::Battery {
                bars: battery_bars,
                charging,
            },
            (Self::Temperature, _) => Symbol::Thermometer,
            (Self::Fan, _) => Symbol::Fan,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepDirection {
    Down,
    Up,
}

/// Independently available default-renderer controls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StockCapabilities(u8);

impl StockCapabilities {
    pub const DISPLAY_BRIGHTNESS: Self = Self(1);
    pub const KEYBOARD_BACKLIGHT: Self = Self(2);
    pub const AUDIO: Self = Self(4);
    pub const MEDIA: Self = Self(8);

    #[must_use]
    pub const fn contains(self, capability: Self) -> bool {
        self.0 & capability.0 == capability.0
    }
}

impl std::ops::BitOr for StockCapabilities {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Renderer-known desktop levels. The fixed Boot Camp face uses mute only;
/// the remaining values stay available to replacement renderers and providers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StockLevels {
    pub display_brightness: Option<u8>,
    pub keyboard_backlight: Option<u8>,
    pub volume: Option<u8>,
    pub muted: bool,
    /// Bit `i` set: extras button `i` is lit.
    pub extras_on: u16,
    /// Bit `i` set: lit extras button `i` shows a live picture (camera, spectrum) that
    /// the session draws into the plate; the bar leaves it black, then adds the corners
    /// and icon on top with [`StockBar::render_live_extras`].
    pub extras_live: u16,
    /// The battery meter's charge in quarters (0 to 4), for its icon's bars.
    pub battery_bars: u8,
    /// Media is playing: the play/pause button is lit.
    pub playing: bool,
    /// The lit play/pause button shows the speakers' spectrum, drawn by the session like
    /// a live extras face (see [`StockBar::render_live_media`]).
    pub media_live: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StockAction {
    Escape,
    Function(FunctionKey),
    StepDisplayBrightness(StepDirection),
    StepKeyboardBacklight(StepDirection),
    ToggleMute,
    /// Keyboard backlight off, or back on to its last level (a tap on the slider's icon).
    ToggleKeyboardBacklight,
    StepVolume(StepDirection),
    MediaPrevious,
    MediaPlayPause,
    MediaNext,
    /// Index into the configured extras buttons.
    Extra(u8),
    /// Set a level from the slider, in percent.
    SetLevel(SliderKind, u8),
}

/// The controls row's level sliders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SliderKind {
    Brightness,
    Keyboard,
    Volume,
}

/// What a touch on a slider hit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SliderSpot {
    /// The volume slider's speaker icon (a tap toggles mute) or the keyboard slider's icon
    /// (a tap turns the backlight off, and back on to its last level).
    Icon,
    /// This level, in percent.
    Level(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StockSliceError {
    InvalidGeometry,
    FrameGeometryMismatch,
    Drawing(FramebufferError),
}

impl fmt::Display for StockSliceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidGeometry => "invalid stock bar geometry",
            Self::FrameGeometryMismatch => "stock bar frame geometry changed",
            Self::Drawing(_) => "stock bar drawing failed",
        })
    }
}

impl Error for StockSliceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Drawing(error) => Some(error),
            Self::InvalidGeometry | Self::FrameGeometryMismatch => None,
        }
    }
}

impl From<FramebufferError> for StockSliceError {
    fn from(error: FramebufferError) -> Self {
        Self::Drawing(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Control {
    DisplayBrightness,
    KeyboardBacklight,
    MediaPrevious,
    MediaPlayPause,
    MediaNext,
    Volume,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeControl {
    control: Control,
    x: u32,
    width: u32,
}

/// The controls row: level sliders where the stock −/+ pairs were (the volume one also takes
/// the mute button's place), media buttons between.
const NORMAL_CONTROLS: [NativeControl; 6] = [
    NativeControl {
        control: Control::DisplayBrightness,
        x: 194,
        width: 372,
    },
    NativeControl {
        control: Control::KeyboardBacklight,
        x: 602,
        width: 372,
    },
    NativeControl {
        control: Control::MediaPrevious,
        x: 1_010,
        width: 182,
    },
    NativeControl {
        control: Control::MediaPlayPause,
        x: 1_200,
        width: 182,
    },
    NativeControl {
        control: Control::MediaNext,
        x: 1_390,
        width: 182,
    },
    NativeControl {
        control: Control::Volume,
        x: 1_608,
        width: 562,
    },
];

/// Level sliders: icon area at the left (native px), the percentage's right margin, and
/// the fill colours.
const SLIDER_ICON_WIDTH: u32 = 72;
const SLIDER_TEXT_MARGIN: u32 = 16;

/// Width of the left part of a graphing extras plate (meters, lit VPN) that holds the
/// icon; the value is printed right of it.
pub(crate) const LIVE_ICON_WIDTH: u32 = 64;

/// Extras plates: the controls row's plate size and pitch, starting where it does.
const EXTRA_X: u32 = 194;
const EXTRA_PITCH: u32 = 190;
const EXTRA_WIDTH: u32 = 182;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LaidOutControl {
    control: Control,
    bounds: Rectangle,
}

/// Pure fixed-geometry layout and hit testing for the maintained default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StockBar {
    width: u32,
    height: u32,
    capabilities: StockCapabilities,
    extras: [Option<ExtraIcon>; MAX_EXTRAS],
    function_row_first: bool,
    theme: Theme,
}

impl StockBar {
    /// # Errors
    ///
    /// Rejects zero dimensions.
    pub const fn new(
        width: u32,
        height: u32,
        capabilities: StockCapabilities,
    ) -> Result<Self, StockSliceError> {
        if width == 0 || height == 0 {
            Err(StockSliceError::InvalidGeometry)
        } else {
            Ok(Self {
                width,
                height,
                capabilities,
                extras: [None; MAX_EXTRAS],
                function_row_first: false,
                theme: Theme::DEFAULT,
            })
        }
    }

    /// The same bar with Esc + F1–F12 as the default row (the controls while Fn is held),
    /// or, if false, the controls as the default (F1–F12 while Fn is held).
    #[must_use]
    pub const fn with_function_row_first(mut self, function_row_first: bool) -> Self {
        self.function_row_first = function_row_first;
        self
    }

    #[must_use]
    pub const fn function_row_first(self) -> bool {
        self.function_row_first
    }

    /// The same bar in these colours.
    #[must_use]
    pub const fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }

    #[must_use]
    pub const fn theme(self) -> Theme {
        self.theme
    }

    /// The same bar with these extras buttons (left to right; extra icons are ignored).
    #[must_use]
    pub fn with_extras(mut self, icons: &[ExtraIcon]) -> Self {
        self.extras = [None; MAX_EXTRAS];
        for (slot, icon) in self.extras.iter_mut().zip(icons) {
            *slot = Some(*icon);
        }
        self
    }

    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }

    #[must_use]
    pub(crate) fn escape_right(self) -> u32 {
        let bounds = self.escape_bounds();
        bounds.x.saturating_add(bounds.width)
    }

    /// # Errors
    ///
    /// Rejects a changed frame geometry or an internally invalid draw.
    pub fn render(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        fn_held: bool,
        levels: StockLevels,
    ) -> Result<(), StockSliceError> {
        self.render_pressed(
            frame,
            Layer::from_fn(fn_held, false, self.function_row_first),
            levels,
            None,
        )
    }

    /// Draws the fixed surface with an optional actively pressed control.
    ///
    /// # Errors
    ///
    /// Rejects a changed frame geometry or an internally invalid draw.
    pub fn render_pressed(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        layer: Layer,
        levels: StockLevels,
        pressed: Option<StockAction>,
    ) -> Result<(), StockSliceError> {
        if frame.width() != self.width || frame.height() != self.height {
            return Err(StockSliceError::FrameGeometryMismatch);
        }
        frame.clear(self.theme.background);
        match layer {
            Layer::Controls => self.render_normal_row(frame, levels, pressed),
            Layer::Function => self.render_function_row(frame, pressed),
            Layer::Extras => self.render_extras_row(frame, levels, pressed),
        }
    }

    /// Maps a completed tap to one currently visible action.
    #[must_use]
    pub fn action_for_gesture(self, gesture: Gesture, layer: Layer) -> Option<StockAction> {
        let Gesture::Tap(contact) = gesture else {
            return None;
        };
        self.action_at(contact, layer)
    }

    /// Maps an active display contact to the control beneath it.
    #[must_use]
    pub fn action_at(self, contact: DisplayContact, layer: Layer) -> Option<StockAction> {
        if contact.x() >= f64::from(self.width) || contact.y() >= f64::from(self.height) {
            return None;
        }
        let x = display_coordinate(contact.x());
        let y = display_coordinate(contact.y());
        match layer {
            Layer::Controls => {}
            Layer::Function => return self.function_hit_test(x, y),
            Layer::Extras => {
                if contains(self.escape_bounds(), x, y) {
                    return Some(StockAction::Escape);
                }
                return self
                    .extra_controls()
                    .into_iter()
                    .find(|(_, _, bounds)| contains(*bounds, x, y))
                    .map(|(index, _, _)| StockAction::Extra(index));
            }
        }
        if contains(self.escape_bounds(), x, y) {
            return Some(StockAction::Escape);
        }
        self.normal_controls()
            .into_iter()
            .find(|item| self.control_available(item.control) && contains(item.bounds, x, y))
            .and_then(|item| control_action(item.control))
    }

    fn render_normal_row(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        levels: StockLevels,
        pressed: Option<StockAction>,
    ) -> Result<(), StockSliceError> {
        self.draw_plate(frame, self.escape_bounds(), StockAction::Escape, pressed)?;
        stock_art::draw_label(
            frame,
            self.escape_bounds(),
            stock_labels::ESC,
            self.theme.label,
        )?;
        for item in self.normal_controls() {
            if !self.control_visible(item.control) {
                continue;
            }
            if let Some(kind) = control_slider(item.control) {
                self.draw_level_slider(frame, item.bounds, kind, levels)?;
                continue;
            }
            if item.control == Control::MediaPlayPause && levels.playing {
                if !levels.media_live {
                    let lit = if pressed == Some(StockAction::MediaPlayPause) {
                        self.theme.lit_pressed
                    } else {
                        self.theme.lit
                    };
                    let radius = self.scaled_y(PLATE_RADIUS).max(1);
                    stock_art::draw_rounded_plate(frame, item.bounds, radius, lit)?;
                    stock_art::draw_symbol(
                        frame,
                        item.bounds,
                        Symbol::PlayPause,
                        self.theme.label,
                    )?;
                }
                continue;
            }
            if let Some(action) = control_action(item.control) {
                self.draw_plate(frame, item.bounds, action, pressed)?;
            }
            let symbol = control_symbol(item.control);
            stock_art::draw_symbol(frame, item.bounds, symbol, self.theme.label)?;
        }
        Ok(())
    }

    /// A level slider: the plate filled from the left to the level, the icon at the left
    /// and the percentage at the right. Muted volume shows the mute icon and a dim fill; a
    /// keyboard backlight at 0 (off) shows a dim icon and percentage.
    fn draw_level_slider(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        bounds: Rectangle,
        kind: SliderKind,
        levels: StockLevels,
    ) -> Result<(), StockSliceError> {
        let level = match kind {
            SliderKind::Brightness => levels.display_brightness,
            SliderKind::Keyboard => levels.keyboard_backlight,
            SliderKind::Volume => levels.volume,
        };
        let muted = kind == SliderKind::Volume && levels.muted;
        let off = kind == SliderKind::Keyboard && level == Some(0);
        let radius = self.scaled_y(PLATE_RADIUS).max(1);
        self.surface(frame, bounds, self.theme.plate)?;
        if let Some(level) = level {
            let filled = bounds.width * u32::from(level.min(100)) / 100;
            let fill = if muted {
                self.theme.muted
            } else {
                self.theme.fill
            };
            let colour = self.theme.plate_colour(fill, bounds.x, self.width);
            stock_art::draw_plate_fill(frame, bounds, radius, filled, colour)?;
        }
        let icon = Rectangle {
            width: self.scaled_x(SLIDER_ICON_WIDTH).min(bounds.width),
            ..bounds
        };
        let symbol = match kind {
            SliderKind::Brightness => Symbol::DisplayUp,
            SliderKind::Keyboard => Symbol::KeyboardUp,
            SliderKind::Volume if muted => Symbol::Mute,
            SliderKind::Volume => Symbol::VolumeUp,
        };
        stock_art::draw_symbol(
            frame,
            icon,
            symbol,
            if off {
                self.theme.dim
            } else {
                self.theme.label
            },
        )?;
        if let Some(level) = level {
            let text = if muted || off {
                self.theme.dim
            } else {
                self.theme.label
            };
            self.draw_percent(frame, bounds, level, text)?;
        }
        Ok(())
    }

    /// "42%" right-aligned in `bounds`, if the glyphs fit (the native 60-high bar).
    fn draw_percent(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        bounds: Rectangle,
        level: u8,
        color: Rgb,
    ) -> Result<(), StockSliceError> {
        let glyphs: Vec<AlphaMask> = format!("{}%", level.min(100))
            .chars()
            .filter_map(|wanted| {
                value_glyphs::LARGE
                    .iter()
                    .find(|(char, _)| *char == wanted)
                    .map(|(_, mask)| *mask)
            })
            .collect();
        let width: u32 = glyphs.iter().map(|glyph| glyph.width).sum();
        let height = glyphs.first().map_or(0, |glyph| glyph.height);
        let margin = self.scaled_x(SLIDER_TEXT_MARGIN);
        if height * 2 > bounds.height || width + margin * 2 > bounds.width {
            return Ok(());
        }
        let mut x = bounds.x + bounds.width - margin - width;
        let y = bounds.y + (bounds.height - height) / 2;
        for glyph in glyphs {
            let place = Rectangle {
                x,
                y,
                width: glyph.width,
                height: glyph.height,
            };
            frame.blend_mask(place, glyph.pixels, color)?;
            x += glyph.width;
        }
        Ok(())
    }

    /// The level slider under a contact on the controls row, and what it hit there.
    #[must_use]
    pub fn slider_at(self, contact: DisplayContact) -> Option<(SliderKind, SliderSpot)> {
        let (x, y) = (
            display_coordinate(contact.x()),
            display_coordinate(contact.y()),
        );
        let item = self
            .normal_controls()
            .into_iter()
            .find(|item| self.control_available(item.control) && contains(item.bounds, x, y))?;
        let kind = control_slider(item.control)?;
        let on_icon = x < item.bounds.x + self.scaled_x(SLIDER_ICON_WIDTH);
        Some((
            kind,
            if matches!(kind, SliderKind::Volume | SliderKind::Keyboard) && on_icon {
                SliderSpot::Icon
            } else {
                SliderSpot::Level(self.slider_level_at(kind, contact.x()))
            },
        ))
    }

    /// The level (percent) at display `x` on slider `kind`, clamped to its plate.
    #[must_use]
    pub fn slider_level_at(self, kind: SliderKind, x: f64) -> u8 {
        let Some(bounds) = self
            .normal_controls()
            .into_iter()
            .find(|item| control_slider(item.control) == Some(kind))
            .map(|item| item.bounds)
        else {
            return 0;
        };
        let along = (x - f64::from(bounds.x)) / f64::from(bounds.width.max(1));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let level = (along * 100.0).round().clamp(0.0, 100.0) as u8;
        level
    }

    fn render_function_row(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        pressed: Option<StockAction>,
    ) -> Result<(), StockSliceError> {
        for slot in 0..FUNCTION_SLOT_COUNT {
            let bounds = self.function_slot(slot);
            if bounds.width == 0 {
                continue;
            }
            let action = function_action(slot).ok_or(StockSliceError::InvalidGeometry)?;
            self.draw_plate(frame, bounds, action, pressed)?;
            stock_art::draw_label(frame, bounds, function_label(action)?, self.theme.label)?;
        }
        Ok(())
    }

    fn render_extras_row(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        levels: StockLevels,
        pressed: Option<StockAction>,
    ) -> Result<(), StockSliceError> {
        self.draw_plate(frame, self.escape_bounds(), StockAction::Escape, pressed)?;
        stock_art::draw_label(
            frame,
            self.escape_bounds(),
            stock_labels::ESC,
            self.theme.label,
        )?;
        for (index, icon, bounds) in self.extra_controls() {
            if levels.extras_live & (1 << index) != 0 {
                continue;
            }
            let is_pressed = pressed == Some(StockAction::Extra(index));
            let on = levels.extras_on & (1 << index) != 0;
            let symbol = icon.symbol(on, levels.battery_bars);
            match (on, is_pressed) {
                (true, true) => self.lit_plate(frame, bounds, self.theme.lit_pressed)?,
                (true, false) => self.lit_plate(frame, bounds, self.theme.lit)?,
                (false, true) => self.surface(frame, bounds, self.theme.pressed)?,
                (false, false) => self.surface(frame, bounds, self.theme.plate)?,
            }
            stock_art::draw_symbol(frame, bounds, symbol, self.theme.label)?;
        }
        Ok(())
    }

    /// Finishes the live extras plates once their pictures are drawn: a lighter tint
    /// while pressed, rounded corners, and the "on" icon with a dark halo so it reads over
    /// any picture.
    ///
    /// # Errors
    ///
    /// Rejects an internally invalid draw.
    pub fn render_live_extras(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        levels: StockLevels,
        pressed: Option<StockAction>,
    ) -> Result<(), StockSliceError> {
        for (index, icon, bounds) in self.extra_controls() {
            if levels.extras_live & (1 << index) == 0 {
                continue;
            }
            if pressed == Some(StockAction::Extra(index)) {
                frame.blend_rectangle(bounds, LIVE_LABEL, 64)?;
            }
            let radius = self.scaled_y(PLATE_RADIUS).max(1);
            stock_art::draw_plate_corners(frame, bounds, radius, self.theme.background)?;
            let icon_bounds = if icon.is_meter() || icon == ExtraIcon::Vpn {
                Rectangle {
                    width: LIVE_ICON_WIDTH.min(bounds.width),
                    ..bounds
                }
            } else {
                bounds
            };
            stock_art::draw_symbol_with_halo(
                frame,
                icon_bounds,
                // Live toggles are always lit; the battery's lit look means on power.
                icon.symbol(levels.extras_on & (1 << index) != 0, levels.battery_bars),
                LIVE_LABEL,
                Some(LIVE_HALO),
            )?;
        }
        Ok(())
    }

    /// Where the play/pause button sits on the controls row, if it shows.
    #[must_use]
    pub fn play_pause_bounds(self) -> Option<Rectangle> {
        self.normal_controls()
            .into_iter()
            .find(|item| {
                item.control == Control::MediaPlayPause && self.control_visible(item.control)
            })
            .map(|item| item.bounds)
    }

    /// Finishes the live play/pause plate once its spectrum is drawn, as
    /// [`StockBar::render_live_extras`] does for the extras.
    ///
    /// # Errors
    ///
    /// Rejects an internally invalid draw.
    pub fn render_live_media(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        pressed: Option<StockAction>,
    ) -> Result<(), StockSliceError> {
        let Some(bounds) = self.play_pause_bounds() else {
            return Ok(());
        };
        if pressed == Some(StockAction::MediaPlayPause) {
            frame.blend_rectangle(bounds, LIVE_LABEL, 64)?;
        }
        let radius = self.scaled_y(PLATE_RADIUS).max(1);
        stock_art::draw_plate_corners(frame, bounds, radius, self.theme.background)?;
        stock_art::draw_symbol_with_halo(
            frame,
            bounds,
            Symbol::PlayPause,
            LIVE_LABEL,
            Some(LIVE_HALO),
        )?;
        Ok(())
    }

    /// Where extras button `index` sits, if configured.
    #[must_use]
    pub fn extra_bounds(self, index: u8) -> Option<Rectangle> {
        self.extra_controls()
            .into_iter()
            .find(|(i, _, _)| *i == index)
            .map(|(_, _, bounds)| bounds)
    }

    /// Toggles fill from the left after Esc, meters from the right end, each in config
    /// order.
    fn extra_controls(self) -> Vec<(u8, ExtraIcon, Rectangle)> {
        let meters = self.extras.iter().flatten().filter(|icon| icon.is_meter());
        let mut meter_x = ACTIVE_RIGHT - u32::try_from(meters.count()).unwrap_or(0) * EXTRA_PITCH
            + (EXTRA_PITCH - EXTRA_WIDTH);
        let mut toggle_x = EXTRA_X;
        (0_u8..)
            .zip(self.extras)
            .filter_map(|(index, icon)| {
                let icon = icon?;
                let slot = if icon.is_meter() {
                    &mut meter_x
                } else {
                    &mut toggle_x
                };
                let x = *slot;
                *slot += EXTRA_PITCH;
                Some((
                    index,
                    icon,
                    Rectangle {
                        x: self.scaled_x(x),
                        y: self.scaled_y(PLATE_Y),
                        width: self.scaled_x(EXTRA_WIDTH).max(1),
                        height: self.scaled_y(PLATE_HEIGHT).max(1),
                    },
                ))
            })
            .collect()
    }

    fn draw_plate(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        bounds: Rectangle,
        action: StockAction,
        pressed: Option<StockAction>,
    ) -> Result<(), StockSliceError> {
        self.surface(
            frame,
            bounds,
            if pressed == Some(action) {
                self.theme.pressed
            } else {
                self.theme.plate
            },
        )
    }

    /// A plate in a theme surface colour (`base`), in the button's hue for pastel and
    /// rainbow.
    fn surface(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        bounds: Rectangle,
        base: Rgb,
    ) -> Result<(), StockSliceError> {
        let radius = self.scaled_y(PLATE_RADIUS).max(1);
        let colour = self.theme.plate_colour(base, bounds.x, self.width);
        stock_art::draw_rounded_plate(frame, bounds, radius, colour)?;
        Ok(())
    }

    /// A lit plate: always the plain colour, so "on" reads the same in every theme.
    fn lit_plate(
        self,
        frame: &mut Xrgb8888Frame<'_>,
        bounds: Rectangle,
        colour: Rgb,
    ) -> Result<(), StockSliceError> {
        let radius = self.scaled_y(PLATE_RADIUS).max(1);
        stock_art::draw_rounded_plate(frame, bounds, radius, colour)?;
        Ok(())
    }

    fn function_hit_test(self, x: u32, y: u32) -> Option<StockAction> {
        if x >= self.active_right() || y >= self.height {
            return None;
        }
        let slot = u32::try_from(
            u64::from(x) * u64::from(FUNCTION_SLOT_COUNT) / u64::from(self.active_right()),
        )
        .ok()?;
        let bounds = self.function_slot(slot);
        contains(bounds, x, y)
            .then(|| function_action(slot))
            .flatten()
    }

    fn escape_bounds(self) -> Rectangle {
        self.function_slot(0)
    }

    fn function_slot(self, slot: u32) -> Rectangle {
        let active_right = self.active_right();
        let left = ceiling_partition(slot, active_right, FUNCTION_SLOT_COUNT);
        let right = ceiling_partition(slot + 1, active_right, FUNCTION_SLOT_COUNT);
        let inset = self.scaled_x(5).min((right - left) / 2);
        let left_inset = if slot == 0 { 0 } else { inset };
        Rectangle {
            x: left + left_inset,
            y: self.scaled_y(PLATE_Y),
            width: (right - left).saturating_sub(left_inset + inset),
            height: self.scaled_y(PLATE_HEIGHT).max(1),
        }
    }

    fn normal_controls(self) -> Vec<LaidOutControl> {
        NORMAL_CONTROLS
            .into_iter()
            .map(|item| LaidOutControl {
                control: item.control,
                bounds: Rectangle {
                    x: self.scaled_x(item.x),
                    y: self.scaled_y(PLATE_Y),
                    width: self.scaled_x(item.width).max(1),
                    height: self.scaled_y(PLATE_HEIGHT).max(1),
                },
            })
            .collect()
    }

    const fn control_available(self, control: Control) -> bool {
        match control {
            Control::DisplayBrightness => self
                .capabilities
                .contains(StockCapabilities::DISPLAY_BRIGHTNESS),
            Control::KeyboardBacklight => self
                .capabilities
                .contains(StockCapabilities::KEYBOARD_BACKLIGHT),
            Control::MediaPrevious | Control::MediaPlayPause | Control::MediaNext => {
                self.capabilities.contains(StockCapabilities::MEDIA)
            }
            Control::Volume => self.capabilities.contains(StockCapabilities::AUDIO),
        }
    }

    const fn control_visible(self, control: Control) -> bool {
        matches!(
            control,
            Control::MediaPrevious | Control::MediaPlayPause | Control::MediaNext
        ) || self.control_available(control)
    }

    fn active_right(self) -> u32 {
        scale(ACTIVE_RIGHT, self.width, NATIVE_WIDTH).max(1)
    }

    fn scaled_x(self, value: u32) -> u32 {
        scale(value, self.width, NATIVE_WIDTH)
    }

    fn scaled_y(self, value: u32) -> u32 {
        scale(value, self.height, NATIVE_HEIGHT)
    }
}

fn scale(value: u32, extent: u32, native_extent: u32) -> u32 {
    u32::try_from(
        (u64::from(value) * u64::from(extent) + u64::from(native_extent) / 2)
            / u64::from(native_extent),
    )
    .unwrap_or(extent)
}

fn ceiling_partition(index: u32, extent: u32, count: u32) -> u32 {
    let numerator = u64::from(index) * u64::from(extent);
    u32::try_from(numerator.div_ceil(u64::from(count))).unwrap_or(extent)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn display_coordinate(value: f64) -> u32 {
    value as u32
}

fn contains(bounds: Rectangle, x: u32, y: u32) -> bool {
    x >= bounds.x
        && y >= bounds.y
        && x < bounds.x.saturating_add(bounds.width)
        && y < bounds.y.saturating_add(bounds.height)
}

/// A button's action; sliders have none (the input interpreter drives them).
const fn control_action(control: Control) -> Option<StockAction> {
    Some(match control {
        Control::MediaPrevious => StockAction::MediaPrevious,
        Control::MediaPlayPause => StockAction::MediaPlayPause,
        Control::MediaNext => StockAction::MediaNext,
        Control::DisplayBrightness | Control::KeyboardBacklight | Control::Volume => return None,
    })
}

const fn control_slider(control: Control) -> Option<SliderKind> {
    Some(match control {
        Control::DisplayBrightness => SliderKind::Brightness,
        Control::KeyboardBacklight => SliderKind::Keyboard,
        Control::Volume => SliderKind::Volume,
        Control::MediaPrevious | Control::MediaPlayPause | Control::MediaNext => return None,
    })
}

const fn control_symbol(control: Control) -> Symbol {
    match control {
        Control::DisplayBrightness => Symbol::DisplayUp,
        Control::KeyboardBacklight => Symbol::KeyboardUp,
        Control::MediaPrevious => Symbol::Previous,
        Control::MediaPlayPause => Symbol::PlayPause,
        Control::MediaNext => Symbol::Next,
        Control::Volume => Symbol::VolumeUp,
    }
}

const fn function_action(slot: u32) -> Option<StockAction> {
    Some(match slot {
        0 => StockAction::Escape,
        1 => StockAction::Function(FunctionKey::F1),
        2 => StockAction::Function(FunctionKey::F2),
        3 => StockAction::Function(FunctionKey::F3),
        4 => StockAction::Function(FunctionKey::F4),
        5 => StockAction::Function(FunctionKey::F5),
        6 => StockAction::Function(FunctionKey::F6),
        7 => StockAction::Function(FunctionKey::F7),
        8 => StockAction::Function(FunctionKey::F8),
        9 => StockAction::Function(FunctionKey::F9),
        10 => StockAction::Function(FunctionKey::F10),
        11 => StockAction::Function(FunctionKey::F11),
        12 => StockAction::Function(FunctionKey::F12),
        _ => return None,
    })
}

fn function_label(action: StockAction) -> Result<AlphaMask, StockSliceError> {
    Ok(match action {
        StockAction::Escape => stock_labels::ESC,
        StockAction::Function(FunctionKey::F1) => stock_labels::F1,
        StockAction::Function(FunctionKey::F2) => stock_labels::F2,
        StockAction::Function(FunctionKey::F3) => stock_labels::F3,
        StockAction::Function(FunctionKey::F4) => stock_labels::F4,
        StockAction::Function(FunctionKey::F5) => stock_labels::F5,
        StockAction::Function(FunctionKey::F6) => stock_labels::F6,
        StockAction::Function(FunctionKey::F7) => stock_labels::F7,
        StockAction::Function(FunctionKey::F8) => stock_labels::F8,
        StockAction::Function(FunctionKey::F9) => stock_labels::F9,
        StockAction::Function(FunctionKey::F10) => stock_labels::F10,
        StockAction::Function(FunctionKey::F11) => stock_labels::F11,
        StockAction::Function(FunctionKey::F12) => stock_labels::F12,
        _ => return Err(StockSliceError::InvalidGeometry),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> StockCapabilities {
        StockCapabilities::DISPLAY_BRIGHTNESS
            | StockCapabilities::KEYBOARD_BACKLIGHT
            | StockCapabilities::AUDIO
            | StockCapabilities::MEDIA
    }

    fn tap(x: f64, y: f64) -> Gesture {
        Gesture::Tap(DisplayContact::new(1, x, y).unwrap())
    }

    #[test]
    fn fn_row_is_escape_plus_twelve_uniform_slots_across_the_display() {
        let bar = StockBar::new(NATIVE_WIDTH, NATIVE_HEIGHT, all()).unwrap();
        let expected = [
            StockAction::Escape,
            StockAction::Function(FunctionKey::F1),
            StockAction::Function(FunctionKey::F2),
            StockAction::Function(FunctionKey::F3),
            StockAction::Function(FunctionKey::F4),
            StockAction::Function(FunctionKey::F5),
            StockAction::Function(FunctionKey::F6),
            StockAction::Function(FunctionKey::F7),
            StockAction::Function(FunctionKey::F8),
            StockAction::Function(FunctionKey::F9),
            StockAction::Function(FunctionKey::F10),
            StockAction::Function(FunctionKey::F11),
            StockAction::Function(FunctionKey::F12),
        ];
        for (slot, action) in expected.into_iter().enumerate() {
            let bounds = bar.function_slot(u32::try_from(slot).unwrap());
            assert_eq!(
                bar.action_for_gesture(
                    tap(f64::from(bounds.x + bounds.width / 2), 30.0),
                    Layer::Function,
                ),
                Some(action),
            );
        }
        assert_eq!(
            bar.action_for_gesture(tap(2_080.0, 30.0), Layer::Function),
            Some(StockAction::Function(FunctionKey::F12))
        );
    }

    #[test]
    fn normal_row_uses_fixed_grouped_controls() {
        let bar = StockBar::new(NATIVE_WIDTH, NATIVE_HEIGHT, all()).unwrap();
        assert_eq!(bar.escape_bounds().x, 0);
        let items = bar.normal_controls();
        assert_eq!(items.len(), 6);
        assert_eq!(items[0].bounds.x, 194);
        assert_eq!(items[5].bounds.x + items[5].bounds.width, NATIVE_WIDTH);
        for item in items {
            let action = bar.action_at(
                DisplayContact::new(1, f64::from(item.bounds.x + item.bounds.width / 2), 30.0)
                    .unwrap(),
                Layer::Controls,
            );
            assert_eq!(action, control_action(item.control));
        }
    }

    #[test]
    fn media_capability_loss_keeps_fixed_chrome_but_disables_actions() {
        let full = StockBar::new(NATIVE_WIDTH, NATIVE_HEIGHT, all()).unwrap();
        let reduced = StockBar::new(
            NATIVE_WIDTH,
            NATIVE_HEIGHT,
            StockCapabilities::DISPLAY_BRIGHTNESS | StockCapabilities::KEYBOARD_BACKLIGHT,
        )
        .unwrap();
        assert_eq!(full.normal_controls(), reduced.normal_controls());
        assert!(reduced.control_visible(Control::MediaPrevious));
        assert!(!reduced.control_available(Control::MediaPrevious));
        let media = full.normal_controls()[2].bounds;
        let contact = DisplayContact::new(
            1,
            f64::from(media.x + media.width / 2),
            f64::from(media.y + media.height / 2),
        )
        .unwrap();
        assert_eq!(
            full.action_at(contact, Layer::Controls),
            Some(StockAction::MediaPrevious)
        );
        assert_eq!(reduced.action_at(contact, Layer::Controls), None);
    }

    #[test]
    fn rendering_is_antialiased_and_pressed_state_is_distinct() {
        let bar = StockBar::new(NATIVE_WIDTH, NATIVE_HEIGHT, all()).unwrap();
        let length = usize::try_from(NATIVE_WIDTH * NATIVE_HEIGHT * 4).unwrap();
        let mut normal = vec![0_u8; length];
        let mut pressed = vec![0_u8; length];
        Xrgb8888Frame::new(NATIVE_WIDTH, NATIVE_HEIGHT, NATIVE_WIDTH * 4, &mut normal)
            .and_then(|mut frame| {
                bar.render(&mut frame, false, StockLevels::default())
                    .map_err(|error| match error {
                        StockSliceError::Drawing(error) => error,
                        _ => FramebufferError::InvalidGeometry,
                    })
            })
            .unwrap();
        Xrgb8888Frame::new(NATIVE_WIDTH, NATIVE_HEIGHT, NATIVE_WIDTH * 4, &mut pressed)
            .and_then(|mut frame| {
                bar.render_pressed(
                    &mut frame,
                    Layer::Controls,
                    StockLevels::default(),
                    Some(StockAction::Escape),
                )
                .map_err(|error| match error {
                    StockSliceError::Drawing(error) => error,
                    _ => FramebufferError::InvalidGeometry,
                })
            })
            .unwrap();
        assert_ne!(normal, pressed);
        assert!(
            normal
                .iter()
                .any(|channel| !matches!(*channel, 0 | 53 | 242))
        );
    }

    #[test]
    fn extras_layer_hits_extras_and_nothing_else() {
        let bar = StockBar::new(NATIVE_WIDTH, NATIVE_HEIGHT, all())
            .unwrap()
            .with_extras(&[ExtraIcon::Camera, ExtraIcon::Mic, ExtraIcon::Vpn]);
        let resting = Layer::from_fn(false, false, false);
        assert_ne!(resting, Layer::Extras);
        assert_eq!(Layer::from_fn(false, true, false), resting);
        assert_ne!(Layer::from_fn(true, false, false), resting);
        assert_ne!(Layer::from_fn(true, false, false), Layer::Extras);
        assert_eq!(Layer::from_fn(true, true, false), Layer::Extras);
        assert_eq!(Layer::from_fn(false, false, false), Layer::Controls);
        assert_eq!(Layer::from_fn(true, false, false), Layer::Function);
        assert_eq!(Layer::from_fn(false, false, true), Layer::Function);
        assert_eq!(Layer::from_fn(true, false, true), Layer::Controls);
        assert_eq!(bar.extra_controls().len(), 3);
        for (index, _, bounds) in bar.extra_controls() {
            let center = DisplayContact::new(
                1,
                f64::from(bounds.x + bounds.width / 2),
                f64::from(bounds.y + bounds.height / 2),
            )
            .unwrap();
            assert_eq!(
                bar.action_at(center, Layer::Extras),
                Some(StockAction::Extra(index))
            );
        }
        let escape = DisplayContact::new(1, 40.0, 30.0).unwrap();
        assert_eq!(
            bar.action_at(escape, Layer::Extras),
            Some(StockAction::Escape)
        );
        let far_right = DisplayContact::new(1, 2_100.0, 30.0).unwrap();
        assert_eq!(bar.action_at(far_right, Layer::Extras), None);
    }

    #[test]
    fn toggles_sit_left_and_meters_right() {
        let bar = StockBar::new(NATIVE_WIDTH, NATIVE_HEIGHT, all())
            .unwrap()
            .with_extras(&[
                ExtraIcon::Camera,
                ExtraIcon::Cpu,
                ExtraIcon::Mic,
                ExtraIcon::Network,
            ]);
        let lefts: Vec<(u8, u32)> = bar
            .extra_controls()
            .into_iter()
            .map(|(index, _, bounds)| (index, bounds.x))
            .collect();
        assert_eq!(lefts, [(0, 194), (1, 1_798), (2, 384), (3, 1_988)]);
    }

    #[test]
    fn play_pause_is_green_while_playing_and_left_black_for_the_spectrum() {
        let bar = StockBar::new(NATIVE_WIDTH, NATIVE_HEIGHT, all()).unwrap();
        let bounds = bar.play_pause_bounds().unwrap();
        assert_eq!(bounds.x, 1_200);
        let edge = |levels: StockLevels| {
            let mut pixels = vec![0_u8; usize::try_from(NATIVE_WIDTH * NATIVE_HEIGHT * 4).unwrap()];
            let mut frame =
                Xrgb8888Frame::new(NATIVE_WIDTH, NATIVE_HEIGHT, NATIVE_WIDTH * 4, &mut pixels)
                    .unwrap();
            bar.render_pressed(&mut frame, Layer::Controls, levels, None)
                .unwrap();
            let offset =
                usize::try_from(((bounds.y + bounds.height / 2) * NATIVE_WIDTH + bounds.x + 6) * 4)
                    .unwrap();
            [pixels[offset], pixels[offset + 1], pixels[offset + 2]]
        };
        let idle = edge(StockLevels::default());
        let playing = edge(StockLevels {
            playing: true,
            ..StockLevels::default()
        });
        let live = edge(StockLevels {
            playing: true,
            media_live: true,
            ..StockLevels::default()
        });
        let (lit, black) = (Theme::DEFAULT.lit, Theme::DEFAULT.background);
        assert_eq!(playing, [lit.blue, lit.green, lit.red]);
        assert_ne!(idle, playing);
        assert_eq!(live, [black.blue, black.green, black.red]);
    }

    #[test]
    fn live_extras_plate_is_left_for_the_picture_then_gets_corners_and_icon() {
        let bar = StockBar::new(NATIVE_WIDTH, NATIVE_HEIGHT, all())
            .unwrap()
            .with_extras(&[ExtraIcon::Camera, ExtraIcon::Mic]);
        let levels = StockLevels {
            extras_on: 0b11,
            extras_live: 0b01,
            ..StockLevels::default()
        };
        let bounds = bar.extra_bounds(0).unwrap();
        let mut pixels = vec![0_u8; usize::try_from(NATIVE_WIDTH * NATIVE_HEIGHT * 4).unwrap()];
        let pixel = |pixels: &[u8], x: u32, y: u32| {
            let offset = usize::try_from((y * NATIVE_WIDTH + x) * 4).unwrap();
            [pixels[offset], pixels[offset + 1], pixels[offset + 2]]
        };
        let centre = (bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
        let edge = (bounds.x + 4, bounds.y + bounds.height / 2);
        {
            let mut frame =
                Xrgb8888Frame::new(NATIVE_WIDTH, NATIVE_HEIGHT, NATIVE_WIDTH * 4, &mut pixels)
                    .unwrap();
            bar.render_pressed(&mut frame, Layer::Extras, levels, None)
                .unwrap();
            assert_eq!(pixel(frame.as_bytes(), centre.0, centre.1), [0, 0, 0]);
            // The mic plate (not live) is drawn as usual: lit green.
            let mic = bar.extra_bounds(1).unwrap();
            assert_eq!(
                pixel(frame.as_bytes(), mic.x + 4, mic.y + 30),
                [64, 128, 32]
            );
            frame
                .fill_rectangle(
                    bounds,
                    Rgb {
                        red: 200,
                        green: 0,
                        blue: 0,
                    },
                )
                .unwrap();
            bar.render_live_extras(&mut frame, levels, None).unwrap();
        }
        assert_eq!(
            pixel(&pixels, bounds.x, bounds.y),
            [0, 0, 0],
            "rounded corner"
        );
        assert_eq!(pixel(&pixels, edge.0, edge.1), [0, 0, 200], "picture shows");
        assert!(
            (bounds.x..bounds.x + bounds.width)
                .any(|x| pixel(&pixels, x, centre.1) == [242, 242, 242]),
            "icon drawn over the picture"
        );
    }

    #[test]
    fn extras_state_changes_the_frame() {
        let bar = StockBar::new(NATIVE_WIDTH, NATIVE_HEIGHT, all())
            .unwrap()
            .with_extras(&[ExtraIcon::Camera, ExtraIcon::Mic, ExtraIcon::Vpn]);
        let draw = |levels: StockLevels| {
            let mut pixels = vec![0_u8; usize::try_from(NATIVE_WIDTH * NATIVE_HEIGHT * 4).unwrap()];
            let mut frame =
                Xrgb8888Frame::new(NATIVE_WIDTH, NATIVE_HEIGHT, NATIVE_WIDTH * 4, &mut pixels)
                    .unwrap();
            bar.render_pressed(&mut frame, Layer::Extras, levels, None)
                .unwrap();
            pixels
        };
        let frames = (0..4)
            .map(|bit| {
                draw(StockLevels {
                    extras_on: (1 << bit) >> 1,
                    ..StockLevels::default()
                })
            })
            .collect::<Vec<_>>();
        for (index, frame) in frames.iter().enumerate() {
            for other in &frames[index + 1..] {
                assert_ne!(frame, other);
            }
        }
    }

    #[test]
    fn geometry_mismatch_is_atomic() {
        let bar = StockBar::new(10, 2, StockCapabilities::default()).unwrap();
        let mut pixels = [0x5a; 72];
        let before = pixels;
        let mut frame = Xrgb8888Frame::new(9, 2, 36, &mut pixels).unwrap();
        assert_eq!(
            bar.render(&mut frame, false, StockLevels::default()),
            Err(StockSliceError::FrameGeometryMismatch),
        );
        assert_eq!(pixels, before);
    }
}
