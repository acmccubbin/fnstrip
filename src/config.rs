//! User configuration: `$XDG_CONFIG_HOME/fnstrip/config.toml` (else `~/.config/...`).
//!
//! The format is a TOML subset, parsed here so the crate stays std-only: `# comments`,
//! `[[extras]]` table-array headers, a `[colors]` table, and `key = "basic"` or
//! `key = 'literal'` string values (basic strings take `\"`, `\\`, `\n`, `\t`), or
//! `true`/`false` for the on/off settings.
//!
//! Parsing is lenient, so a typo or a config written for a newer fnstrip costs only that
//! line: each problem is logged with its line number (in the user journal) and skipped, and
//! the rest of the file applies. Only a missing or unreadable file falls back to the
//! built-in default (`config/default.toml`).
//!
//! Compatibility: settings are only ever added. A renamed one keeps its old name working,
//! with a warning, for at least one minor release.

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
    /// The user's config (each mistake in it logged and skipped), or the built-in default
    /// if there is none or it can't be read.
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default_config();
        };
        match fs::read_to_string(&path) {
            Ok(text) => {
                let (config, problems) = Self::parse(&text);
                for problem in problems {
                    eprintln!("fnstrip: {}: {problem}; skipped", path.display());
                }
                config
            }
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
    /// Only if the embedded default has a mistake, which a unit test rules out.
    #[must_use]
    pub fn default_config() -> Self {
        let (config, problems) = Self::parse(DEFAULT_CONFIG);
        assert!(problems.is_empty(), "built-in default config: {problems:?}");
        config
    }

    /// The config in `text`, and its problems, each with its line number. Lenient, so a
    /// config written for a newer fnstrip, or with a typo, still mostly works: an unknown
    /// setting or section is skipped, a bad value leaves that setting at its default, and
    /// a broken `[[extras]]` entry leaves out that one button.
    #[must_use]
    pub fn parse(text: &str) -> (Self, Vec<ConfigError>) {
        let mut config = Self::default();
        let mut problems = Vec::new();
        let mut section = Section::Top;
        // `[colors]` entries apply over the named theme once the whole file is read.
        let mut colors: Vec<(usize, String, Value)> = Vec::new();
        let mut theme_name: Option<String> = None;
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let mut problem = |message: String| problems.push(ConfigError { line, message });
            let content = strip_comment(raw).trim();
            if content.is_empty() {
                continue;
            }
            if content.starts_with('[') {
                if let Section::Extra(start, extra) = std::mem::replace(&mut section, Section::Top)
                    && let Err(error) = config.push_extra(start, extra)
                {
                    problems.push(error);
                }
                section = match content {
                    "[colors]" => Section::Colors,
                    "[[extras]]" => Section::Extra(line, PartialExtra::default()),
                    _ => {
                        problems.push(ConfigError {
                            line,
                            message: format!(
                                "unknown section `{content}` (known: [colors], [[extras]])"
                            ),
                        });
                        Section::Unknown
                    }
                };
                continue;
            }
            let Some((key, value)) = content.split_once('=') else {
                problem("expected key = \"value\"".to_owned());
                continue;
            };
            let key = key.trim();
            let value = match parse_value(value.trim()) {
                Ok(value) => value,
                Err(message) => {
                    problem(message);
                    continue;
                }
            };
            match &mut section {
                Section::Top => {
                    if let Err(message) = config.set_top_level(key, value, &mut theme_name) {
                        problem(message);
                    }
                }
                Section::Colors => colors.push((line, key.to_owned(), value)),
                Section::Extra(_, extra) => {
                    if let Err(message) = extra.set(key, value) {
                        problem(message);
                    }
                }
                Section::Unknown => {}
            }
        }
        if let Section::Extra(start, extra) = section
            && let Err(error) = config.push_extra(start, extra)
        {
            problems.push(error);
        }
        if let Some(name) = theme_name {
            config.theme = Theme::named(&name).unwrap_or_default();
        }
        for (line, key, value) in colors {
            let result = value
                .text(&key)
                .and_then(|text| config.theme.set(&key, &text));
            if let Err(message) = result {
                problems.push(ConfigError { line, message });
            }
        }
        (config, problems)
    }

    /// A setting above the first `[[extras]]` (the theme is applied after the whole file,
    /// under any `[colors]`).
    fn set_top_level(
        &mut self,
        key: &str,
        value: Value,
        theme_name: &mut Option<String>,
    ) -> Result<(), String> {
        match key {
            "default_row" => {
                self.function_row_first = match value.text(key)?.as_str() {
                    "controls" => false,
                    "function" => true,
                    _ => return Err("default_row must be \"controls\" or \"function\"".into()),
                };
            }
            "startup_animation" => self.startup_animation = value.on_off(key)?,
            "debug_gestures" => self.debug_gestures = value.on_off(key)?,
            "temperature_unit" => {
                self.fahrenheit = match value.text(key)?.to_ascii_lowercase().as_str() {
                    "c" | "celsius" => false,
                    "f" | "fahrenheit" => true,
                    _ => return Err("temperature_unit must be \"c\" or \"f\"".into()),
                };
            }
            "theme" => {
                let name = value.text(key)?;
                if Theme::named(&name).is_none() {
                    return Err(format!(
                        "unknown theme `{name}` (known: {})",
                        Theme::NAMES.join(", ")
                    ));
                }
                *theme_name = Some(name);
            }
            _ => return Err(format!("unknown setting `{key}`")),
        }
        Ok(())
    }

    fn push_extra(&mut self, line: usize, extra: PartialExtra) -> Result<(), ConfigError> {
        let error = |message: String| ConfigError { line, message };
        if self.extras.len() == MAX_EXTRAS {
            return Err(error(format!(
                "at most {MAX_EXTRAS} [[extras]] fit the bar; this one is left out"
            )));
        }
        let icon_name = extra
            .icon
            .ok_or_else(|| error("this [[extras]] has no icon; left out".to_owned()))?;
        let icon = ExtraIcon::from_name(&icon_name).ok_or_else(|| {
            error(format!(
                "unknown icon `{icon_name}` (known: {}); this button is left out",
                ExtraIcon::NAMES.join(", ")
            ))
        })?;
        let command = match extra.command {
            Some(command) => command,
            None if icon.is_meter() => String::new(),
            None => {
                return Err(error(format!(
                    "the `{icon_name}` button has no command; left out"
                )));
            }
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

/// Where in the file a `key = value` line belongs.
enum Section {
    Top,
    Colors,
    /// An `[[extras]]` entry, from this line.
    Extra(usize, PartialExtra),
    /// A section this fnstrip doesn't know (reported once, its entries skipped).
    Unknown,
}

/// A value: a quoted string, or `true`/`false` for the on/off settings.
#[derive(Debug)]
enum Value {
    Text(String),
    Bool(bool),
}

impl Value {
    fn text(self, key: &str) -> Result<String, String> {
        match self {
            Self::Text(text) => Ok(text),
            Self::Bool(_) => Err(format!("`{key}` takes a quoted string")),
        }
    }

    /// `"on"`/`"off"`, or `true`/`false`.
    fn on_off(self, key: &str) -> Result<bool, String> {
        match self {
            Self::Bool(on) => Ok(on),
            Self::Text(text) if text == "on" => Ok(true),
            Self::Text(text) if text == "off" => Ok(false),
            Self::Text(_) => Err(format!("{key} must be \"on\" or \"off\"")),
        }
    }
}

fn parse_value(value: &str) -> Result<Value, String> {
    match value {
        "true" => Ok(Value::Bool(true)),
        "false" => Ok(Value::Bool(false)),
        _ => parse_string(value).map(Value::Text),
    }
}

#[derive(Default)]
struct PartialExtra {
    icon: Option<String>,
    command: Option<String>,
    status: Option<String>,
    interface: Option<String>,
}

impl PartialExtra {
    fn set(&mut self, key: &str, value: Value) -> Result<(), String> {
        let slot = match key {
            "icon" => &mut self.icon,
            "command" => &mut self.command,
            "status" => &mut self.status,
            "interface" => &mut self.interface,
            "default_row" | "startup_animation" | "debug_gestures" | "temperature_unit"
            | "theme" => {
                return Err(format!(
                    "`{key}` belongs above the first [[extras]], not inside one"
                ));
            }
            _ => {
                return Err(format!(
                    "unknown key `{key}` in [[extras]] (known: icon, command, status, interface)"
                ));
            }
        };
        if slot.is_some() {
            return Err(format!("`{key}` given twice; the first one is used"));
        }
        *slot = Some(value.text(key)?);
        Ok(())
    }
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
        return Err("values must be quoted strings (or true/false)".to_owned());
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

    /// The config, and fails the test on any problem.
    fn clean(text: &str) -> Config {
        let (config, problems) = Config::parse(text);
        assert!(problems.is_empty(), "{text}: {problems:?}");
        config
    }

    /// The first problem's line and message.
    fn problem(text: &str) -> (usize, String) {
        let (_, problems) = Config::parse(text);
        let first = problems
            .first()
            .unwrap_or_else(|| panic!("no problem in {text}"));
        (first.line, first.message.clone())
    }

    #[test]
    fn built_in_default_parses() {
        let config = Config::default_config();
        assert_eq!(config.extras.len(), 6);
        assert!(!config.function_row_first);
    }

    #[test]
    fn parses_entries_comments_and_both_string_kinds() {
        let config = clean(
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
        );
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
        let config =
            clean("[[extras]]\nicon = \"cpu\"\n[[extras]]\nicon = \"net\"\ninterface = 'wlan0'");
        assert_eq!(config.extras[0].icon, ExtraIcon::Cpu);
        assert_eq!(config.extras[0].command, "");
        assert_eq!(config.extras[1].interface.as_deref(), Some("wlan0"));
    }

    #[test]
    fn on_off_settings_take_strings_or_booleans() {
        assert!(!clean("").debug_gestures);
        assert!(clean("debug_gestures = \"on\"").debug_gestures);
        assert!(clean("debug_gestures = true").debug_gestures);
        assert!(!clean("startup_animation = false").startup_animation);
        assert!(clean("").startup_animation);
        assert!(Config::default_config().startup_animation);
        assert!(
            problem("debug_gestures = \"yes\"")
                .1
                .contains("\"on\" or \"off\"")
        );
        assert!(problem("theme = true").1.contains("quoted string"));
    }

    #[test]
    fn temperature_is_celsius_unless_set_to_fahrenheit() {
        assert!(!clean("").fahrenheit);
        assert!(clean("temperature_unit = \"f\"").fahrenheit);
        assert!(clean("temperature_unit = \"Fahrenheit\"").fahrenheit);
        assert!(!clean("temperature_unit = \"c\"").fahrenheit);
        assert!(!Config::parse("temperature_unit = \"k\"").0.fahrenheit);
    }

    #[test]
    fn theme_and_colors_apply_in_that_order() {
        let config =
            clean("theme = \"mono\"\n[colors]\nlabel = \"#fffff0\"\n[[extras]]\nicon = \"cpu\"\n");
        let mono = Theme::named("mono").unwrap();
        assert_eq!(config.theme.plate, mono.plate);
        assert_eq!(config.theme.label.blue, 0xf0);
        assert_eq!(config.extras.len(), 1);
        assert_eq!(clean("").theme, Theme::DEFAULT);
        assert!(problem("theme = \"neon\"").1.contains("unknown theme"));
        assert_eq!(problem("[colors]\nplate = \"grey\"").0, 2);
    }

    #[test]
    fn default_row_is_a_top_level_setting() {
        assert!(clean("default_row = 'function'").function_row_first);
        assert!(!clean("default_row = 'controls'").function_row_first);
        assert_eq!(problem("\ndefault_row = 'x'").0, 2);
        assert!(
            problem("[[extras]]\ndefault_row = 'function'")
                .1
                .contains("belongs above")
        );
    }

    #[test]
    fn problems_name_the_line() {
        let cases = [
            ("color = \"red\"", 1, "unknown setting"),
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
            ("[[extras]]\nicon = \"mic\"", 1, "no command"),
            ("[[extras]]\ncommand = \"x\"", 1, "no icon"),
            ("[[extras]]\ncommand = \"x", 2, "unterminated"),
            ("just words", 1, "expected key"),
        ];
        for (text, line, fragment) in cases {
            let (at, message) = problem(text);
            assert_eq!(at, line, "{text}");
            assert!(message.contains(fragment), "{text}: {message}");
        }
    }

    #[test]
    fn a_problem_costs_only_its_own_line_or_button() {
        // A setting from a newer fnstrip, a typo'd value, an unknown section with its
        // entries, and a broken button: everything else still applies.
        let (config, problems) = Config::parse(
            "theme = \"mono\"\n\
             sparkles = \"on\"\n\
             startup_animation = \"maybe\"\n\
             debug_gestures = \"on\"\n\
             [sounds]\n\
             click = \"on\"\n\
             [[extras]]\n\
             icon = \"cpu\"\n\
             [[extras]]\n\
             icon = \"kettle\"\n\
             command = \"x\"\n\
             [[extras]]\n\
             icon = \"mem\"\n\
             speed = \"fast\"\n",
        );
        assert_eq!(config.theme, Theme::named("mono").unwrap());
        assert!(config.startup_animation, "a bad value keeps the default");
        assert!(config.debug_gestures);
        let icons: Vec<_> = config.extras.iter().map(|extra| extra.icon).collect();
        assert_eq!(icons, [ExtraIcon::Cpu, ExtraIcon::Memory]);
        let lines: Vec<_> = problems.iter().map(|problem| problem.line).collect();
        assert_eq!(lines, [2, 3, 5, 9, 14], "{problems:?}");
    }

    #[test]
    fn extras_beyond_ten_are_left_out() {
        let entry = "[[extras]]\nicon = \"mic\"\ncommand = \"x\"\n";
        assert_eq!(clean(&entry.repeat(MAX_EXTRAS)).extras.len(), MAX_EXTRAS);
        let (config, problems) = Config::parse(&entry.repeat(MAX_EXTRAS + 1));
        assert_eq!(config.extras.len(), MAX_EXTRAS);
        assert_eq!(problems.len(), 1);
    }
}
