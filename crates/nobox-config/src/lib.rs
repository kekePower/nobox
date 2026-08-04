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
    /// On-screen focus-cycle presentation.
    pub switcher: SwitcherConfig,
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
        if !(160..=1_024).contains(&self.switcher.width) {
            return Err(ConfigError::InvalidSwitcherWidth(self.switcher.width));
        }
        if !(16..=64).contains(&self.switcher.row_height) {
            return Err(ConfigError::InvalidSwitcherRowHeight(
                self.switcher.row_height,
            ));
        }
        if !(1..=32).contains(&self.switcher.max_rows) {
            return Err(ConfigError::InvalidSwitcherRows(self.switcher.max_rows));
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
        if self.mouse.drag_threshold > 256 {
            return Err(ConfigError::DragThresholdTooLarge(
                self.mouse.drag_threshold,
            ));
        }
        if !(100..=2_000).contains(&self.mouse.double_click_ms) {
            return Err(ConfigError::InvalidDoubleClickTime(
                self.mouse.double_click_ms,
            ));
        }
        if self.mouse.bindings.len() > 256 {
            return Err(ConfigError::TooManyMouseBindings(self.mouse.bindings.len()));
        }
        let mut mouse_bindings = BTreeSet::new();
        for binding in &self.mouse.bindings {
            if binding.actions.len() > 16 {
                return Err(ConfigError::TooManyMouseBindingActions {
                    binding: binding.to_string(),
                    count: binding.actions.len(),
                });
            }
            let identity = (binding.context, binding.button.clone(), binding.trigger);
            if !mouse_bindings.insert(identity) {
                return Err(ConfigError::DuplicateMouseBinding(binding.to_string()));
            }
            for action in &binding.actions {
                self.validate_action(action, binding.to_string())?;
            }
        }
        if !(100..=60_000).contains(&self.keyboard.chain_timeout_ms) {
            return Err(ConfigError::InvalidChainTimeout(
                self.keyboard.chain_timeout_ms,
            ));
        }
        if self.keyboard.bindings.len() > 256 {
            return Err(ConfigError::TooManyKeyBindings(
                self.keyboard.bindings.len(),
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
        let mut bindings = BTreeSet::<KeySequence>::new();
        for binding in &self.keyboard.bindings {
            if binding.key.chords().len() > 8 {
                return Err(ConfigError::KeySequenceTooLong {
                    key: binding.key.to_string(),
                    length: binding.key.chords().len(),
                });
            }
            if binding.actions.len() > 16 {
                return Err(ConfigError::TooManyBindingActions {
                    key: binding.key.to_string(),
                    count: binding.actions.len(),
                });
            }
            if binding
                .key
                .chords()
                .iter()
                .skip(1)
                .any(|chord| chord == &self.keyboard.chain_quit_key)
            {
                return Err(ConfigError::ConflictingChainQuitKey {
                    key: self.keyboard.chain_quit_key.to_string(),
                    binding: binding.key.to_string(),
                });
            }
            if let Some(prefix) = bindings.iter().find(|configured| {
                configured.is_strict_prefix_of(&binding.key)
                    || binding.key.is_strict_prefix_of(configured)
            }) {
                return Err(ConfigError::ConflictingKeyBinding {
                    first: prefix.to_string(),
                    second: binding.key.to_string(),
                });
            }
            if !bindings.insert(binding.key.clone()) {
                return Err(ConfigError::DuplicateKeyBinding(binding.key.to_string()));
            }
            for action in &binding.actions {
                if matches!(action, Action::Move | Action::Resize) {
                    return Err(ConfigError::PointerActionInKeyBinding {
                        key: binding.key.to_string(),
                        action: match action {
                            Action::Move => "move",
                            Action::Resize => "resize",
                            _ => unreachable!(),
                        },
                    });
                }
                self.validate_action(action, binding.key.to_string())?;
            }
        }
        Ok(())
    }

    fn validate_action(&self, action: &Action, binding: String) -> Result<(), ConfigError> {
        if let Action::Execute { command } = action
            && command.trim().is_empty()
        {
            return Err(ConfigError::EmptyCommand(binding));
        }
        let workspace = match action {
            Action::SwitchWorkspace { workspace } | Action::MoveToWorkspace { workspace, .. } => {
                Some(*workspace)
            }
            Action::Execute { .. }
            | Action::Close
            | Action::Focus
            | Action::Raise
            | Action::Lower
            | Action::Minimize
            | Action::ToggleMaximize
            | Action::Move
            | Action::Resize
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
                key: binding,
                workspace: workspace.unwrap_or_default(),
                count: self.workspaces.names.len(),
            });
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

/// Lightweight on-screen focus-cycle list.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SwitcherConfig {
    /// Show the list while a modifier-held focus cycle is active.
    pub enabled: bool,
    /// Preferred width in pixels, clamped to the selected output.
    pub width: u32,
    /// Height of each title row in pixels.
    pub row_height: u32,
    /// Maximum visible rows before the list follows the selection.
    pub max_rows: u32,
}

impl Default for SwitcherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            width: 420,
            row_height: 28,
            max_rows: 8,
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
    /// Pointer movement required before a drag binding fires.
    pub drag_threshold: u32,
    /// Maximum delay between clicks recognized as a double click.
    pub double_click_ms: u32,
    /// Ordered context-aware pointer bindings.
    pub bindings: Vec<MouseBinding>,
}

impl Default for MouseConfig {
    fn default() -> Self {
        Self {
            modifier: MouseModifier::Super,
            move_button: 1,
            resize_button: 3,
            edge_resistance: 10,
            drag_threshold: 8,
            double_click_ms: 500,
            bindings: vec![
                MouseBinding::single(
                    MouseContext::Client,
                    MouseChord::new([], MouseButton::Left),
                    MouseTrigger::Press,
                    Action::Focus,
                ),
                MouseBinding::single(
                    MouseContext::Client,
                    MouseChord::new([], MouseButton::Left),
                    MouseTrigger::Click,
                    Action::Raise,
                ),
                MouseBinding::single(
                    MouseContext::Titlebar,
                    MouseChord::new([], MouseButton::Left),
                    MouseTrigger::Press,
                    Action::Focus,
                ),
                MouseBinding::single(
                    MouseContext::Titlebar,
                    MouseChord::new([], MouseButton::Left),
                    MouseTrigger::Click,
                    Action::Raise,
                ),
                MouseBinding::single(
                    MouseContext::Titlebar,
                    MouseChord::new([], MouseButton::Left),
                    MouseTrigger::Drag,
                    Action::Move,
                ),
                MouseBinding::single(
                    MouseContext::Titlebar,
                    MouseChord::new([], MouseButton::Left),
                    MouseTrigger::DoubleClick,
                    Action::ToggleMaximize,
                ),
                MouseBinding::single(
                    MouseContext::Titlebar,
                    MouseChord::new([], MouseButton::Middle),
                    MouseTrigger::Click,
                    Action::Lower,
                ),
                MouseBinding::single(
                    MouseContext::Border,
                    MouseChord::new([], MouseButton::Left),
                    MouseTrigger::Drag,
                    Action::Resize,
                ),
                MouseBinding::single(
                    MouseContext::Minimize,
                    MouseChord::new([], MouseButton::Left),
                    MouseTrigger::Click,
                    Action::Minimize,
                ),
                MouseBinding::single(
                    MouseContext::Maximize,
                    MouseChord::new([], MouseButton::Left),
                    MouseTrigger::Click,
                    Action::ToggleMaximize,
                ),
                MouseBinding::single(
                    MouseContext::Close,
                    MouseChord::new([], MouseButton::Left),
                    MouseTrigger::Click,
                    Action::Close,
                ),
                MouseBinding::single(
                    MouseContext::Desktop,
                    MouseChord::new([], MouseButton::Up),
                    MouseTrigger::Click,
                    Action::PreviousWorkspace,
                ),
                MouseBinding::single(
                    MouseContext::Desktop,
                    MouseChord::new([], MouseButton::Down),
                    MouseTrigger::Click,
                    Action::NextWorkspace,
                ),
            ],
        }
    }
}

/// One context-aware pointer binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MouseBinding {
    /// Decoration or surface context where the gesture begins.
    pub context: MouseContext,
    /// Modifier and physical button chord.
    pub button: MouseChord,
    /// Gesture phase that dispatches the actions.
    pub trigger: MouseTrigger,
    /// Ordered actions dispatched for the gesture.
    pub actions: Vec<Action>,
}

impl MouseBinding {
    fn new(
        context: MouseContext,
        button: MouseChord,
        trigger: MouseTrigger,
        actions: impl IntoIterator<Item = Action>,
    ) -> Self {
        Self {
            context,
            button,
            trigger,
            actions: actions.into_iter().collect(),
        }
    }

    fn single(
        context: MouseContext,
        button: MouseChord,
        trigger: MouseTrigger,
        action: Action,
    ) -> Self {
        Self::new(context, button, trigger, [action])
    }
}

impl std::fmt::Display for MouseBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} {} {}",
            self.context.as_str(),
            self.button,
            self.trigger.as_str()
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMouseBinding {
    context: MouseContext,
    button: MouseChord,
    trigger: MouseTrigger,
    #[serde(default)]
    action: Option<Action>,
    #[serde(default)]
    actions: Vec<Action>,
}

impl<'de> Deserialize<'de> for MouseBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawMouseBinding::deserialize(deserializer)?;
        let actions = deserialize_binding_actions(raw.action, raw.actions, "mouse")?;
        Ok(Self {
            context: raw.context,
            button: raw.button,
            trigger: raw.trigger,
            actions,
        })
    }
}

/// Global keyboard bindings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct KeyboardConfig {
    /// Chord that cancels an active key sequence.
    pub chain_quit_key: KeyChord,
    /// Milliseconds before an incomplete key sequence is cancelled.
    pub chain_timeout_ms: u32,
    /// Ordered key-to-action mappings.
    pub bindings: Vec<KeyBinding>,
}

impl Default for KeyboardConfig {
    fn default() -> Self {
        Self {
            chain_quit_key: KeyChord::new([KeyboardModifier::Control], "g"),
            chain_timeout_ms: 3_000,
            bindings: vec![
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Alt], "Tab"),
                    Action::NextWindow,
                ),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Alt, KeyboardModifier::Shift], "Tab"),
                    Action::PreviousWindow,
                ),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Super], "Return"),
                    Action::Execute {
                        command: "xterm".to_owned(),
                    },
                ),
                KeyBinding::single(KeyChord::new([KeyboardModifier::Super], "q"), Action::Close),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Escape"),
                    Action::Exit,
                ),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Super], "Left"),
                    Action::WorkspaceLeft,
                ),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Super], "Right"),
                    Action::WorkspaceRight,
                ),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Super], "Up"),
                    Action::WorkspaceUp,
                ),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Super], "Down"),
                    Action::WorkspaceDown,
                ),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Left"),
                    Action::MoveToWorkspaceLeft { follow: false },
                ),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Right"),
                    Action::MoveToWorkspaceRight { follow: false },
                ),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Up"),
                    Action::MoveToWorkspaceUp { follow: false },
                ),
                KeyBinding::single(
                    KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Down"),
                    Action::MoveToWorkspaceDown { follow: false },
                ),
            ],
        }
    }
}

/// One global keyboard binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyBinding {
    /// Key sequence such as `W-Return` or `C-x C-s`.
    pub key: KeySequence,
    /// Ordered actions executed when the complete sequence is pressed.
    pub actions: Vec<Action>,
}

impl KeyBinding {
    fn single(key: KeyChord, action: Action) -> Self {
        Self {
            key: KeySequence::new([key]),
            actions: vec![action],
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKeyBinding {
    key: KeySequence,
    #[serde(default)]
    action: Option<Action>,
    #[serde(default)]
    actions: Vec<Action>,
}

impl<'de> Deserialize<'de> for KeyBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawKeyBinding::deserialize(deserializer)?;
        let actions = deserialize_binding_actions(raw.action, raw.actions, "keyboard")?;
        Ok(Self {
            key: raw.key,
            actions,
        })
    }
}

fn deserialize_binding_actions<E>(
    action: Option<Action>,
    actions: Vec<Action>,
    kind: &str,
) -> Result<Vec<Action>, E>
where
    E: serde::de::Error,
{
    match (action, actions.is_empty()) {
        (Some(action), true) => Ok(vec![action]),
        (None, false) => Ok(actions),
        (Some(_), false) => Err(E::custom(format_args!(
            "{kind} binding must use action or actions, not both"
        ))),
        (None, true) => Err(E::custom(format_args!(
            "{kind} binding requires at least one action"
        ))),
    }
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
    /// Focus the action target.
    Focus,
    /// Raise the action target within its policy layer.
    Raise,
    /// Lower the action target within its policy layer.
    Lower,
    /// Minimize the action target through the shared iconic lifecycle.
    Minimize,
    /// Toggle both maximize axes on the action target.
    ToggleMaximize,
    /// Start an interactive move from the triggering pointer gesture.
    Move,
    /// Start an interactive resize from the triggering pointer gesture.
    Resize,
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

/// One or more key chords pressed in order.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct KeySequence {
    chords: Vec<KeyChord>,
}

impl KeySequence {
    fn new(chords: impl IntoIterator<Item = KeyChord>) -> Self {
        Self {
            chords: chords.into_iter().collect(),
        }
    }

    /// Returns the ordered chords in this sequence.
    #[must_use]
    pub fn chords(&self) -> &[KeyChord] {
        &self.chords
    }

    fn is_strict_prefix_of(&self, other: &Self) -> bool {
        self.chords.len() < other.chords.len() && other.chords.starts_with(&self.chords)
    }
}

impl std::fmt::Display for KeySequence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, chord) in self.chords.iter().enumerate() {
            if index > 0 {
                formatter.write_str(" ")?;
            }
            write!(formatter, "{chord}")?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for KeySequence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl FromStr for KeySequence {
    type Err = KeySequenceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let chords = value
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| KeySequenceError(value.to_owned()))?;
        if chords.is_empty() {
            return Err(KeySequenceError(value.to_owned()));
        }
        Ok(Self { chords })
    }
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
        if value.trim() != value || value.chars().any(char::is_whitespace) {
            return Err(KeyChordError(value.to_owned()));
        }
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

/// Parsed modifier and physical pointer-button chord.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MouseChord {
    modifiers: Vec<KeyboardModifier>,
    button: MouseButton,
}

impl MouseChord {
    fn new(modifiers: impl IntoIterator<Item = KeyboardModifier>, button: MouseButton) -> Self {
        let mut modifiers = modifiers.into_iter().collect::<Vec<_>>();
        modifiers.sort_unstable();
        modifiers.dedup();
        Self { modifiers, button }
    }

    /// Returns the chord's modifiers in canonical order.
    #[must_use]
    pub fn modifiers(&self) -> &[KeyboardModifier] {
        &self.modifiers
    }

    /// Returns the physical pointer button.
    #[must_use]
    pub const fn button(&self) -> MouseButton {
        self.button
    }
}

impl std::fmt::Display for MouseChord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for modifier in &self.modifiers {
            write!(formatter, "{}-", modifier.short_name())?;
        }
        formatter.write_str(self.button.as_str())
    }
}

impl<'de> Deserialize<'de> for MouseChord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl FromStr for MouseChord {
    type Err = MouseChordError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim() != value || value.chars().any(char::is_whitespace) {
            return Err(MouseChordError(value.to_owned()));
        }
        let parts = value.split('-').collect::<Vec<_>>();
        let Some((button_name, modifier_names)) = parts.split_last() else {
            return Err(MouseChordError(value.to_owned()));
        };
        let button = button_name
            .parse()
            .map_err(|_| MouseChordError(value.to_owned()))?;
        let mut modifiers = Vec::with_capacity(modifier_names.len());
        for name in modifier_names {
            let modifier = match name.to_ascii_lowercase().as_str() {
                "c" | "ctrl" | "control" => KeyboardModifier::Control,
                "a" | "alt" => KeyboardModifier::Alt,
                "s" | "shift" => KeyboardModifier::Shift,
                "w" | "super" => KeyboardModifier::Super,
                _ => return Err(MouseChordError(value.to_owned())),
            };
            if modifiers.contains(&modifier) {
                return Err(MouseChordError(value.to_owned()));
            }
            modifiers.push(modifier);
        }
        Ok(Self::new(modifiers, button))
    }
}

/// Conventional X11 pointer button used by a binding.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MouseButton {
    /// Primary/left button (X11 button 1).
    Left,
    /// Middle button (X11 button 2).
    Middle,
    /// Secondary/right button (X11 button 3).
    Right,
    /// Wheel up (X11 button 4).
    Up,
    /// Wheel down (X11 button 5).
    Down,
}

impl MouseButton {
    /// Returns the X11-compatible button number.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Middle => 2,
            Self::Right => 3,
            Self::Up => 4,
            Self::Down => 5,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Middle => "Middle",
            Self::Right => "Right",
            Self::Up => "Up",
            Self::Down => "Down",
        }
    }
}

impl FromStr for MouseButton {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "left" | "button1" | "1" => Ok(Self::Left),
            "middle" | "button2" | "2" => Ok(Self::Middle),
            "right" | "button3" | "3" => Ok(Self::Right),
            "up" | "button4" | "4" => Ok(Self::Up),
            "down" | "button5" | "5" => Ok(Self::Down),
            _ => Err(()),
        }
    }
}

/// Pointer location used to select a binding.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum MouseContext {
    /// Root window outside managed desktop surfaces.
    Root,
    /// Desktop surface, including desktop-role clients.
    Desktop,
    /// Application-owned client content.
    Client,
    /// Any managed decoration, used as a context fallback.
    Frame,
    /// Titlebar excluding its buttons.
    Titlebar,
    /// Any resize border, used as a context fallback.
    Border,
    /// Top resize border.
    Top,
    /// Bottom resize border.
    Bottom,
    /// Left resize border.
    Left,
    /// Right resize border.
    Right,
    /// Top-left resize corner.
    TopLeft,
    /// Top-right resize corner.
    TopRight,
    /// Bottom-left resize corner.
    BottomLeft,
    /// Bottom-right resize corner.
    BottomRight,
    /// Minimize titlebar button.
    Minimize,
    /// Maximize titlebar button.
    Maximize,
    /// Close titlebar button.
    Close,
}

impl MouseContext {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Desktop => "desktop",
            Self::Client => "client",
            Self::Frame => "frame",
            Self::Titlebar => "titlebar",
            Self::Border => "border",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Right => "right",
            Self::TopLeft => "top_left",
            Self::TopRight => "top_right",
            Self::BottomLeft => "bottom_left",
            Self::BottomRight => "bottom_right",
            Self::Minimize => "minimize",
            Self::Maximize => "maximize",
            Self::Close => "close",
        }
    }
}

/// Gesture phase that dispatches a pointer binding.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum MouseTrigger {
    /// Initial button press.
    Press,
    /// Matching button release.
    Release,
    /// Press and release without crossing the drag threshold.
    Click,
    /// Second nearby click within the configured time.
    DoubleClick,
    /// Movement beyond the configured threshold while the button is held.
    Drag,
}

impl MouseTrigger {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Press => "press",
            Self::Release => "release",
            Self::Click => "click",
            Self::DoubleClick => "double_click",
            Self::Drag => "drag",
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
    /// Keep the switcher readable without allowing oversized X11 requests.
    #[error("focus switcher width {0}px is outside 160..=1024px")]
    InvalidSwitcherWidth(u32),
    /// Keep rows readable and bound popup geometry.
    #[error("focus switcher row height {0}px is outside 16..=64px")]
    InvalidSwitcherRowHeight(u32),
    /// Bound rendering work and popup height.
    #[error("focus switcher row count {0} is outside 1..=32")]
    InvalidSwitcherRows(u32),
    /// Move and resize cannot use the same button.
    #[error("move_button and resize_button both use button {0}")]
    SameMouseButton(u8),
    /// X11 only has five conventional pointer buttons for these bindings.
    #[error("mouse button {0} is outside the supported range 1..=5")]
    InvalidMouseButton(u8),
    /// Prevent a large resistance zone from making pointer operations unusable.
    #[error("mouse edge resistance {0} exceeds the maximum of 256 pixels")]
    EdgeResistanceTooStrong(u32),
    /// Keep gesture recognition responsive and arithmetic bounded.
    #[error("mouse drag threshold {0} exceeds the maximum of 256 pixels")]
    DragThresholdTooLarge(u32),
    /// Double-click recognition must remain useful and bounded.
    #[error("mouse double-click time {0}ms is outside 100..=2000ms")]
    InvalidDoubleClickTime(u32),
    /// Keep passive grabs and gesture lookup bounded.
    #[error("mouse binding count {0} exceeds the maximum of 256")]
    TooManyMouseBindings(usize),
    /// Keep ordered dispatch bounded for one pointer event.
    #[error("mouse binding {binding} has {count} actions; maximum is 16")]
    TooManyMouseBindingActions {
        /// Canonical context, chord, and trigger.
        binding: String,
        /// Configured action count.
        count: usize,
    },
    /// One gesture identity must have one unambiguous ordered action list.
    #[error("duplicate mouse binding for {0}")]
    DuplicateMouseBinding(String),
    /// Key-chain timeouts must be responsive without permitting overflow-prone values.
    #[error("keyboard chain timeout {0}ms is outside 100..=60000ms")]
    InvalidChainTimeout(u32),
    /// Keep passive grabs and the compiled input tree bounded.
    #[error("keyboard binding count {0} exceeds the maximum of 256")]
    TooManyKeyBindings(usize),
    /// Keep key-chain state and keycode expansion bounded.
    #[error("keyboard sequence {key} has {length} chords; maximum is 8")]
    KeySequenceTooLong {
        /// Canonical sequence.
        key: String,
        /// Configured chord count.
        length: usize,
    },
    /// Keep ordered dispatch bounded for one input event.
    #[error("keyboard binding {key} has {count} actions; maximum is 16")]
    TooManyBindingActions {
        /// Canonical sequence.
        key: String,
        /// Configured action count.
        count: usize,
    },
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
    /// The same key sequence appeared more than once.
    #[error("duplicate keyboard binding for {0}")]
    DuplicateKeyBinding(String),
    /// A complete binding cannot also be the prefix of another binding.
    #[error("keyboard binding {first} conflicts with prefixed binding {second}")]
    ConflictingKeyBinding {
        /// Complete sequence that is also a prefix.
        first: String,
        /// Conflicting sequence.
        second: String,
    },
    /// The chain quit chord must remain unambiguous while a sequence is active.
    #[error("keyboard chain quit key {key} conflicts with binding {binding}")]
    ConflictingChainQuitKey {
        /// Configured chain-cancellation chord.
        key: String,
        /// Sequence containing that chord after its prefix.
        binding: String,
    },
    /// Execute actions must contain a command.
    #[error("execute action for {0} has an empty command")]
    EmptyCommand(String),
    /// A binding references a workspace outside the configured set.
    #[error("binding {key} references workspace {workspace}, but count is {count}")]
    InvalidWorkspaceBinding {
        /// Canonical key chord.
        key: String,
        /// Invalid one-based workspace number.
        workspace: u32,
        /// Configured workspace count.
        count: usize,
    },
    /// Interactive pointer actions require press coordinates and a pointer target.
    #[error("keyboard binding {key} cannot use pointer-only {action} action")]
    PointerActionInKeyBinding {
        /// Canonical key sequence.
        key: String,
        /// Pointer-only action name.
        action: &'static str,
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

/// Error returned for malformed pointer-button chord syntax.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid mouse chord {0:?}; use modifiers C/A/S/W followed by Left/Middle/Right/Up/Down")]
pub struct MouseChordError(String);

/// Error returned for a malformed key-sequence string.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid key sequence {0:?}; separate key chords with spaces")]
pub struct KeySequenceError(String);

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
    fn mouse_chords_contexts_and_ordered_actions_are_typed() {
        let chord = "Control-W-Button3"
            .parse::<MouseChord>()
            .expect("valid pointer chord");
        assert_eq!(chord.to_string(), "C-W-Right");
        assert_eq!(chord.button(), MouseButton::Right);
        assert_eq!(
            chord.modifiers(),
            [KeyboardModifier::Control, KeyboardModifier::Super]
        );

        let config = Config::parse(
            "[[mouse.bindings]]\ncontext = 'bottom_right'\nbutton = 'W-Left'\n\
             trigger = 'drag'\nactions = [{ type = 'focus' }, { type = 'resize' }]\n\
             [[mouse.bindings]]\ncontext = 'root'\nbutton = 'Up'\ntrigger = 'click'\n\
             action = { type = 'previous_workspace' }",
        )
        .expect("valid context-aware pointer bindings");
        assert_eq!(config.mouse.bindings.len(), 2);
        assert_eq!(config.mouse.bindings[0].actions.len(), 2);
        assert_eq!(
            config.mouse.bindings[1].actions,
            [Action::PreviousWorkspace]
        );
    }

    #[test]
    fn ambiguous_or_unbounded_mouse_bindings_are_rejected() {
        assert!("W-W-Left".parse::<MouseChord>().is_err());
        let duplicate = Config::parse(
            "[[mouse.bindings]]\ncontext = 'titlebar'\nbutton = 'Left'\ntrigger = 'click'\n\
             action = { type = 'raise' }\n\
             [[mouse.bindings]]\ncontext = 'titlebar'\nbutton = 'Button1'\ntrigger = 'click'\n\
             action = { type = 'lower' }",
        )
        .expect_err("canonical duplicate pointer binding must fail");
        assert!(matches!(duplicate, ConfigError::DuplicateMouseBinding(_)));
        assert!(matches!(
            Config::parse("[mouse]\ndrag_threshold = 257"),
            Err(ConfigError::DragThresholdTooLarge(257))
        ));
        assert!(matches!(
            Config::parse("[mouse]\ndouble_click_ms = 99"),
            Err(ConfigError::InvalidDoubleClickTime(99))
        ));
        assert!(matches!(
            Config::parse("[[keyboard.bindings]]\nkey = 'W-m'\naction = { type = 'move' }"),
            Err(ConfigError::PointerActionInKeyBinding { .. })
        ));
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
    fn focus_switcher_geometry_is_bounded() {
        assert!(matches!(
            Config::parse("[switcher]\nwidth = 159"),
            Err(ConfigError::InvalidSwitcherWidth(159))
        ));
        assert!(matches!(
            Config::parse("[switcher]\nrow_height = 65"),
            Err(ConfigError::InvalidSwitcherRowHeight(65))
        ));
        assert!(matches!(
            Config::parse("[switcher]\nmax_rows = 0"),
            Err(ConfigError::InvalidSwitcherRows(0))
        ));
    }

    #[test]
    fn key_chords_are_parsed_and_canonicalized() {
        let chord = "Shift-W-Return".parse::<KeyChord>().expect("valid chord");
        assert_eq!(chord.to_string(), "S-W-Return");
        assert_eq!(
            chord.modifiers(),
            [KeyboardModifier::Shift, KeyboardModifier::Super]
        );
        let sequence = "Super-x   Ctrl-s"
            .parse::<KeySequence>()
            .expect("valid sequence");
        assert_eq!(sequence.to_string(), "W-x C-s");
        assert_eq!(sequence.chords().len(), 2);
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

        let conflict = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-x'\naction = { type = 'close' }\n\
             [[keyboard.bindings]]\nkey = 'W-x t'\naction = { type = 'exit' }",
        )
        .expect_err("leaf and prefix must conflict");
        assert!(matches!(
            conflict,
            ConfigError::ConflictingKeyBinding { .. }
        ));
    }

    #[test]
    fn ordered_keyboard_actions_preserve_legacy_singular_form() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-x t'\n\
             actions = [{ type = 'next_workspace' }, { type = 'close' }]\n\
             [[keyboard.bindings]]\nkey = 'W-c'\n\
             action = { type = 'close' }",
        )
        .expect("valid plural and legacy actions");
        assert_eq!(config.keyboard.bindings[0].actions.len(), 2);
        assert_eq!(config.keyboard.bindings[1].actions, [Action::Close]);

        for source in [
            "[[keyboard.bindings]]\nkey = 'W-x'",
            "[[keyboard.bindings]]\nkey = 'W-x'\nactions = []",
            "[[keyboard.bindings]]\nkey = 'W-x'\naction = { type = 'close' }\n\
             actions = [{ type = 'exit' }]",
        ] {
            assert!(Config::parse(source).is_err());
        }

        let quit_conflict = Config::parse(
            "[keyboard]\nchain_quit_key = 'C-g'\n\
             [[keyboard.bindings]]\nkey = 'W-x C-g'\naction = { type = 'close' }",
        )
        .expect_err("quit chord cannot also continue a chain");
        assert!(matches!(
            quit_conflict,
            ConfigError::ConflictingChainQuitKey { .. }
        ));
        assert!(matches!(
            Config::parse("[keyboard]\nchain_timeout_ms = 99"),
            Err(ConfigError::InvalidChainTimeout(99))
        ));
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
