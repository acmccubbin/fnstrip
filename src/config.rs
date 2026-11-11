//! User configuration: `$XDG_CONFIG_HOME/fnstrip/config.toml` (else `~/.config/...`).
//!
//! The format is a strict TOML subset, parsed here so the crate stays std-only:
//! `# comments`, `[[extras]]` table-array headers, a `[colors]` table, and `key = "basic"` or `key = 'literal'`
//! string values (basic strings take `\"`, `\\`, `\n`, `\t`). Anything else is an error
//! with a line number. A missing or broken file falls back to the built-in default
//! (`config/default.toml`), so a typo never costs the bar.

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::stock_slice::{ExtraIcon, MAX_EXTRAS};
use crate::theme::Theme;

const DEFAULT_CONFIG: &str = include_str!("../config/default.toml");

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools, reason = "independent on/off settings")]
pub struct Config {
    /// Esc + F1–F12 show with Fn up, instead of the controls (`default_row = "function"`).
    pub function_row_first: bool,
    /// The light show when the renderer starts (`startup_animation = "on"`, the default).
    pub startup_animation: bool,
    /// Buttons of the extras row (double-tap Fn), left to right.
    pub extras: Vec<ExtraConfig>,
    /// Colours: `theme = "mono"`, then any `[colors]` overrides.
    pub theme: Theme,
    /// The temperature meter in °F (`temperature_unit = "f"`); °C by default.
    pub fahrenheit: bool,
    /// Fn triple-tap-and-hold for the test screen and quad-tap for the next theme
    /// (`debug_gestures = "on"`); off by default.
    pub debug_gestures: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtraConfig {
    pub icon: ExtraIcon,
    /// Run with `sh -c` on tap. Empty (meters may leave it out): tapping does nothing.
    pub command: String,
    /// Run with `sh -c` to light the button: exit status 0 means on. None: never lit.
    pub status: Option<String>,
    /// Network interface a `net` or `vpn` button graphs (`vpn`: default `wg0`; `net`:
    /// default every interface of a device that is up).
    pub interface: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ConfigError {}

impl Default for Config {
    fn default() -> Self {
        Self {
            function_row_first: false,
            startup_animation: true,
            extras: Vec::new(),
            theme: Theme::DEFAULT,
            fahrenheit: false,
            debug_gestures: false,
        }
    }
}

impl Config {
    /// The user's config, or the built-in default if it's missing or invalid (logged).
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default_config();
        };
        match fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|error| {
                eprintln!(
                    "fnstrip: {}: {error}; using the built-in default",
                    path.display()
                );
                Self::default_config()
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default_config(),
            Err(error) => {
                eprintln!(
                    "fnstrip: {}: {error}; using the built-in default",
                    path.display()
                );
                Self::default_config()
            }
        }
    }

    /// # Panics
    ///
    /// Only if the embedded default is invalid, which a unit test rules out.
    #[must_use]
    pub fn default_config() -> Self {
        Self::parse(DEFAULT_CONFIG).expect("built-in default config parses")
    }

    /// # Errors
    ///
    /// The first syntax or schema problem, with its line number.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        let mut current: Option<(usize, PartialExtra)> = None;
        // Inside `[colors]`; its entries apply over the named theme once it is known.
        let mut in_colors = false;
        let mut colors: Vec<(usize, String, String)> = Vec::new();
        let mut theme_name: Option<String> = None;
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let error = |message: &str| ConfigError {
                line,
                message: message.to_owned(),
            };
            let content = strip_comment(raw).trim();
            if content.is_empty() {
                continue;
            }
            if content.starts_with('[') {
                if let Some((start, extra)) = current.take() {
                    config.push_extra(start, extra)?;
                }
                in_colors = content == "[colors]";
                if in_colors {
                    continue;
                }
                if content != "[[extras]]" {
                    return Err(error("unknown section (only [colors] and [[extras]])"));
                }
                current = Some((line, PartialExtra::default()));
                continue;
            }
            let (key, value) = content
                .split_once('=')
                .ok_or_else(|| error("expected key = \"value\""))?;
            let key = key.trim();
            let value = parse_string(value.trim()).map_err(|message| error(&message))?;
            if in_colors {
                colors.push((line, key.to_owned(), value));
                continue;
            }
            let Some((_, extra)) = current.as_mut() else {
                config
                    .set_top_level(key, value, &mut theme_name)
                    .map_err(|message| error(&message))?;
                continue;
            };
            let slot = match key {
                "icon" => &mut extra.icon,
                "command" => &mut extra.command,
                "status" => &mut extra.status,
                "interface" => &mut extra.interface,
                _ => return Err(error(&format!("unknown key `{key}`"))),
            };
            if slot.replace(value).is_some() {
                return Err(error(&format!("`{key}` given twice")));
            }
        }
        if let Some((start, extra)) = current {
            config.push_extra(start, extra)?;
        }
        if let Some(name) = theme_name {
            config.theme = Theme::named(&name).unwrap_or_default();
        }
        for (line, key, value) in colors {
            config
                .theme
                .set(&key, &value)
                .map_err(|message| ConfigError { line, message })?;
        }
        Ok(config)
    }

    /// A setting above the first `[[extras]]` (the theme is applied after the whole file,
    /// under any `[colors]`).
    fn set_top_level(
        &mut self,
        key: &str,
        value: String,
        theme_name: &mut Option<String>,
    ) -> Result<(), String> {
        match key {
            "default_row" => {
                self.function_row_first = match value.as_str() {
                    "controls" => false,
                    "function" => true,
                    _ => return Err("default_row must be \"controls\" or \"function\"".into()),
                };
            }
            "startup_animation" => {
                self.startup_animation = match value.as_str() {
                    "on" => true,
                    "off" => false,
                    _ => return Err("startup_animation must be \"on\" or \"off\"".into()),
                };
            }
            "debug_gestures" => {
                self.debug_gestures = match value.as_str() {
                    "on" => true,
                    "off" => false,
                    _ => return Err("debug_gestures must be \"on\" or \"off\"".into()),
                };
            }
            "temperature_unit" => {
                self.fahrenheit = match value.to_ascii_lowercase().as_str() {
                    "c" | "celsius" => false,
                    "f" | "fahrenheit" => true,
                    _ => return Err("temperature_unit must be \"c\" or \"f\"".into()),
                };
            }
            "theme" => {
                if Theme::named(&value).is_none() {
                    return Err(format!(
                        "unknown theme `{value}` (known: {})",
                        Theme::NAMES.join(", ")
                    ));
                }
                *theme_name = Some(value);
            }
            _ => return Err("key outside an [[extras]] entry".into()),
        }
        Ok(())
    }

    fn push_extra(&mut self, line: usize, extra: PartialExtra) -> Result<(), ConfigError> {
        let error = |message: String| ConfigError { line, message };
        if self.extras.len() == MAX_EXTRAS {
            return Err(error(format!(
                "at most {MAX_EXTRAS} [[extras]] fit the bar"
            )));
        }
        let icon_name = extra
            .icon
            .ok_or_else(|| error("[[extras]] needs an icon".to_owned()))?;
        let icon = ExtraIcon::from_name(&icon_name).ok_or_else(|| {
            error(format!(
                "unknown icon `{icon_name}` (known: {})",
                ExtraIcon::NAMES.join(", ")
            ))
        })?;
        let command = match extra.command {
            Some(command) => command,
            None if icon.is_meter() => String::new(),
            None => return Err(error("[[extras]] needs a command".to_owned())),
        };
        self.extras.push(ExtraConfig {
            icon,
            command,
            status: extra.status,
            interface: extra.interface,
        });
        Ok(())
    }
}

#[derive(Default)]
struct PartialExtra {
    icon: Option<String>,
    command: Option<String>,
    status: Option<String>,
    interface: Option<String>,
}

fn config_path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("fnstrip/config.toml"))
}

/// Drops a `#` comment that isn't inside a string.
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        match (quote, character) {
            (Some('"'), '\\') if !escaped => {
                escaped = true;
                continue;
            }
            (Some(open), close) if close == open && !escaped => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '#') => return &line[..index],
            _ => {}
        }
        escaped = false;
    }
    line
}

fn parse_string(value: &str) -> Result<String, String> {
    let mut characters = value.chars();
    let quote = characters.next();
    if !matches!(quote, Some('"' | '\'')) {
        return Err("values must be quoted strings".to_owned());
    }
    let mut output = String::new();
    while let Some(character) = characters.next() {
        if Some(character) == quote {
            return if characters.as_str().trim().is_empty() {
                Ok(output)
            } else {
                Err("unexpected text after the closing quote".to_owned())
            };
        }
        if character == '\\' && quote == Some('"') {
            output.push(match characters.next() {
                Some('"') => '"',
                Some('\\') => '\\',
                Some('n') => '\n',
                Some('t') => '\t',
                _ => return Err("unsupported escape (use \\\" \\\\ \\n \\t)".to_owned()),
            });
        } else {
            output.push(character);
        }
    }
    Err("unterminated string".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_default_parses() {
        let config = Config::default_config();
        assert_eq!(config.extras.len(), 6);
        assert!(!config.function_row_first);
    }

    #[test]
    fn parses_entries_comments_and_both_string_kinds() {
        let config = Config::parse(
            r##"
            # extras row
            [[extras]]
            icon = "mic"   # trailing comment
            command = 'echo "# not a comment"'
            status = "test \"$X\" = 1"

            [[extras]]
            icon = 'camera'
            command = "webcam toggle"
            "##,
        )
        .unwrap();
        assert_eq!(
            config.extras,
            [
                ExtraConfig {
                    icon: ExtraIcon::Mic,
                    command: r##"echo "# not a comment""##.to_owned(),
                    status: Some(r#"test "$X" = 1"#.to_owned()),
                    interface: None,
                },
                ExtraConfig {
                    icon: ExtraIcon::Camera,
                    command: "webcam toggle".to_owned(),
                    status: None,
                    interface: None,
                },
            ]
        );
    }

    #[test]
    fn meters_need_no_command_and_take_an_interface() {
        let config = Config::parse(
            "[[extras]]
icon = \"cpu\"
[[extras]]
icon = \"net\"
interface = 'wlan0'",
        )
        .unwrap();
        assert_eq!(config.extras[0].icon, ExtraIcon::Cpu);
        assert_eq!(config.extras[0].command, "");
        assert_eq!(config.extras[1].interface.as_deref(), Some("wlan0"));
    }

    #[test]
    fn debug_gestures_are_off_unless_turned_on() {
        assert!(!Config::parse("").unwrap().debug_gestures);
        assert!(
            Config::parse("debug_gestures = \"on\"")
                .unwrap()
                .debug_gestures
        );
        assert!(Config::parse("debug_gestures = \"yes\"").is_err());
    }

    #[test]
    fn temperature_is_celsius_unless_set_to_fahrenheit() {
        assert!(!Config::parse("").unwrap().fahrenheit);
        assert!(
            Config::parse("temperature_unit = \"f\"")
                .unwrap()
                .fahrenheit
        );
        assert!(
            Config::parse("temperature_unit = \"Fahrenheit\"")
                .unwrap()
                .fahrenheit
        );
        assert!(
            !Config::parse("temperature_unit = \"c\"")
                .unwrap()
                .fahrenheit
        );
        assert!(Config::parse("temperature_unit = \"k\"").is_err());
    }

    #[test]
    fn theme_and_colors_apply_in_that_order() {
        let config = Config::parse(
            "theme = \"mono\"\n[colors]\nlabel = \"#fffff0\"\n\
             [[extras]]\nicon = \"cpu\"\n",
        )
        .unwrap();
        let mono = Theme::named("mono").unwrap();
        assert_eq!(config.theme.plate, mono.plate);
        assert_eq!(config.theme.label.blue, 0xf0);
        assert_eq!(config.extras.len(), 1);
        assert_eq!(Config::parse("").unwrap().theme, Theme::DEFAULT);
        let error = Config::parse("theme = \"neon\"").unwrap_err();
        assert!(error.message.contains("unknown theme"), "{error}");
        let error = Config::parse("[colors]\nplate = \"grey\"").unwrap_err();
        assert_eq!(error.line, 2);
    }

    #[test]
    fn startup_animation_is_on_unless_turned_off() {
        assert!(Config::parse("").unwrap().startup_animation);
        assert!(Config::default_config().startup_animation);
        assert!(
            Config::parse("startup_animation = 'on'")
                .unwrap()
                .startup_animation
        );
        assert!(
            !Config::parse("startup_animation = 'off'")
                .unwrap()
                .startup_animation
        );
        assert!(Config::parse("startup_animation = 'loud'").is_err());
        assert!(Config::parse("[[extras]]\nstartup_animation = 'off'").is_err());
    }

    #[test]
    fn default_row_is_a_top_level_setting() {
        assert!(
            Config::parse("default_row = 'function'")
                .unwrap()
                .function_row_first
        );
        assert!(
            !Config::parse("default_row = 'controls'")
                .unwrap()
                .function_row_first
        );
        assert_eq!(Config::parse("\ndefault_row = 'x'").unwrap_err().line, 2);
        assert!(Config::parse("[[extras]]\ndefault_row = 'function'").is_err());
    }

    #[test]
    fn errors_name_the_line() {
        let cases = [
            ("icon = \"mic\"", 1, "outside"),
            ("[buttons]", 1, "unknown section"),
            ("[[extras]]\nicon = mic", 2, "quoted"),
            (
                "[[extras]]\nicon = \"mic\"\ncolour = \"red\"",
                3,
                "unknown key",
            ),
            ("[[extras]]\nicon = \"mic\"\nicon = \"vpn\"", 3, "twice"),
            (
                "[[extras]]\nicon = \"kettle\"\ncommand = \"x\"",
                1,
                "unknown icon",
            ),
            ("[[extras]]\nicon = \"mic\"", 1, "needs a command"),
            ("[[extras]]\ncommand = \"x\"", 1, "needs an icon"),
            ("[[extras]]\ncommand = \"x", 2, "unterminated"),
        ];
        for (text, line, fragment) in cases {
            let error = Config::parse(text).unwrap_err();
            assert_eq!(error.line, line, "{text}");
            assert!(error.message.contains(fragment), "{text}: {error}");
        }
    }

    #[test]
    fn too_many_extras_is_an_error() {
        let entry = "[[extras]]\nicon = \"mic\"\ncommand = \"x\"\n";
        assert!(Config::parse(&entry.repeat(MAX_EXTRAS)).is_ok());
        assert!(Config::parse(&entry.repeat(MAX_EXTRAS + 1)).is_err());
    }
}
