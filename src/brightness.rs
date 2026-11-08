//! Display and keyboard backlight levels for the controls row's sliders: read from sysfs,
//! set with `brightnessctl` (it goes through logind, so no root is needed; the hardware
//! service can only step them). Settings are coalesced on a worker thread: a drag sends
//! many levels, and only the latest one still waiting is applied.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Light {
    /// The first `/sys/class/backlight` device (e.g. `gmux_backlight`).
    Display,
    /// The first `/sys/class/leds/*kbd_backlight` device (e.g. `spi::kbd_backlight`).
    Keyboard,
}

impl Light {
    fn device(self) -> Option<PathBuf> {
        let (class, wanted): (&str, fn(&str) -> bool) = match self {
            Self::Display => ("/sys/class/backlight", |_| true),
            Self::Keyboard => ("/sys/class/leds", |name| name.ends_with("kbd_backlight")),
        };
        let mut devices: Vec<PathBuf> = fs::read_dir(class)
            .ok()?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(wanted)
            })
            .collect();
        devices.sort();
        devices.into_iter().next()
    }

    /// Lowest level a slider sets: the display never goes fully dark.
    const fn minimum(self) -> u8 {
        match self {
            Self::Display => 1,
            Self::Keyboard => 0,
        }
    }
}

/// Current level in percent of the maximum.
#[must_use]
pub fn read(light: Light) -> Option<u8> {
    let device = light.device()?;
    let value = |name: &str| -> Option<u64> {
        fs::read_to_string(device.join(name))
            .ok()?
            .trim()
            .parse()
            .ok()
    };
    let (current, maximum) = (value("brightness")?, value("max_brightness")?);
    if maximum == 0 {
        return None;
    }
    u8::try_from((current * 100 + maximum / 2) / maximum).ok()
}

/// Sets one light's level on a worker thread, latest first.
pub struct Setter {
    levels: Sender<u8>,
}

impl Setter {
    #[must_use]
    pub fn start(light: Light) -> Self {
        let (levels, receiver) = mpsc::channel::<u8>();
        let device = light
            .device()
            .and_then(|path| path.file_name()?.to_str().map(str::to_owned));
        thread::spawn(move || {
            while let Ok(mut level) = receiver.recv() {
                while let Ok(newer) = receiver.try_recv() {
                    level = newer;
                }
                let Some(device) = &device else {
                    continue;
                };
                let level = level.clamp(light.minimum(), 100);
                let started = std::time::Instant::now();
                let result = Command::new("brightnessctl")
                    .args([
                        "--quiet",
                        &format!("--device={device}"),
                        "set",
                        &format!("{level}%"),
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .status();
                if crate::record::active() {
                    crate::record::event(&format!(
                        "brightnessctl {device} {level}% took {:?}",
                        started.elapsed()
                    ));
                }
                if !result.as_ref().is_ok_and(std::process::ExitStatus::success) {
                    eprintln!(
                        "fnstrip: brightnessctl could not set {device} to {level}%: {result:?}"
                    );
                }
            }
        });
        Self { levels }
    }

    pub fn set(&self, percent: u8) {
        let _ = self.levels.send(percent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_percentages_where_a_light_exists() {
        for light in [Light::Display, Light::Keyboard] {
            assert!(read(light).is_none_or(|level| level <= 100));
        }
    }
}
