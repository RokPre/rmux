use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub bindings: BTreeMap<String, String>,
    pub buffer_line: BufferLine,
    pub divider: DividerStyle,
    pub margin: MarginStyle,
    pub resize: Resize,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct BufferLine {
    pub focused_foreground: String,
    pub focused_background: String,
    pub focused_prefix: String,
    pub focused_suffix: String,
    pub focused_prefix_foreground: Option<String>,
    pub focused_prefix_background: Option<String>,
    pub focused_suffix_foreground: Option<String>,
    pub focused_suffix_background: Option<String>,
    pub shown_foreground: String,
    pub shown_background: String,
    pub shown_prefix: String,
    pub shown_suffix: String,
    pub shown_prefix_foreground: Option<String>,
    pub shown_prefix_background: Option<String>,
    pub shown_suffix_foreground: Option<String>,
    pub shown_suffix_background: Option<String>,
    pub hidden_foreground: String,
    pub hidden_background: String,
    pub hidden_prefix: String,
    pub hidden_suffix: String,
    pub hidden_prefix_foreground: Option<String>,
    pub hidden_prefix_background: Option<String>,
    pub hidden_suffix_foreground: Option<String>,
    pub hidden_suffix_background: Option<String>,
    pub marker_color: bool,
    pub bold: bool,
    pub rename_foreground: Option<String>,
    pub rename_background: Option<String>,
    pub background: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DividerStyle {
    pub foreground: String,
    pub vertical: String,
    pub horizontal: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct MarginStyle {
    pub foreground: String,
    pub background: String,
    pub fill: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Resize {
    pub step_percent: u8,
}

impl Default for Config {
    fn default() -> Self {
        let bindings = [
            ("previous_buffer", "ctrl+h"),
            ("next_buffer", "ctrl+l"),
            ("rename_buffer", "ctrl+r"),
            ("new_buffer", "ctrl+t"),
            ("close_buffer", "ctrl+w"),
            ("reorder_buffer_left", "ctrl+shift+h"),
            ("reorder_buffer_right", "ctrl+shift+l"),
            ("resize_left", "alt+ctrl+h"),
            ("resize_down", "alt+ctrl+j"),
            ("resize_up", "alt+ctrl+k"),
            ("resize_right", "alt+ctrl+l"),
            ("move_left", "alt+shift+h"),
            ("move_down", "alt+shift+j"),
            ("move_up", "alt+shift+k"),
            ("move_right", "alt+shift+l"),
            ("focus_left", "alt+h"),
            ("focus_down", "alt+j"),
            ("focus_up", "alt+k"),
            ("focus_right", "alt+l"),
            ("split_vertical", "alt+v"),
            ("split_horizontal", "alt+b"),
            ("close_window", "alt+w"),
            ("equalize", "alt+="),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
        Self {
            bindings,
            buffer_line: BufferLine::default(),
            divider: DividerStyle::default(),
            margin: MarginStyle::default(),
            resize: Resize::default(),
        }
    }
}
impl Default for BufferLine {
    fn default() -> Self {
        Self {
            focused_foreground: "white".into(),
            focused_background: "dark_grey".into(),
            focused_prefix: "[".into(),
            focused_suffix: "]".into(),
            focused_prefix_foreground: None,
            focused_prefix_background: None,
            focused_suffix_foreground: None,
            focused_suffix_background: None,
            shown_foreground: "white".into(),
            shown_background: "dark_grey".into(),
            shown_prefix: String::new(),
            shown_suffix: String::new(),
            shown_prefix_foreground: None,
            shown_prefix_background: None,
            shown_suffix_foreground: None,
            shown_suffix_background: None,
            hidden_foreground: "white".into(),
            hidden_background: "dark_grey".into(),
            hidden_prefix: String::new(),
            hidden_suffix: String::new(),
            hidden_prefix_foreground: None,
            hidden_prefix_background: None,
            hidden_suffix_foreground: None,
            hidden_suffix_background: None,
            marker_color: false,
            bold: true,
            rename_foreground: None,
            rename_background: None,
            background: "dark_grey".into(),
        }
    }
}
impl Default for DividerStyle {
    fn default() -> Self {
        Self {
            foreground: "dark_grey".into(),
            vertical: "│".into(),
            horizontal: "─".into(),
        }
    }
}
impl Default for MarginStyle {
    fn default() -> Self {
        Self {
            foreground: "dark_grey".into(),
            background: "reset".into(),
            fill: ".".into(),
        }
    }
}
impl Default for Resize {
    fn default() -> Self {
        Self { step_percent: 5 }
    }
}

impl Config {
    pub(crate) fn load() -> Result<Self, String> {
        let path = config_path()?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("cannot read config {}: {error}", path.display())),
        };
        let config: Self = toml::from_str(&text)
            .map_err(|error| format!("invalid config {}: {error}", path.display()))?;
        config.validate(&path)?;
        Ok(config)
    }

    fn validate(&self, path: &PathBuf) -> Result<(), String> {
        let invalid = |message: String| format!("invalid config {}: {message}", path.display());
        if !(1..=50).contains(&self.resize.step_percent) {
            return Err(invalid(
                "resize.step_percent must be between 1 and 50".into(),
            ));
        }
        for (field, value) in [
            (
                "buffer_line.focused_foreground",
                &self.buffer_line.focused_foreground,
            ),
            (
                "buffer_line.focused_background",
                &self.buffer_line.focused_background,
            ),
            (
                "buffer_line.shown_foreground",
                &self.buffer_line.shown_foreground,
            ),
            (
                "buffer_line.shown_background",
                &self.buffer_line.shown_background,
            ),
            (
                "buffer_line.hidden_foreground",
                &self.buffer_line.hidden_foreground,
            ),
            (
                "buffer_line.hidden_background",
                &self.buffer_line.hidden_background,
            ),
            ("buffer_line.background", &self.buffer_line.background),
            ("divider.foreground", &self.divider.foreground),
            ("margin.foreground", &self.margin.foreground),
            ("margin.background", &self.margin.background),
        ] {
            if parse_color(value).is_none() {
                return Err(invalid(format!("{field} has unknown color {value:?}")));
            }
        }
        for (field, value) in [
            (
                "buffer_line.rename_foreground",
                &self.buffer_line.rename_foreground,
            ),
            (
                "buffer_line.rename_background",
                &self.buffer_line.rename_background,
            ),
            (
                "buffer_line.focused_prefix_foreground",
                &self.buffer_line.focused_prefix_foreground,
            ),
            (
                "buffer_line.focused_prefix_background",
                &self.buffer_line.focused_prefix_background,
            ),
            (
                "buffer_line.focused_suffix_foreground",
                &self.buffer_line.focused_suffix_foreground,
            ),
            (
                "buffer_line.focused_suffix_background",
                &self.buffer_line.focused_suffix_background,
            ),
            (
                "buffer_line.shown_prefix_foreground",
                &self.buffer_line.shown_prefix_foreground,
            ),
            (
                "buffer_line.shown_prefix_background",
                &self.buffer_line.shown_prefix_background,
            ),
            (
                "buffer_line.shown_suffix_foreground",
                &self.buffer_line.shown_suffix_foreground,
            ),
            (
                "buffer_line.shown_suffix_background",
                &self.buffer_line.shown_suffix_background,
            ),
            (
                "buffer_line.hidden_prefix_foreground",
                &self.buffer_line.hidden_prefix_foreground,
            ),
            (
                "buffer_line.hidden_prefix_background",
                &self.buffer_line.hidden_prefix_background,
            ),
            (
                "buffer_line.hidden_suffix_foreground",
                &self.buffer_line.hidden_suffix_foreground,
            ),
            (
                "buffer_line.hidden_suffix_background",
                &self.buffer_line.hidden_suffix_background,
            ),
        ] {
            if let Some(value) = value {
                if parse_color(value).is_none() {
                    return Err(invalid(format!("{field} has unknown color {value:?}")));
                }
            }
        }
        for (field, value) in [
            (
                "buffer_line.focused_prefix",
                &self.buffer_line.focused_prefix,
            ),
            (
                "buffer_line.focused_suffix",
                &self.buffer_line.focused_suffix,
            ),
            ("buffer_line.shown_prefix", &self.buffer_line.shown_prefix),
            ("buffer_line.shown_suffix", &self.buffer_line.shown_suffix),
            ("buffer_line.hidden_prefix", &self.buffer_line.hidden_prefix),
            ("buffer_line.hidden_suffix", &self.buffer_line.hidden_suffix),
        ] {
            if value.chars().any(char::is_control) {
                return Err(invalid(format!(
                    "{field} must not contain terminal control characters"
                )));
            }
        }
        for glyph in [&self.divider.vertical, &self.divider.horizontal] {
            if glyph.chars().count() != 1
                || glyph.chars().any(char::is_control)
                || unicode_width::UnicodeWidthStr::width(glyph.as_str()) != 1
            {
                return Err(invalid(
                    "divider glyphs must each be one printable terminal column".into(),
                ));
            }
        }
        let fill = &self.margin.fill;
        if fill.chars().count() != 1
            || fill.chars().any(char::is_control)
            || unicode_width::UnicodeWidthStr::width(fill.as_str()) != 1
        {
            return Err(invalid(
                "margin.fill must be one printable terminal column".into(),
            ));
        }
        let mut bindings = Config::default().bindings;
        bindings.extend(self.bindings.clone());
        let mut sequences: Vec<(String, Vec<u8>)> = Vec::new();
        for (action, key) in &bindings {
            if !ACTIONS.contains(&action.as_str()) {
                return Err(invalid(format!("unknown binding action {action:?}")));
            }
            let sequence = parse_key(key).ok_or_else(|| invalid(format!("bindings.{action} has invalid key {key:?}; use ctrl+KEY, alt+KEY, alt+shift+KEY, alt+ctrl+KEY, ctrl+shift+KEY, or a plain ASCII character")))?;
            if sequences.iter().any(|(_, other)| other == &sequence) {
                return Err(invalid(format!(
                    "bindings.{action} duplicates another key sequence"
                )));
            }
            sequences.push((action.clone(), sequence));
        }
        for (i, (action, sequence)) in sequences.iter().enumerate() {
            for (other, other_sequence) in sequences.iter().skip(i + 1) {
                if sequence.starts_with(other_sequence) || other_sequence.starts_with(sequence) {
                    return Err(invalid(format!(
                        "bindings.{action} and bindings.{other} have ambiguous prefix sequences"
                    )));
                }
            }
        }
        Ok(())
    }
}

const ACTIONS: &[&str] = &[
    "previous_buffer",
    "next_buffer",
    "rename_buffer",
    "new_buffer",
    "close_buffer",
    "reorder_buffer_left",
    "reorder_buffer_right",
    "resize_left",
    "resize_down",
    "resize_up",
    "resize_right",
    "move_left",
    "move_down",
    "move_up",
    "move_right",
    "focus_left",
    "focus_down",
    "focus_up",
    "focus_right",
    "split_vertical",
    "split_horizontal",
    "close_window",
    "equalize",
];

pub(crate) fn parse_color(name: &str) -> Option<crossterm::style::Color> {
    use crossterm::style::Color::*;
    Some(match name.to_ascii_lowercase().as_str() {
        "reset" => Reset,
        "black" => Black,
        "dark_grey" | "dark-gray" => DarkGrey,
        "red" => Red,
        "dark_red" | "dark-red" => DarkRed,
        "green" => Green,
        "dark_green" | "dark-green" => DarkGreen,
        "yellow" => Yellow,
        "dark_yellow" | "dark-yellow" => DarkYellow,
        "blue" => Blue,
        "dark_blue" | "dark-blue" => DarkBlue,
        "magenta" => Magenta,
        "dark_magenta" | "dark-magenta" => DarkMagenta,
        "cyan" => Cyan,
        "dark_cyan" | "dark-cyan" => DarkCyan,
        "white" => White,
        "grey" | "gray" => Grey,
        _ if name.starts_with('#')
            && name.len() == 7
            && name.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit) =>
        {
            Rgb {
                r: u8::from_str_radix(&name[1..3], 16).ok()?,
                g: u8::from_str_radix(&name[3..5], 16).ok()?,
                b: u8::from_str_radix(&name[5..7], 16).ok()?,
            }
        }
        _ => return None,
    })
}

pub(crate) fn parse_key(key: &str) -> Option<Vec<u8>> {
    let (modifiers, character) = if let Some(character) = key.strip_prefix("ctrl+shift+") {
        ("ctrl+shift", character)
    } else if let Some(character) = key.strip_prefix("alt+ctrl+") {
        ("alt+ctrl", character)
    } else if let Some(character) = key.strip_prefix("alt+shift+") {
        ("alt+shift", character)
    } else if let Some(character) = key.strip_prefix("ctrl+") {
        ("ctrl", character)
    } else if let Some(character) = key.strip_prefix("alt+") {
        ("alt", character)
    } else {
        ("", key)
    };
    if character.len() != 1 || !character.as_bytes()[0].is_ascii() {
        return None;
    }
    let c = character.as_bytes()[0];
    match modifiers {
        "ctrl+shift" if c.is_ascii_alphabetic() => {
            Some(format!("\x1b[{};6u", c.to_ascii_lowercase() as u16).into_bytes())
        }
        "alt+ctrl" if c.is_ascii_alphabetic() => Some(vec![0x1b, c.to_ascii_uppercase() - b'@']),
        "ctrl" if c.is_ascii_alphabetic() => Some(vec![c.to_ascii_uppercase() - b'@']),
        "alt+shift" => Some(vec![0x1b, c.to_ascii_uppercase()]),
        "alt" => Some(vec![0x1b, c]),
        "" => Some(vec![c]),
        _ => None,
    }
}

fn config_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("rmux/config.toml"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "cannot locate config: HOME is not set".to_owned())?;
    Ok(PathBuf::from(home).join(".config/rmux/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_toml_keeps_omitted_defaults() {
        let config: Config = toml::from_str("[resize]\nstep_percent = 12\n").unwrap();
        assert_eq!(config.resize.step_percent, 12);
        assert_eq!(config.buffer_line.focused_background, "dark_grey");
        assert_eq!(config.divider.vertical, "│");
        assert_eq!(config.margin.background, "reset");
        config.validate(&PathBuf::from("test.toml")).unwrap();
        assert_eq!(config.buffer_line.focused_prefix, "[");
        assert_eq!(config.buffer_line.focused_suffix, "]");
        assert_eq!(config.buffer_line.shown_prefix, "");
        assert!(!config.buffer_line.marker_color);
        assert_eq!(config.buffer_line.focused_prefix_foreground, None);
        assert_eq!(config.buffer_line.hidden_suffix_background, None);
    }

    #[test]
    fn rename_prompt_colors_are_optional_and_validated() {
        let mut config: Config = toml::from_str(
            "[buffer_line]\nrename_foreground = \"#ff9e64\"\nrename_background = \"#16161e\"\n",
        )
        .unwrap();
        config.validate(&PathBuf::from("test.toml")).unwrap();
        config.buffer_line.rename_background = Some("not-a-color".into());
        assert!(
            config
                .validate(&PathBuf::from("test.toml"))
                .unwrap_err()
                .contains("buffer_line.rename_background")
        );
    }
    #[test]
    fn marker_colors_are_optional_independent_and_validated() {
        let config: Config = toml::from_str(
            r##"
                [buffer_line]
                focused_prefix_foreground = "#ff0000"
                focused_suffix_background = "blue"
                shown_prefix_background = "#00ff00"
                hidden_suffix_foreground = "white"
            "##,
        )
        .unwrap();
        assert_eq!(
            config.buffer_line.focused_prefix_foreground.as_deref(),
            Some("#ff0000")
        );
        assert_eq!(config.buffer_line.focused_prefix_background, None);
        assert_eq!(
            config.buffer_line.focused_suffix_background.as_deref(),
            Some("blue")
        );
        assert_eq!(
            config.buffer_line.shown_prefix_background.as_deref(),
            Some("#00ff00")
        );
        assert_eq!(
            config.buffer_line.hidden_suffix_foreground.as_deref(),
            Some("white")
        );
        config.validate(&PathBuf::from("test.toml")).unwrap();

        let mut invalid: Config = toml::from_str(
            r#"
                [buffer_line]
                shown_suffix_foreground = "not-a-color"
            "#,
        )
        .unwrap();
        let error = invalid.validate(&PathBuf::from("test.toml")).unwrap_err();
        assert!(error.contains("buffer_line.shown_suffix_foreground"));

        invalid.buffer_line.shown_suffix_foreground = None;
        invalid.buffer_line.hidden_prefix_background = Some("still-not-a-color".into());
        let error = invalid.validate(&PathBuf::from("test.toml")).unwrap_err();
        assert!(error.contains("buffer_line.hidden_prefix_background"));
    }

    #[test]
    fn invalid_overrides_report_specific_config_values() {
        let mut config = Config::default();
        config.bindings.insert("new_buffer".into(), "ctrl+g".into());
        config
            .bindings
            .insert("next_buffer".into(), "ctrl+g".into());
        assert!(
            config
                .validate(&PathBuf::from("test.toml"))
                .unwrap_err()
                .contains("duplicates")
        );
        config = Config::default();
        config.divider.vertical = "\x1b".into();
        assert!(
            config
                .validate(&PathBuf::from("test.toml"))
                .unwrap_err()
                .contains("divider glyphs")
        );
        config = Config::default();
        config.buffer_line.shown_foreground = "not-a-color".into();
        assert!(
            config
                .validate(&PathBuf::from("test.toml"))
                .unwrap_err()
                .contains("shown_foreground")
        );
    }
    #[test]
    fn marker_strings_reject_terminal_controls() {
        let mut config = Config::default();
        config.buffer_line.focused_prefix = "\u{1b}[31m".into();
        assert!(
            config
                .validate(&PathBuf::from("test.toml"))
                .unwrap_err()
                .contains("focused_prefix")
        );
    }

    #[test]
    fn defaults_encode_existing_escape_sequences() {
        assert_eq!(parse_key("ctrl+shift+h").unwrap(), b"\x1b[104;6u");
        assert_eq!(parse_key("alt+ctrl+h").unwrap(), b"\x1b\x08");
        assert_eq!(parse_key("alt+shift+h").unwrap(), b"\x1bH");
    }

    #[test]
    fn literal_plus_keys_are_supported() {
        assert_eq!(parse_key("+").unwrap(), b"+");
        assert_eq!(parse_key("alt++").unwrap(), b"\x1b+");
        assert_eq!(parse_key("alt+shift++").unwrap(), b"\x1b+");
    }
    #[test]
    fn unsafe_color_text_and_literal_key_case_are_handled_safely() {
        assert!(parse_color("#ééééé").is_none());
        assert_eq!(parse_key("A").unwrap(), b"A");
        assert_eq!(parse_key("alt+Q").unwrap(), b"\x1bQ");
        assert_eq!(
            parse_color("#a1b2c3"),
            Some(crossterm::style::Color::Rgb {
                r: 0xa1,
                g: 0xb2,
                b: 0xc3
            })
        );
    }
}
