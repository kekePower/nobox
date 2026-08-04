//! Loading, validation, and discovery for nobox configuration.

use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::Deserialize;
use thiserror::Error;

/// The configuration shipped with nobox.
pub const DEFAULT_CONFIG: &str = include_str!("../default.toml");

/// Complete user configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Focus behavior.
    pub focus: FocusConfig,
    /// Minimal client decoration.
    pub theme: ThemeConfig,
    /// Mouse actions.
    pub mouse: MouseConfig,
    /// Global keyboard actions.
    pub keyboard: KeyboardConfig,
}

impl Config {
    /// Parses and validates TOML configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed TOML or an invalid combination of values.
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(source)?;
        config.validate()?;
        Ok(config)
    }

    /// Loads and validates configuration from a file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or is invalid.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let source = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&source)
    }

    /// Validates relationships that serde cannot express.
    ///
    /// # Errors
    ///
    /// Returns an error when mouse buttons overlap or a border is unreasonable.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.mouse.move_button == self.mouse.resize_button {
            return Err(ConfigError::SameMouseButton(self.mouse.move_button));
        }
        if !(1..=5).contains(&self.mouse.move_button) {
            return Err(ConfigError::InvalidMouseButton(self.mouse.move_button));
        }
        if !(1..=5).contains(&self.mouse.resize_button) {
            return Err(ConfigError::InvalidMouseButton(self.mouse.resize_button));
        }
        if self.theme.border_width > 64 {
            return Err(ConfigError::BorderTooWide(self.theme.border_width));
        }
        let mut bindings = BTreeSet::new();
        for binding in &self.keyboard.bindings {
            if !bindings.insert(binding.key.clone()) {
                return Err(ConfigError::DuplicateKeyBinding(binding.key.to_string()));
            }
            if let Action::Execute { command } = &binding.action
                && command.trim().is_empty()
            {
                return Err(ConfigError::EmptyCommand(binding.key.to_string()));
            }
        }
        Ok(())
    }
}

/// Focus behavior.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct FocusConfig {
    /// Focus newly mapped windows.
    pub focus_new: bool,
    /// Raise a window whenever nobox focuses it.
    pub raise_on_focus: bool,
}

impl Default for FocusConfig {
    fn default() -> Self {
        Self {
            focus_new: true,
            raise_on_focus: true,
        }
    }
}

/// Client border settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    /// Border width in pixels.
    pub border_width: u32,
    /// Focused-client border color.
    pub active_border: RgbColor,
    /// Unfocused-client border color.
    pub inactive_border: RgbColor,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            border_width: 2,
            active_border: RgbColor::new(0x8a, 0xad, 0xf4),
            inactive_border: RgbColor::new(0x49, 0x4d, 0x64),
        }
    }
}

/// Mouse bindings used by the X11 backend.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MouseConfig {
    /// Modifier held for window-management drags.
    pub modifier: MouseModifier,
    /// Button used to move a client.
    pub move_button: u8,
    /// Button used to resize a client from its bottom-right corner.
    pub resize_button: u8,
}

impl Default for MouseConfig {
    fn default() -> Self {
        Self {
            modifier: MouseModifier::Super,
            move_button: 1,
            resize_button: 3,
        }
    }
}

/// Global keyboard bindings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct KeyboardConfig {
    /// Ordered key-to-action mappings.
    pub bindings: Vec<KeyBinding>,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            bindings: vec![
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Super], "Return"),
                    action: Action::Execute {
                        command: "xterm".to_owned(),
                    },
                },
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Super], "q"),
                    action: Action::Close,
                },
                KeyBinding {
                    key: KeyChord::new(
                        [KeyboardModifier::Super, KeyboardModifier::Shift],
                        "Escape",
                    ),
                    action: Action::Exit,
                },
            ],
        }
    }
}

/// One global keyboard binding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KeyBinding {
    /// Key chord such as `W-Return` or `W-S-Escape`.
    pub key: KeyChord,
    /// Action executed when the chord is pressed.
    pub action: Action,
}

/// An action dispatched by the window manager.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Start a command through `/bin/sh -c`.
    Execute {
        /// Shell command to start.
        command: String,
    },
    /// Ask the focused client to close using ICCCM when supported.
    Close,
    /// Exit the window manager.
    Exit,
}

/// Parsed global key chord.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct KeyChord {
    modifiers: Vec<KeyboardModifier>,
    symbol: String,
}

impl KeyChord {
    fn new(
        modifiers: impl IntoIterator<Item = KeyboardModifier>,
        symbol: impl Into<String>,
    ) -> Self {
        let mut modifiers = modifiers.into_iter().collect::<Vec<_>>();
        modifiers.sort_unstable();
        modifiers.dedup();
        Self {
            modifiers,
            symbol: symbol.into(),
        }
    }

    /// Returns the chord's modifiers in canonical order.
    #[must_use]
    pub fn modifiers(&self) -> &[KeyboardModifier] {
        &self.modifiers
    }

    /// Returns the X11 keysym name.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }
}

impl std::fmt::Display for KeyChord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for modifier in &self.modifiers {
            write!(formatter, "{}-", modifier.short_name())?;
        }
        formatter.write_str(&self.symbol)
    }
}

impl<'de> Deserialize<'de> for KeyChord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl FromStr for KeyChord {
    type Err = KeyChordError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parts = value.split('-').collect::<Vec<_>>();
        let Some((symbol, modifier_names)) = parts.split_last() else {
            return Err(KeyChordError(value.to_owned()));
        };
        if symbol.trim().is_empty() {
            return Err(KeyChordError(value.to_owned()));
        }

        let mut modifiers = Vec::with_capacity(modifier_names.len());
        for name in modifier_names {
            let modifier = match name.to_ascii_lowercase().as_str() {
                "c" | "ctrl" | "control" => KeyboardModifier::Control,
                "a" | "alt" => KeyboardModifier::Alt,
                "s" | "shift" => KeyboardModifier::Shift,
                "w" | "super" => KeyboardModifier::Super,
                _ => return Err(KeyChordError(value.to_owned())),
            };
            if modifiers.contains(&modifier) {
                return Err(KeyChordError(value.to_owned()));
            }
            modifiers.push(modifier);
        }
        Ok(Self::new(modifiers, *symbol))
    }
}

/// A modifier in a global key chord.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum KeyboardModifier {
    /// Control.
    Control,
    /// Alt/Mod1.
    Alt,
    /// Shift.
    Shift,
    /// Super/Mod4.
    Super,
}

impl KeyboardModifier {
    const fn short_name(self) -> &'static str {
        match self {
            Self::Control => "C",
            Self::Alt => "A",
            Self::Shift => "S",
            Self::Super => "W",
        }
    }
}

/// Modifier supported for mouse actions.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum MouseModifier {
    /// Alt/Mod1.
    Alt,
    /// Super/Mod4.
    #[default]
    Super,
}

/// An RGB color stored as `0xRRGGBB`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RgbColor(u32);

impl RgbColor {
    /// Creates a color from red, green, and blue channels.
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self((red as u32) << 16 | (green as u32) << 8 | blue as u32)
    }

    /// Returns `0xRRGGBB`.
    #[must_use]
    pub const fn pixel(self) -> u32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for RgbColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl FromStr for RgbColor {
    type Err = ColorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(hex) = value.strip_prefix('#') else {
            return Err(ColorError);
        };
        if hex.len() != 6 {
            return Err(ColorError);
        }
        u32::from_str_radix(hex, 16)
            .map(Self)
            .map_err(|_| ColorError)
    }
}

/// Returns the path nobox uses for its primary configuration file.
///
/// `NOBOX_CONFIG_FILE` overrides the XDG location.
///
/// # Errors
///
/// Returns an error when neither `XDG_CONFIG_HOME` nor `HOME` is usable.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    if let Some(path) = env::var_os("NOBOX_CONFIG_FILE").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    Ok(config_dir()?.join("config.toml"))
}

/// Returns the nobox configuration directory.
///
/// # Errors
///
/// Returns an error when neither `XDG_CONFIG_HOME` nor `HOME` is usable.
pub fn config_dir() -> Result<PathBuf, ConfigError> {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path).join("nobox"));
    }
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join(".config/nobox"))
        .ok_or(ConfigError::NoConfigHome)
}

/// Configuration failures with actionable context.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The operating environment has no configuration home.
    #[error("neither XDG_CONFIG_HOME nor HOME is set")]
    NoConfigHome,
    /// A configuration file could not be read.
    #[error("could not read configuration at {path}")]
    Read {
        /// Attempted path.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// TOML could not be decoded.
    #[error("invalid TOML configuration")]
    Toml(#[from] toml::de::Error),
    /// Move and resize cannot use the same button.
    #[error("move_button and resize_button both use button {0}")]
    SameMouseButton(u8),
    /// X11 only has five conventional pointer buttons for these bindings.
    #[error("mouse button {0} is outside the supported range 1..=5")]
    InvalidMouseButton(u8),
    /// Prevent accidental unusable decoration.
    #[error("border width {0} exceeds the maximum of 64 pixels")]
    BorderTooWide(u32),
    /// The same chord appeared more than once.
    #[error("duplicate keyboard binding for {0}")]
    DuplicateKeyBinding(String),
    /// Execute actions must contain a command.
    #[error("execute action for {0} has an empty command")]
    EmptyCommand(String),
}

/// Error returned for a malformed `#RRGGBB` color.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("expected a color in #RRGGBB form")]
pub struct ColorError;

/// Error returned for malformed key-chord syntax.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid key chord {0:?}; use modifiers C/A/S/W followed by an X11 keysym name")]
pub struct KeyChordError(String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_configuration_matches_rust_defaults() {
        assert_eq!(
            Config::parse(DEFAULT_CONFIG).expect("valid default"),
            Config::default()
        );
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let error = Config::parse("mystery = true").expect_err("unknown key must fail");
        assert!(matches!(error, ConfigError::Toml(_)));
    }

    #[test]
    fn duplicate_mouse_actions_are_rejected() {
        let error = Config::parse("[mouse]\nmove_button = 2\nresize_button = 2")
            .expect_err("duplicate button must fail");
        assert!(matches!(error, ConfigError::SameMouseButton(2)));
    }

    #[test]
    fn colors_require_six_hex_digits() {
        assert_eq!("#12aBcF".parse::<RgbColor>(), Ok(RgbColor(0x12_ab_cf)));
        assert!("blue".parse::<RgbColor>().is_err());
    }

    #[test]
    fn key_chords_are_parsed_and_canonicalized() {
        let chord = "Shift-W-Return".parse::<KeyChord>().expect("valid chord");
        assert_eq!(chord.to_string(), "S-W-Return");
        assert_eq!(
            chord.modifiers(),
            [KeyboardModifier::Shift, KeyboardModifier::Super]
        );
    }

    #[test]
    fn malformed_and_duplicate_key_chords_are_rejected() {
        assert!("W-W-q".parse::<KeyChord>().is_err());
        let error = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-q'\naction = { type = 'close' }\n\
             [[keyboard.bindings]]\nkey = 'Super-q'\naction = { type = 'exit' }",
        )
        .expect_err("equivalent duplicate chord must fail");
        assert!(matches!(error, ConfigError::DuplicateKeyBinding(_)));
    }
}
