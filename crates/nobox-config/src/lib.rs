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
    /// Initial window placement behavior.
    pub placement: PlacementConfig,
    /// Protocol-neutral workspace names and count.
    pub workspaces: WorkspaceConfig,
    /// Minimal client decoration.
    pub theme: ThemeConfig,
    /// Mouse actions.
    pub mouse: MouseConfig,
    /// Global keyboard actions.
    pub keyboard: KeyboardConfig,
    /// Ordered application-specific policy overrides.
    pub applications: Vec<ApplicationRule>,
}

/// Smart initial-placement behavior.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PlacementConfig {
    /// Center windows within the first completely free grid field.
    pub center_free_space: bool,
}

impl Default for PlacementConfig {
    fn default() -> Self {
        Self {
            center_free_space: true,
        }
    }
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
    /// Returns an error when input or decoration values are unreasonable.
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
        if self.mouse.edge_resistance > 256 {
            return Err(ConfigError::EdgeResistanceTooStrong(
                self.mouse.edge_resistance,
            ));
        }
        if self.theme.border_width > 64 {
            return Err(ConfigError::BorderTooWide(self.theme.border_width));
        }
        if self.theme.titlebar_height > 128 {
            return Err(ConfigError::TitlebarTooTall(self.theme.titlebar_height));
        }
        if self.workspaces.names.is_empty() {
            return Err(ConfigError::NoWorkspaces);
        }
        if self.workspaces.names.len() > 32 {
            return Err(ConfigError::TooManyWorkspaces(self.workspaces.names.len()));
        }
        if usize::try_from(self.workspaces.columns)
            .is_ok_and(|columns| columns > self.workspaces.names.len())
        {
            return Err(ConfigError::TooManyWorkspaceColumns {
                columns: self.workspaces.columns,
                count: self.workspaces.names.len(),
            });
        }
        for (index, name) in self.workspaces.names.iter().enumerate() {
            if name.trim().is_empty() || name.contains('\0') {
                return Err(ConfigError::InvalidWorkspaceName(index + 1));
            }
        }
        for (index, rule) in self.applications.iter().enumerate() {
            if rule.matcher.is_empty() {
                return Err(ConfigError::EmptyApplicationMatcher(index + 1));
            }
            for pattern in [
                rule.matcher.name.as_deref(),
                rule.matcher.class.as_deref(),
                rule.matcher.role.as_deref(),
                rule.matcher.title.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                if pattern.is_empty() {
                    return Err(ConfigError::EmptyApplicationPattern(index + 1));
                }
            }
            if let Some(workspace) = rule.settings.workspace
                && (workspace == 0
                    || usize::try_from(workspace)
                        .map_or(true, |workspace| workspace > self.workspaces.names.len()))
            {
                return Err(ConfigError::InvalidApplicationWorkspace {
                    rule: index + 1,
                    workspace,
                    count: self.workspaces.names.len(),
                });
            }
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
            let workspace = match &binding.action {
                Action::SwitchWorkspace { workspace }
                | Action::MoveToWorkspace { workspace, .. } => Some(*workspace),
                Action::Execute { .. }
                | Action::Close
                | Action::NextWindow
                | Action::PreviousWindow
                | Action::PreviousWorkspace
                | Action::NextWorkspace
                | Action::WorkspaceLeft
                | Action::WorkspaceRight
                | Action::WorkspaceUp
                | Action::WorkspaceDown
                | Action::MoveToPreviousWorkspace { .. }
                | Action::MoveToNextWorkspace { .. }
                | Action::MoveToWorkspaceLeft { .. }
                | Action::MoveToWorkspaceRight { .. }
                | Action::MoveToWorkspaceUp { .. }
                | Action::MoveToWorkspaceDown { .. }
                | Action::Exit => None,
            };
            if workspace.is_some_and(|workspace| {
                workspace == 0
                    || usize::try_from(workspace)
                        .map_or(true, |workspace| workspace > self.workspaces.names.len())
            }) {
                return Err(ConfigError::InvalidWorkspaceBinding {
                    key: binding.key.to_string(),
                    workspace: workspace.unwrap_or_default(),
                    count: self.workspaces.names.len(),
                });
            }
        }
        Ok(())
    }
}

/// Protocol-neutral application metadata used by ordered rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationIdentity<'a> {
    /// Application instance/name.
    pub name: &'a str,
    /// Application class.
    pub class: &'a str,
    /// Toolkit/application role string.
    pub role: &'a str,
    /// Current window title.
    pub title: &'a str,
    /// Functional top-level type.
    pub kind: ApplicationKind,
}

/// Functional type available to application-rule matching.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationKind {
    /// Ordinary application window.
    Normal,
    /// Dialog.
    Dialog,
    /// Utility window.
    Utility,
    /// Toolbar.
    Toolbar,
    /// Menu.
    Menu,
    /// Splash window.
    Splash,
    /// Desktop surface.
    Desktop,
    /// Dock or panel.
    Dock,
    /// Drop-down menu.
    DropdownMenu,
    /// Pop-up menu.
    PopupMenu,
    /// Tooltip.
    Tooltip,
    /// Notification.
    Notification,
    /// Combo-box pop-up.
    Combo,
    /// Drag-and-drop surface.
    DragAndDrop,
}

/// One ordered application rule.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationRule {
    /// Fields that must all match.
    #[serde(rename = "match")]
    pub matcher: ApplicationMatcher,
    /// Optional policy values supplied by this rule.
    #[serde(flatten)]
    pub settings: ApplicationSettings,
}

impl ApplicationRule {
    /// Returns whether every configured matcher accepts `identity`.
    #[must_use]
    pub fn matches(&self, identity: ApplicationIdentity<'_>) -> bool {
        self.matcher.matches(identity)
    }
}

/// Conjunctive application identity matcher.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ApplicationMatcher {
    /// Instance/name wildcard.
    pub name: Option<String>,
    /// Class wildcard.
    pub class: Option<String>,
    /// Role wildcard.
    pub role: Option<String>,
    /// Title wildcard.
    pub title: Option<String>,
    /// Functional type.
    pub kind: Option<ApplicationKind>,
}

impl ApplicationMatcher {
    fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.class.is_none()
            && self.role.is_none()
            && self.title.is_none()
            && self.kind.is_none()
    }

    fn matches(&self, identity: ApplicationIdentity<'_>) -> bool {
        self.name
            .as_deref()
            .is_none_or(|pattern| wildcard_matches(pattern, identity.name))
            && self
                .class
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, identity.class))
            && self
                .role
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, identity.role))
            && self
                .title
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, identity.title))
            && self.kind.is_none_or(|kind| kind == identity.kind)
    }
}

/// Optional policy values merged from matching rules in declaration order.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ApplicationSettings {
    /// One-based workspace number.
    pub workspace: Option<u32>,
    /// Requested stacking layer.
    pub layer: Option<ApplicationLayer>,
    /// Whether nobox should decorate the client.
    pub decorated: Option<bool>,
    /// Whether a newly mapped client should receive focus.
    pub focus: Option<bool>,
}

impl ApplicationSettings {
    fn merge(&mut self, newer: Self) {
        if newer.workspace.is_some() {
            self.workspace = newer.workspace;
        }
        if newer.layer.is_some() {
            self.layer = newer.layer;
        }
        if newer.decorated.is_some() {
            self.decorated = newer.decorated;
        }
        if newer.focus.is_some() {
            self.focus = newer.focus;
        }
    }
}

/// User-requested rule layer independent of display protocol.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationLayer {
    /// Below ordinary clients.
    Below,
    /// Default layer.
    Normal,
    /// Above ordinary clients and docks.
    Above,
}

impl Config {
    /// Resolves ordered matching application rules; later values override earlier ones.
    #[must_use]
    pub fn application_settings(&self, identity: ApplicationIdentity<'_>) -> ApplicationSettings {
        let mut settings = ApplicationSettings::default();
        for rule in &self.applications {
            if rule.matches(identity) {
                settings.merge(rule.settings);
            }
        }
        settings
    }
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut pattern_index, mut value_index) = (0, 0);
    let (mut star, mut retry_value) = (None, 0);
    while value_index < value.len() {
        if pattern.get(pattern_index) == Some(&b'?')
            || pattern
                .get(pattern_index)
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&value[value_index]))
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern.get(pattern_index) == Some(&b'*') {
            star = Some(pattern_index);
            pattern_index += 1;
            retry_value = value_index;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            retry_value += 1;
            value_index = retry_value;
        } else {
            return false;
        }
    }
    pattern[pattern_index..].iter().all(|byte| *byte == b'*')
}

/// Named policy workspaces.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Ordered names; the number of names is the workspace count.
    pub names: Vec<String>,
    /// Grid columns; zero derives a single row from the workspace count.
    pub columns: u32,
    /// Wrap directional navigation at grid edges.
    pub wrap: bool,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            names: ["1", "2", "3", "4"].map(str::to_owned).to_vec(),
            columns: 0,
            wrap: true,
        }
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

/// Server-side decoration settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    /// Border width in pixels.
    pub border_width: u32,
    /// Titlebar height in pixels; zero disables the titlebar.
    pub titlebar_height: u32,
    /// Focused-client border color.
    pub active_border: RgbColor,
    /// Unfocused-client border color.
    pub inactive_border: RgbColor,
    /// Urgent-client border color.
    pub urgent_border: RgbColor,
    /// Focused-client titlebar color.
    pub active_titlebar: RgbColor,
    /// Unfocused-client titlebar color.
    pub inactive_titlebar: RgbColor,
    /// Urgent-client titlebar color.
    pub urgent_titlebar: RgbColor,
    /// Title text color.
    pub title_text: RgbColor,
    /// Minimize-button color.
    pub minimize_button: RgbColor,
    /// Maximize-button color.
    pub maximize_button: RgbColor,
    /// Close-button color.
    pub close_button: RgbColor,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            border_width: 2,
            titlebar_height: 24,
            active_border: RgbColor::new(0x8a, 0xad, 0xf4),
            inactive_border: RgbColor::new(0x49, 0x4d, 0x64),
            urgent_border: RgbColor::new(0xed, 0x87, 0x96),
            active_titlebar: RgbColor::new(0x36, 0x39, 0x4f),
            inactive_titlebar: RgbColor::new(0x24, 0x27, 0x3a),
            urgent_titlebar: RgbColor::new(0x5b, 0x30, 0x3b),
            title_text: RgbColor::new(0xca, 0xd3, 0xf5),
            minimize_button: RgbColor::new(0xee, 0xd4, 0x9f),
            maximize_button: RgbColor::new(0xa6, 0xda, 0x95),
            close_button: RgbColor::new(0xed, 0x87, 0x96),
        }
    }
}

/// Mouse-driven window-management bindings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MouseConfig {
    /// Modifier held for window-management drags.
    pub modifier: MouseModifier,
    /// Button used to move a client.
    pub move_button: u8,
    /// Button used to resize a client from its bottom-right corner.
    pub resize_button: u8,
    /// Distance in pixels at which move and resize edges snap to the work area.
    pub edge_resistance: u32,
}

impl Default for MouseConfig {
    fn default() -> Self {
        Self {
            modifier: MouseModifier::Super,
            move_button: 1,
            resize_button: 3,
            edge_resistance: 10,
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
                    key: KeyChord::new([KeyboardModifier::Alt], "Tab"),
                    action: Action::NextWindow,
                },
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Alt, KeyboardModifier::Shift], "Tab"),
                    action: Action::PreviousWindow,
                },
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
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Super], "Left"),
                    action: Action::WorkspaceLeft,
                },
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Super], "Right"),
                    action: Action::WorkspaceRight,
                },
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Super], "Up"),
                    action: Action::WorkspaceUp,
                },
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Super], "Down"),
                    action: Action::WorkspaceDown,
                },
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Left"),
                    action: Action::MoveToWorkspaceLeft { follow: false },
                },
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Right"),
                    action: Action::MoveToWorkspaceRight { follow: false },
                },
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Up"),
                    action: Action::MoveToWorkspaceUp { follow: false },
                },
                KeyBinding {
                    key: KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Down"),
                    action: Action::MoveToWorkspaceDown { follow: false },
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
    /// Focus the next client in the current most-recently-used cycle.
    NextWindow,
    /// Focus the previous client in the current most-recently-used cycle.
    PreviousWindow,
    /// Switch to the previous workspace, wrapping at the first.
    PreviousWorkspace,
    /// Switch to the next workspace, wrapping at the last.
    NextWorkspace,
    /// Switch to the workspace geometrically left in the active layout.
    WorkspaceLeft,
    /// Switch to the workspace geometrically right in the active layout.
    WorkspaceRight,
    /// Switch to the workspace geometrically above in the active layout.
    WorkspaceUp,
    /// Switch to the workspace geometrically below in the active layout.
    WorkspaceDown,
    /// Switch to a one-based configured workspace.
    SwitchWorkspace {
        /// One-based workspace number used in user configuration.
        workspace: u32,
    },
    /// Move the focused client to a one-based configured workspace.
    MoveToWorkspace {
        /// One-based workspace number used in user configuration.
        workspace: u32,
        /// Switch to the destination after moving the client.
        #[serde(default)]
        follow: bool,
    },
    /// Move the focused client to the previous workspace.
    MoveToPreviousWorkspace {
        /// Switch to the destination after moving the client.
        #[serde(default)]
        follow: bool,
    },
    /// Move the focused client to the next workspace.
    MoveToNextWorkspace {
        /// Switch to the destination after moving the client.
        #[serde(default)]
        follow: bool,
    },
    /// Move the focused client left in the active workspace layout.
    MoveToWorkspaceLeft {
        /// Switch to the destination after moving the client.
        #[serde(default)]
        follow: bool,
    },
    /// Move the focused client right in the active workspace layout.
    MoveToWorkspaceRight {
        /// Switch to the destination after moving the client.
        #[serde(default)]
        follow: bool,
    },
    /// Move the focused client upward in the active workspace layout.
    MoveToWorkspaceUp {
        /// Switch to the destination after moving the client.
        #[serde(default)]
        follow: bool,
    },
    /// Move the focused client downward in the active workspace layout.
    MoveToWorkspaceDown {
        /// Switch to the destination after moving the client.
        #[serde(default)]
        follow: bool,
    },
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
    /// Prevent a large resistance zone from making pointer operations unusable.
    #[error("mouse edge resistance {0} exceeds the maximum of 256 pixels")]
    EdgeResistanceTooStrong(u32),
    /// Prevent accidental unusable decoration.
    #[error("border width {0} exceeds the maximum of 64 pixels")]
    BorderTooWide(u32),
    /// Prevent accidental unusable titlebars.
    #[error("titlebar height {0} exceeds the maximum of 128 pixels")]
    TitlebarTooTall(u32),
    /// At least one workspace must remain available.
    #[error("at least one workspace name is required")]
    NoWorkspaces,
    /// Keep the policy state and EWMH properties at a practical size.
    #[error("workspace count {0} exceeds the maximum of 32")]
    TooManyWorkspaces(usize),
    /// A configured grid cannot have more columns than workspaces.
    #[error("workspace columns {columns} exceeds workspace count {count}")]
    TooManyWorkspaceColumns {
        /// Invalid column count.
        columns: u32,
        /// Configured workspace count.
        count: usize,
    },
    /// Workspace names must be visible and EWMH-safe.
    #[error("workspace {0} must have a non-empty name without NUL characters")]
    InvalidWorkspaceName(usize),
    /// The same chord appeared more than once.
    #[error("duplicate keyboard binding for {0}")]
    DuplicateKeyBinding(String),
    /// Execute actions must contain a command.
    #[error("execute action for {0} has an empty command")]
    EmptyCommand(String),
    /// A binding references a workspace outside the configured set.
    #[error("keyboard binding for {key} references workspace {workspace}, but count is {count}")]
    InvalidWorkspaceBinding {
        /// Canonical key chord.
        key: String,
        /// Invalid one-based workspace number.
        workspace: u32,
        /// Configured workspace count.
        count: usize,
    },
    /// Rules without a matcher would unintentionally affect every client.
    #[error("application rule {0} must contain at least one match field")]
    EmptyApplicationMatcher(usize),
    /// Empty patterns are ambiguous and almost always accidental.
    #[error("application rule {0} contains an empty match pattern")]
    EmptyApplicationPattern(usize),
    /// An application rule references a workspace outside the configured set.
    #[error("application rule {rule} references workspace {workspace}, but count is {count}")]
    InvalidApplicationWorkspace {
        /// One-based rule position.
        rule: usize,
        /// Invalid one-based workspace.
        workspace: u32,
        /// Configured workspace count.
        count: usize,
    },
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
    fn excessive_edge_resistance_is_rejected() {
        let error = Config::parse("[mouse]\nedge_resistance = 257")
            .expect_err("oversized resistance must fail");
        assert!(matches!(error, ConfigError::EdgeResistanceTooStrong(257)));
    }

    #[test]
    fn colors_require_six_hex_digits() {
        assert_eq!("#12aBcF".parse::<RgbColor>(), Ok(RgbColor(0x12_ab_cf)));
        assert!("blue".parse::<RgbColor>().is_err());
    }

    #[test]
    fn excessive_titlebar_height_is_rejected() {
        let error = Config::parse("[theme]\ntitlebar_height = 129")
            .expect_err("oversized titlebar must fail");
        assert!(matches!(error, ConfigError::TitlebarTooTall(129)));
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

    #[test]
    fn workspace_names_define_the_valid_action_range() {
        let valid = Config::parse(
            "[workspaces]\nnames = ['main', 'chat']\n\
             [[keyboard.bindings]]\nkey = 'W-2'\n\
             action = { type = 'switch_workspace', workspace = 2 }\n\
             [[keyboard.bindings]]\nkey = 'W-S-1'\n\
             action = { type = 'move_to_workspace', workspace = 1, follow = true }",
        )
        .expect("valid workspace bindings");
        assert_eq!(valid.workspaces.names, ["main", "chat"]);

        let error = Config::parse(
            "[workspaces]\nnames = ['only']\n\
             [[keyboard.bindings]]\nkey = 'W-2'\n\
             action = { type = 'switch_workspace', workspace = 2 }",
        )
        .expect_err("out-of-range workspace must fail");
        assert!(matches!(
            error,
            ConfigError::InvalidWorkspaceBinding { workspace: 2, .. }
        ));
    }

    #[test]
    fn workspace_names_must_be_nonempty_and_ewmh_safe() {
        for source in [
            "[workspaces]\nnames = []",
            "[workspaces]\nnames = ['main', '  ']",
            "[workspaces]\nnames = [\"main\\u0000hidden\"]",
        ] {
            assert!(Config::parse(source).is_err());
        }
    }

    #[test]
    fn workspace_grid_rejects_more_columns_than_workspaces() {
        let config = Config::parse(
            "[workspaces]\nnames = ['code', 'web', 'chat', 'misc']\ncolumns = 2\nwrap = false",
        )
        .expect("valid two-column grid");
        assert_eq!(config.workspaces.columns, 2);
        assert!(!config.workspaces.wrap);

        let error = Config::parse("[workspaces]\nnames = ['one', 'two']\ncolumns = 3")
            .expect_err("oversized grid must fail");
        assert!(matches!(
            error,
            ConfigError::TooManyWorkspaceColumns {
                columns: 3,
                count: 2
            }
        ));
    }

    #[test]
    fn application_rules_match_wildcards_and_merge_in_order() {
        let config = Config::parse(
            "[[applications]]\n\
             match = { class = 'Fire*', kind = 'normal' }\n\
             workspace = 2\nlayer = 'below'\nfocus = false\n\
             [[applications]]\n\
             match = { name = 'Navigator', title = '*Private?' }\n\
             layer = 'above'\ndecorated = false",
        )
        .expect("valid application rules");
        let settings = config.application_settings(ApplicationIdentity {
            name: "navigator",
            class: "Firefox",
            role: "browser",
            title: "Private1",
            kind: ApplicationKind::Normal,
        });
        assert_eq!(settings.workspace, Some(2));
        assert_eq!(settings.layer, Some(ApplicationLayer::Above));
        assert_eq!(settings.decorated, Some(false));
        assert_eq!(settings.focus, Some(false));
    }

    #[test]
    fn application_rules_reject_empty_matchers_patterns_and_workspaces() {
        let empty = Config::parse("[[applications]]\nmatch = {}\nfocus = false")
            .expect_err("empty matcher must fail");
        assert!(matches!(empty, ConfigError::EmptyApplicationMatcher(1)));

        let pattern = Config::parse("[[applications]]\nmatch = { class = '' }")
            .expect_err("empty pattern must fail");
        assert!(matches!(pattern, ConfigError::EmptyApplicationPattern(1)));

        let workspace = Config::parse(
            "[workspaces]\nnames = ['one']\n\
             [[applications]]\nmatch = { class = '*' }\nworkspace = 2",
        )
        .expect_err("invalid rule workspace must fail");
        assert!(matches!(
            workspace,
            ConfigError::InvalidApplicationWorkspace { workspace: 2, .. }
        ));
    }
}
