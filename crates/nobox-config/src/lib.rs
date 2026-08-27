//! Loading, validation, discovery, and compatibility import for nobox configuration.

mod agent;
mod document;
mod openbox_theme;

pub use agent::{
    AgentConfig, AgentGrant, AgentLaunchConfig, AgentPolicy, AgentVisibility, GrantedCapability,
    LaunchPolicy, MAX_AGENT_GRANTS, MAX_AGENT_SOCKET_PATH, MAX_LAUNCH_ENTRIES, MAX_SUPPRESSION_MS,
};
pub use document::{ConfigDocument, ConfigDocumentError, SettingKey, SettingValue};
pub use openbox_theme::{OpenboxThemeImport, OpenboxThemeImportError};

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fmt, fs,
    num::NonZeroU32,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::Deserialize;
use thiserror::Error;

/// The configuration shipped with nobox.
pub const DEFAULT_CONFIG: &str = include_str!("../default.toml");

/// Maximum workspace count accepted by configuration and runtime actions.
pub const MAX_WORKSPACES: usize = 32;

/// Maximum UTF-8 output accepted from a command-backed menu.
pub const MAX_COMMAND_MENU_BYTES: usize = 65_536;

/// Maximum number of application launchers shown by the panel.
pub const MAX_PANEL_LAUNCHERS: usize = 32;

/// Maximum number of ordered components in the panel layout.
pub const MAX_PANEL_ITEMS: usize = 16;

/// Maximum number of persistent connector rules accepted from configuration.
pub const MAX_OUTPUTS: usize = 32;

/// Complete user configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Focus behavior.
    pub focus: FocusConfig,
    /// On-screen focus-cycle presentation.
    pub switcher: SwitcherConfig,
    /// Shared menu definitions and presentation bounds.
    pub menu: MenuConfig,
    /// Optional external panel presentation and components.
    pub panel: PanelConfig,
    /// Commands used by standard semantic actions.
    pub commands: CommandsConfig,
    /// User-facing shortcuts for common semantic actions.
    pub shortcuts: ShortcutsConfig,
    /// Initial window placement behavior.
    pub placement: PlacementConfig,
    /// User-reserved screen edges shared by every workspace.
    pub margins: MarginConfig,
    /// Display-server-neutral connector preferences used by direct backends.
    pub outputs: OutputsConfig,
    /// Native Wayland session helpers and privileged protocol owners.
    pub wayland: WaylandConfig,
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
    /// WM-mediated agent access, off unless explicitly enabled.
    pub agent: AgentConfig,
}

/// Native Wayland helper configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WaylandConfig {
    /// Whether the compositor may start its optional XWayland compatibility server.
    pub xwayland: bool,
    /// Absolute executable and arguments for the compositor-authorized input method.
    pub input_method: Vec<String>,
}

/// Shell commands behind standard launch, screenshot, and session actions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CommandsConfig {
    /// Preferred terminal command.
    pub terminal: String,
    /// Full-screen screenshot command.
    pub screenshot: String,
    /// Active-window screenshot command.
    pub window_screenshot: String,
    /// Optional external session/logout dialog command.
    pub session: String,
}

impl Default for CommandsConfig {
    fn default() -> Self {
        Self {
            terminal: "xterm".to_owned(),
            screenshot: "nobox-screenshot".to_owned(),
            window_screenshot: "nobox-screenshot -w".to_owned(),
            session: String::new(),
        }
    }
}

/// Editable shortcuts for the common command-backed actions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ShortcutsConfig {
    /// Additional traditional shortcut for the configured terminal.
    pub terminal: KeyChord,
    /// Shortcut for a full-screen screenshot.
    pub screenshot: KeyChord,
    /// Shortcut for an active-window screenshot.
    pub window_screenshot: KeyChord,
}

impl Default for ShortcutsConfig {
    fn default() -> Self {
        Self {
            terminal: KeyChord::new([KeyboardModifier::Control, KeyboardModifier::Alt], "t"),
            screenshot: KeyChord::new([], "Print"),
            window_screenshot: KeyChord::new([KeyboardModifier::Alt], "Print"),
        }
    }
}

/// Smart initial-placement behavior.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PlacementConfig {
    /// Center windows within the first completely free grid field.
    pub center_free_space: bool,
}

/// Screen edge used by the optional panel.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PanelPosition {
    /// Place the panel along the top screen edge.
    Top,
    /// Place the panel along the bottom screen edge.
    #[default]
    Bottom,
}

/// One component in the panel's left-to-right layout.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum PanelItem {
    /// Buttons for configured desktop-entry launchers.
    Launchers,
    /// Buttons for switching workspaces.
    Workspaces,
    /// Buttons for managed application windows.
    Tasks,
    /// Flexible space that pushes later components to the trailing edge.
    Spacer,
    /// Local time formatted with [`PanelConfig::clock_format`].
    Clock,
}

impl PanelItem {
    /// Stable TOML spelling used by Settings and documentation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Launchers => "launchers",
            Self::Workspaces => "workspaces",
            Self::Tasks => "tasks",
            Self::Spacer => "spacer",
            Self::Clock => "clock",
        }
    }
}

/// Which managed windows the panel task list includes.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PanelTaskScope {
    /// Only windows on the active workspace, plus sticky windows.
    #[default]
    CurrentWorkspace,
    /// Windows from every workspace.
    AllWorkspaces,
}

/// Configuration for the separate optional panel process.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PanelConfig {
    /// Start the panel with the nobox session.
    pub enabled: bool,
    /// Screen edge occupied by the panel.
    pub position: PanelPosition,
    /// Panel height in pixels.
    pub height: u32,
    /// Panel background color.
    pub background: RgbColor,
    /// Text color used by every panel component.
    pub foreground: RgbColor,
    /// Background for the active workspace and task.
    pub active_background: RgbColor,
    /// Background for applications requesting attention.
    pub urgent_background: RgbColor,
    /// Outer panel padding in pixels.
    pub padding: u32,
    /// Gap between adjacent panel components in pixels.
    pub spacing: u32,
    /// Maximum width of one task button in pixels.
    pub task_max_width: u32,
    /// Which workspaces contribute task buttons.
    pub task_scope: PanelTaskScope,
    /// Left-to-right component order; `spacer` consumes remaining room.
    pub items: Vec<PanelItem>,
    /// Ordered desktop-entry identifiers shown as launchers.
    pub launchers: Vec<String>,
    /// `strftime`-style local clock format.
    pub clock_format: String,
    /// Show buttons for switching workspaces.
    pub show_workspaces: bool,
    /// Show buttons for windows on the current workspace.
    pub show_tasks: bool,
    /// Show the local time at the trailing edge.
    pub show_clock: bool,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            position: PanelPosition::Bottom,
            height: 30,
            background: RgbColor::new(0x1f, 0x22, 0x28),
            foreground: RgbColor::new(0xf2, 0xf2, 0xf2),
            active_background: RgbColor::new(0x4c, 0x78, 0xa8),
            urgent_background: RgbColor::new(0xb8, 0x42, 0x42),
            padding: 4,
            spacing: 4,
            task_max_width: 220,
            task_scope: PanelTaskScope::CurrentWorkspace,
            items: vec![
                PanelItem::Launchers,
                PanelItem::Workspaces,
                PanelItem::Tasks,
                PanelItem::Spacer,
                PanelItem::Clock,
            ],
            launchers: Vec::new(),
            clock_format: "%H:%M".to_owned(),
            show_workspaces: true,
            show_tasks: true,
            show_clock: true,
        }
    }
}

impl Default for PlacementConfig {
    fn default() -> Self {
        Self {
            center_free_space: true,
        }
    }
}

impl Config {
    /// Resolves the standard shortcut policy and explicit keyboard overrides.
    #[must_use]
    pub fn effective_key_bindings(&self) -> Vec<KeyBinding> {
        self.keyboard.effective_bindings_with(&self.shortcuts)
    }

    /// Parses and validates TOML configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed TOML or an invalid combination of values.
    pub fn parse(source: &str) -> Result<Self, ConfigError> {
        let mut config: Self = toml::from_str(source)?;
        config.promote_legacy_standard_actions();
        config.validate()?;
        Ok(config)
    }

    fn promote_legacy_standard_actions(&mut self) {
        let Some(root) = self
            .menu
            .definitions
            .iter_mut()
            .find(|definition| definition.id == "root")
        else {
            return;
        };
        for entry in &mut root.entries {
            let MenuEntry::Item { label, actions } = entry else {
                continue;
            };
            if label == "_Terminal"
                && matches!(
                    actions.as_slice(),
                    [Action::Execute {
                        command,
                        prompt: None,
                        startup_notify: None,
                    }] if command == "xterm"
                )
            {
                *actions = vec![Action::LaunchTerminal];
            }
        }
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

    /// Parses command-menu TOML and validates it against this complete config.
    ///
    /// The document contains one `entries` array using the same strict entry and
    /// action schema as configured static menus. Submenus may reference existing
    /// named menus, and all normal action/resource bounds still apply.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized, malformed, empty, cyclic, or otherwise
    /// invalid generated content.
    pub fn parse_command_menu(
        &self,
        menu: &str,
        source: &str,
    ) -> Result<Vec<MenuEntry>, ConfigError> {
        if source.len() > MAX_COMMAND_MENU_BYTES {
            return Err(ConfigError::CommandMenuOutputTooLarge(source.len()));
        }
        let generated: CommandMenuDocument = toml::from_str(source)?;
        let mut candidate = self.clone();
        let index = candidate
            .menu
            .definitions
            .iter()
            .position(|definition| definition.id == menu)
            .ok_or_else(|| ConfigError::UnknownMenu {
                context: "command menu output".to_owned(),
                menu: menu.to_owned(),
            })?;
        let definition = &mut candidate.menu.definitions[index];
        definition.source = MenuSource::Static;
        definition.command = None;
        definition.entries = generated.entries;
        candidate.validate()?;
        Ok(std::mem::take(
            &mut candidate.menu.definitions[index].entries,
        ))
    }

    /// Validates relationships that serde cannot express.
    ///
    /// # Errors
    ///
    /// Returns an error when input or decoration values are unreasonable.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.wayland.input_method.len() > 32 {
            return Err(ConfigError::TooManyInputMethodArguments(
                self.wayland.input_method.len(),
            ));
        }
        if let Some(executable) = self.wayland.input_method.first()
            && (!Path::new(executable).is_absolute()
                || executable.contains('\0')
                || executable.len() > 4_096)
        {
            return Err(ConfigError::InvalidInputMethodExecutable);
        }
        if self
            .wayland
            .input_method
            .iter()
            .skip(1)
            .any(|argument| argument.contains('\0') || argument.len() > 4_096)
            || self
                .wayland
                .input_method
                .iter()
                .map(String::len)
                .sum::<usize>()
                > 16_384
        {
            return Err(ConfigError::InvalidInputMethodArguments);
        }
        for (name, command, required) in [
            ("terminal", self.commands.terminal.as_str(), true),
            ("screenshot", self.commands.screenshot.as_str(), true),
            (
                "window_screenshot",
                self.commands.window_screenshot.as_str(),
                true,
            ),
            ("session", self.commands.session.as_str(), false),
        ] {
            if (required && command.trim().is_empty())
                || command.contains('\0')
                || command.len() > 16_384
            {
                return Err(ConfigError::InvalidConfiguredCommand(name));
            }
        }
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
        self.validate_menus()?;
        if !(20..=96).contains(&self.panel.height) {
            return Err(ConfigError::InvalidPanelHeight(self.panel.height));
        }
        if self.panel.padding > 48 {
            return Err(ConfigError::InvalidPanelPadding(self.panel.padding));
        }
        if self.panel.spacing > 32 {
            return Err(ConfigError::InvalidPanelSpacing(self.panel.spacing));
        }
        if !(80..=512).contains(&self.panel.task_max_width) {
            return Err(ConfigError::InvalidPanelTaskWidth(
                self.panel.task_max_width,
            ));
        }
        if self.panel.items.is_empty() || self.panel.items.len() > MAX_PANEL_ITEMS {
            return Err(ConfigError::InvalidPanelItems(self.panel.items.len()));
        }
        let mut panel_items = BTreeSet::new();
        for item in &self.panel.items {
            if !panel_items.insert(*item) {
                return Err(ConfigError::DuplicatePanelItem(*item));
            }
        }
        if self.panel.clock_format.is_empty()
            || self.panel.clock_format.len() > 128
            || self.panel.clock_format.contains(['\0', '\n', '\r'])
            || chrono::format::StrftimeItems::new(&self.panel.clock_format)
                .any(|item| matches!(item, chrono::format::Item::Error))
        {
            return Err(ConfigError::InvalidPanelClockFormat);
        }
        if self.panel.launchers.len() > MAX_PANEL_LAUNCHERS {
            return Err(ConfigError::TooManyPanelLaunchers(
                self.panel.launchers.len(),
            ));
        }
        let mut launchers = BTreeSet::new();
        for launcher in &self.panel.launchers {
            if !agent::is_desktop_entry_id(launcher) {
                return Err(ConfigError::InvalidPanelLauncher(launcher.clone()));
            }
            if !launchers.insert(launcher) {
                return Err(ConfigError::DuplicatePanelLauncher(launcher.clone()));
            }
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
        if self.mouse.compatibility_modifiers.len() > 4 {
            return Err(ConfigError::TooManyMouseCompatibilityModifiers(
                self.mouse.compatibility_modifiers.len(),
            ));
        }
        if self.mouse.bindings.len() > 256 {
            return Err(ConfigError::TooManyMouseBindings(self.mouse.bindings.len()));
        }
        if self.mouse.disabled_bindings.len() > 256 {
            return Err(ConfigError::TooManyDisabledMouseBindings(
                self.mouse.disabled_bindings.len(),
            ));
        }
        let mut mouse_bindings = BTreeSet::new();
        for binding in &self.mouse.bindings {
            let identity = (binding.context, binding.button.clone(), binding.trigger);
            if !mouse_bindings.insert(identity) {
                return Err(ConfigError::DuplicateMouseBinding(binding.to_string()));
            }
        }
        let mut disabled_mouse_bindings = BTreeSet::new();
        for binding in &self.mouse.disabled_bindings {
            let identity = (binding.context, binding.button.clone(), binding.trigger);
            if !disabled_mouse_bindings.insert(identity) {
                return Err(ConfigError::DuplicateDisabledMouseBinding(
                    binding.to_string(),
                ));
            }
        }
        let effective_mouse_bindings = self.mouse.effective_bindings();
        if effective_mouse_bindings.len() > 256 {
            return Err(ConfigError::TooManyMouseBindings(
                effective_mouse_bindings.len(),
            ));
        }
        for binding in &effective_mouse_bindings {
            if binding.actions.len() > 16 {
                return Err(ConfigError::TooManyMouseBindingActions {
                    binding: binding.to_string(),
                    count: binding.actions.len(),
                });
            }
            for action in &binding.actions {
                self.validate_action(action, &|| binding.to_string())?;
            }
        }
        if !(100..=60_000).contains(&self.keyboard.chain_timeout_ms) {
            return Err(ConfigError::InvalidChainTimeout(
                self.keyboard.chain_timeout_ms,
            ));
        }
        for (field, value) in [
            ("model", self.keyboard.model.as_str()),
            ("layout", self.keyboard.layout.as_str()),
            ("variant", self.keyboard.variant.as_str()),
            ("options", self.keyboard.options.as_str()),
        ] {
            if value.len() > 255
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii() && !byte.is_ascii_control())
            {
                return Err(ConfigError::InvalidKeyboardXkbField(field));
            }
        }
        let effective_key_bindings = self.effective_key_bindings();
        if effective_key_bindings.len() > 256 {
            return Err(ConfigError::TooManyKeyBindings(
                effective_key_bindings.len(),
            ));
        }
        if self.keyboard.disabled_bindings.len() > 256 {
            return Err(ConfigError::TooManyDisabledKeyBindings(
                self.keyboard.disabled_bindings.len(),
            ));
        }
        if self.theme.border_width > 64 {
            return Err(ConfigError::BorderTooWide(self.theme.border_width));
        }
        if self.theme.titlebar_height > 128 {
            return Err(ConfigError::TitlebarTooTall(self.theme.titlebar_height));
        }
        if self.theme.font.trim().is_empty()
            || self.theme.font.len() > 255
            || !self
                .theme
                .font
                .bytes()
                .all(|byte| byte.is_ascii() && !byte.is_ascii_control())
        {
            return Err(ConfigError::InvalidThemeFont);
        }
        if self.theme.title_padding > 64 {
            return Err(ConfigError::TitlePaddingTooWide(self.theme.title_padding));
        }
        self.outputs.validate()?;
        if self.workspaces.names.is_empty() {
            return Err(ConfigError::NoWorkspaces);
        }
        if self.workspaces.names.len() > MAX_WORKSPACES {
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
        if self.workspaces.initial == 0
            || usize::try_from(self.workspaces.initial)
                .map_or(true, |initial| initial > self.workspaces.names.len())
        {
            return Err(ConfigError::InvalidInitialWorkspace {
                workspace: self.workspaces.initial,
                count: self.workspaces.names.len(),
            });
        }
        for (edge, pixels) in [
            ("top", self.margins.top),
            ("right", self.margins.right),
            ("bottom", self.margins.bottom),
            ("left", self.margins.left),
        ] {
            if pixels > 16_384 {
                return Err(ConfigError::MarginTooLarge { edge, pixels });
            }
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
                rule.matcher.group_name.as_deref(),
                rule.matcher.group_class.as_deref(),
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
            if let Some(ApplicationWorkspace::Index(workspace)) = rule.settings.workspace
                && usize::try_from(workspace.get())
                    .map_or(true, |workspace| workspace > self.workspaces.names.len())
            {
                return Err(ConfigError::InvalidApplicationWorkspace {
                    rule: index + 1,
                    workspace: workspace.get(),
                    count: self.workspaces.names.len(),
                });
            }
            if rule
                .settings
                .size
                .is_some_and(|size| size.width.is_none() && size.height.is_none())
            {
                return Err(ConfigError::EmptyApplicationSize(index + 1));
            }
        }
        self.agent.validate()?;
        let mut disabled_bindings = BTreeSet::<&KeySequence>::new();
        for binding in &self.keyboard.disabled_bindings {
            if binding.chords().len() > 8 {
                return Err(ConfigError::KeySequenceTooLong {
                    key: binding.to_string(),
                    length: binding.chords().len(),
                });
            }
            if !disabled_bindings.insert(binding) {
                return Err(ConfigError::DuplicateDisabledKeyBinding(
                    binding.to_string(),
                ));
            }
        }
        let mut configured_bindings = BTreeSet::<&KeySequence>::new();
        for binding in &self.keyboard.bindings {
            if !configured_bindings.insert(&binding.key) {
                return Err(ConfigError::DuplicateKeyBinding(binding.key.to_string()));
            }
        }
        let mut bindings = BTreeSet::<&KeySequence>::new();
        for binding in &effective_key_bindings {
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
            if !bindings.insert(&binding.key) {
                return Err(ConfigError::DuplicateKeyBinding(binding.key.to_string()));
            }
            for action in &binding.actions {
                self.validate_action(action, &|| binding.key.to_string())?;
            }
        }
        Ok(())
    }

    fn validate_action(
        &self,
        action: &Action,
        binding: &dyn Fn() -> String,
    ) -> Result<(), ConfigError> {
        let mut actions = 0_usize;
        self.validate_action_tree(action, binding, 0, &mut actions)
    }

    fn validate_action_tree(
        &self,
        action: &Action,
        binding: &dyn Fn() -> String,
        depth: usize,
        actions: &mut usize,
    ) -> Result<(), ConfigError> {
        const MAX_ACTION_DEPTH: usize = 8;
        const MAX_ACTION_TREE_ACTIONS: usize = 128;
        if depth > MAX_ACTION_DEPTH {
            return Err(ConfigError::ActionNestingTooDeep {
                context: binding(),
                depth,
            });
        }
        *actions = actions.saturating_add(1);
        if *actions > MAX_ACTION_TREE_ACTIONS {
            return Err(ConfigError::ActionTreeTooLarge(binding()));
        }
        if let Action::Execute {
            command,
            prompt,
            startup_notify,
        } = action
        {
            if command.trim().is_empty() || command.contains('\0') || command.len() > 16_384 {
                return Err(ConfigError::InvalidCommand(binding()));
            }
            if prompt.as_deref().is_some_and(|prompt| {
                prompt.trim().is_empty() || prompt.contains('\0') || prompt.len() > 255
            }) {
                return Err(ConfigError::InvalidExecutePrompt(binding()));
            }
            if startup_notify
                .as_ref()
                .is_some_and(|notification| !notification.is_valid())
            {
                return Err(ConfigError::InvalidStartupNotification(binding()));
            }
        }
        if let Action::Restart {
            command: Some(command),
        } = action
            && command.trim().is_empty()
        {
            return Err(ConfigError::EmptyRestartCommand(binding()));
        }
        if let Action::Debug { message } = action
            && (message.trim().is_empty() || message.contains('\0') || message.len() > 1_024)
        {
            return Err(ConfigError::InvalidDebugMessage(binding()));
        }
        match action {
            Action::If {
                queries,
                then_actions,
                else_actions,
            } => {
                self.validate_action_queries(queries, binding)?;
                for action in then_actions.iter().chain(else_actions) {
                    self.validate_action_tree(action, binding, depth.saturating_add(1), actions)?;
                }
            }
            Action::ForEach {
                queries,
                then_actions,
                else_actions,
                none,
            } => {
                self.validate_action_queries(queries, binding)?;
                for action in then_actions.iter().chain(else_actions).chain(none) {
                    self.validate_action_tree(action, binding, depth.saturating_add(1), actions)?;
                }
            }
            _ => {}
        }
        let workspace = match action {
            Action::SwitchWorkspace { workspace } | Action::MoveToWorkspace { workspace, .. } => {
                Some(*workspace)
            }
            Action::ShowMenu { menu } => {
                if self
                    .menu
                    .definitions
                    .iter()
                    .any(|definition| definition.id == *menu)
                {
                    None
                } else {
                    return Err(ConfigError::UnknownMenu {
                        context: binding(),
                        menu: menu.clone(),
                    });
                }
            }
            Action::Execute { .. }
            | Action::LaunchTerminal
            | Action::Screenshot { .. }
            | Action::Restart { .. }
            | Action::SessionLogout { .. }
            | Action::Debug { .. }
            | Action::If { .. }
            | Action::ForEach { .. }
            | Action::Stop
            | Action::Close
            | Action::Kill
            | Action::Reconfigure
            | Action::Focus { .. }
            | Action::FocusToBottom
            | Action::Unfocus
            | Action::FocusFallback
            | Action::Raise
            | Action::Lower
            | Action::RaiseLower
            | Action::Minimize
            | Action::Maximize { .. }
            | Action::Unmaximize { .. }
            | Action::ToggleMaximize
            | Action::ToggleMaximizeHorizontal
            | Action::ToggleMaximizeVertical
            | Action::ToggleFullscreen
            | Action::ToggleAlwaysOnTop
            | Action::ToggleAlwaysOnBottom
            | Action::SendToLayer { .. }
            | Action::Decorate
            | Action::Undecorate
            | Action::ToggleDecorations
            | Action::ToggleSticky
            | Action::Shade
            | Action::Unshade
            | Action::ToggleShade
            | Action::ShadeLower
            | Action::UnshadeRaise
            | Action::ToggleShowDesktop { .. }
            | Action::Move
            | Action::Resize { .. }
            | Action::MoveRelative { .. }
            | Action::ResizeRelative { .. }
            | Action::MoveToEdge { .. }
            | Action::GrowToEdge { .. }
            | Action::GrowToFill
            | Action::ShrinkToEdge { .. }
            | Action::MoveResizeTo { .. }
            | Action::MoveToCenter { .. }
            | Action::FocusDirection { .. }
            | Action::CycleDirection { .. }
            | Action::NextWindow
            | Action::PreviousWindow
            | Action::PreviousWorkspace
            | Action::NextWorkspace
            | Action::LastWorkspace
            | Action::AddWorkspace { .. }
            | Action::RemoveWorkspace { .. }
            | Action::WorkspaceLeft { .. }
            | Action::WorkspaceRight { .. }
            | Action::WorkspaceUp { .. }
            | Action::WorkspaceDown { .. }
            | Action::MoveToPreviousWorkspace { .. }
            | Action::MoveToNextWorkspace { .. }
            | Action::MoveToLastWorkspace { .. }
            | Action::MoveToWorkspaceLeft { .. }
            | Action::MoveToWorkspaceRight { .. }
            | Action::MoveToWorkspaceUp { .. }
            | Action::MoveToWorkspaceDown { .. }
            | Action::Exit { .. } => None,
        };
        if workspace.is_some_and(|workspace| {
            workspace == 0
                || usize::try_from(workspace)
                    .map_or(true, |workspace| workspace > self.workspaces.names.len())
        }) {
            return Err(ConfigError::InvalidWorkspaceBinding {
                key: binding(),
                workspace: workspace.unwrap_or_default(),
                count: self.workspaces.names.len(),
            });
        }
        Ok(())
    }

    fn validate_action_queries(
        &self,
        queries: &[ActionQuery],
        context: &dyn Fn() -> String,
    ) -> Result<(), ConfigError> {
        if queries.is_empty() {
            return Err(ConfigError::EmptyActionQueries(context()));
        }
        for query in queries {
            for (field, pattern) in [
                ("name", query.name.as_deref()),
                ("class", query.class.as_deref()),
                ("role", query.role.as_deref()),
                ("title", query.title.as_deref()),
            ] {
                if pattern.is_some_and(str::is_empty) {
                    return Err(ConfigError::EmptyActionQueryPattern {
                        context: context(),
                        field,
                    });
                }
            }
            let assigned = match query.workspace {
                Some(ActionQueryWorkspace::Number(workspace)) => Some(workspace.get()),
                _ => None,
            };
            for workspace in assigned
                .into_iter()
                .chain(query.active_workspace.map(std::num::NonZeroU32::get))
            {
                if usize::try_from(workspace)
                    .map_or(true, |workspace| workspace > self.workspaces.names.len())
                {
                    return Err(ConfigError::InvalidActionQueryWorkspace {
                        context: context(),
                        workspace,
                        count: self.workspaces.names.len(),
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_menus(&self) -> Result<(), ConfigError> {
        if !(160..=1_024).contains(&self.menu.width) {
            return Err(ConfigError::InvalidMenuWidth(self.menu.width));
        }
        if !(16..=64).contains(&self.menu.row_height) {
            return Err(ConfigError::InvalidMenuRowHeight(self.menu.row_height));
        }
        if !(2..=32).contains(&self.menu.max_rows) {
            return Err(ConfigError::InvalidMenuRows(self.menu.max_rows));
        }
        if !(50..=5_000).contains(&self.menu.command_timeout_ms) {
            return Err(ConfigError::InvalidMenuCommandTimeout(
                self.menu.command_timeout_ms,
            ));
        }
        if self.menu.definitions.len() > 64 {
            return Err(ConfigError::TooManyMenus(self.menu.definitions.len()));
        }

        let mut definitions = BTreeMap::new();
        for (index, definition) in self.menu.definitions.iter().enumerate() {
            validate_menu_text(&definition.id)
                .filter(|id| id.len() <= 64 && id.chars().all(is_menu_id_character))
                .ok_or(ConfigError::InvalidMenuId(index + 1))?;
            validate_menu_text(&definition.title)
                .ok_or_else(|| ConfigError::InvalidMenuTitle(definition.id.clone()))?;
            if definitions
                .insert(definition.id.as_str(), definition)
                .is_some()
            {
                return Err(ConfigError::DuplicateMenuId(definition.id.clone()));
            }
            match definition.source {
                MenuSource::Static => {
                    if definition.command.is_some() {
                        return Err(ConfigError::UnexpectedMenuCommand(definition.id.clone()));
                    }
                    if definition.entries.is_empty() {
                        return Err(ConfigError::EmptyMenu(definition.id.clone()));
                    }
                    if !definition.entries.iter().any(|entry| {
                        matches!(entry, MenuEntry::Item { .. } | MenuEntry::Submenu { .. })
                    }) {
                        return Err(ConfigError::MenuHasNoSelectableEntry(definition.id.clone()));
                    }
                }
                MenuSource::Command => {
                    let valid = definition.command.as_deref().is_some_and(|command| {
                        command.trim() == command
                            && !command.is_empty()
                            && command.len() <= 4_096
                            && !command.contains('\0')
                    });
                    if !valid {
                        return Err(ConfigError::InvalidMenuCommand(definition.id.clone()));
                    }
                    if !definition.entries.is_empty() {
                        return Err(ConfigError::DynamicMenuHasEntries(definition.id.clone()));
                    }
                }
                MenuSource::Applications
                | MenuSource::Client
                | MenuSource::ClientWorkspaces
                | MenuSource::Windows => {
                    if definition.command.is_some() {
                        return Err(ConfigError::UnexpectedMenuCommand(definition.id.clone()));
                    }
                    if !definition.entries.is_empty() {
                        return Err(ConfigError::DynamicMenuHasEntries(definition.id.clone()));
                    }
                }
            }
            if definition.entries.len() > 256 {
                return Err(ConfigError::TooManyMenuEntries {
                    menu: definition.id.clone(),
                    count: definition.entries.len(),
                });
            }
            for (entry_index, entry) in definition.entries.iter().enumerate() {
                match entry {
                    MenuEntry::Item { label, actions } => {
                        validate_menu_text(label).ok_or_else(|| ConfigError::InvalidMenuLabel {
                            menu: definition.id.clone(),
                            entry: entry_index + 1,
                        })?;
                        if actions.len() > 16 {
                            return Err(ConfigError::TooManyMenuEntryActions {
                                menu: definition.id.clone(),
                                entry: entry_index + 1,
                                count: actions.len(),
                            });
                        }
                        for action in actions {
                            self.validate_action(action, &|| {
                                format!("menu {} entry {}", definition.id, entry_index + 1)
                            })?;
                        }
                    }
                    MenuEntry::Submenu { label, menu } => {
                        validate_menu_text(label).ok_or_else(|| ConfigError::InvalidMenuLabel {
                            menu: definition.id.clone(),
                            entry: entry_index + 1,
                        })?;
                        if !definitions.contains_key(menu.as_str())
                            && !self
                                .menu
                                .definitions
                                .iter()
                                .any(|candidate| candidate.id == *menu)
                        {
                            return Err(ConfigError::UnknownMenu {
                                context: format!(
                                    "menu {} entry {}",
                                    definition.id,
                                    entry_index + 1
                                ),
                                menu: menu.clone(),
                            });
                        }
                    }
                    MenuEntry::Separator { label } => {
                        if label
                            .as_deref()
                            .is_some_and(|label| validate_menu_text(label).is_none())
                        {
                            return Err(ConfigError::InvalidMenuLabel {
                                menu: definition.id.clone(),
                                entry: entry_index + 1,
                            });
                        }
                    }
                }
            }
        }

        let mut complete = BTreeSet::new();
        for id in definitions.keys().copied() {
            validate_menu_acyclic(id, &definitions, &mut BTreeSet::new(), &mut complete)?;
        }
        Ok(())
    }
}

fn validate_menu_text(value: &str) -> Option<&str> {
    (!value.trim().is_empty() && !value.contains('\0') && value.len() <= 256).then_some(value)
}

fn is_menu_id_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
}

fn validate_menu_acyclic<'a>(
    id: &'a str,
    definitions: &BTreeMap<&'a str, &'a MenuDefinition>,
    visiting: &mut BTreeSet<&'a str>,
    complete: &mut BTreeSet<&'a str>,
) -> Result<(), ConfigError> {
    if complete.contains(id) {
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(ConfigError::CyclicMenu(id.to_owned()));
    }
    if let Some(definition) = definitions.get(id) {
        for entry in &definition.entries {
            if let MenuEntry::Submenu { menu, .. } = entry {
                validate_menu_acyclic(menu, definitions, visiting, complete)?;
            }
        }
    }
    visiting.remove(id);
    complete.insert(id);
    Ok(())
}

/// Protocol-neutral application metadata used by ordered rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationIdentity<'a> {
    /// Application instance/name.
    pub name: &'a str,
    /// Application class.
    pub class: &'a str,
    /// Window-group leader instance/name, when one exists.
    pub group_name: &'a str,
    /// Window-group leader class, when one exists.
    pub group_class: &'a str,
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
    /// Window-group leader instance/name wildcard.
    pub group_name: Option<String>,
    /// Window-group leader class wildcard.
    pub group_class: Option<String>,
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
            && self.group_name.is_none()
            && self.group_class.is_none()
            && self.role.is_none()
            && self.title.is_none()
            && self.kind.is_none()
    }

    /// Returns whether every configured matcher accepts `identity`.
    #[must_use]
    pub fn matches(&self, identity: ApplicationIdentity<'_>) -> bool {
        self.name
            .as_deref()
            .is_none_or(|pattern| wildcard_matches(pattern, identity.name))
            && self
                .class
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, identity.class))
            && self
                .group_name
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, identity.group_name))
            && self
                .group_class
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, identity.group_class))
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
    /// One-based workspace number or every workspace.
    pub workspace: Option<ApplicationWorkspace>,
    /// Requested stacking layer.
    pub layer: Option<ApplicationLayer>,
    /// Whether nobox should decorate the client.
    pub decorated: Option<bool>,
    /// Whether a newly mapped client should receive focus.
    pub focus: Option<bool>,
    /// Whether the client starts minimized.
    #[serde(alias = "iconic")]
    pub minimized: Option<bool>,
    /// Whether the client starts shaded.
    pub shaded: Option<bool>,
    /// Whether the client is omitted from pagers.
    pub skip_pager: Option<bool>,
    /// Whether the client is omitted from task lists.
    pub skip_taskbar: Option<bool>,
    /// Whether the client starts fullscreen.
    pub fullscreen: Option<bool>,
    /// Initial maximization axes.
    pub maximized: Option<ApplicationMaximized>,
    /// Initial absolute placement.
    pub position: Option<ApplicationPosition>,
    /// Initial client dimensions.
    pub size: Option<ApplicationSize>,
    /// How visible the client is to agent sessions.
    pub agent_visibility: Option<AgentVisibility>,
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
        if newer.minimized.is_some() {
            self.minimized = newer.minimized;
        }
        if newer.shaded.is_some() {
            self.shaded = newer.shaded;
        }
        if newer.skip_pager.is_some() {
            self.skip_pager = newer.skip_pager;
        }
        if newer.skip_taskbar.is_some() {
            self.skip_taskbar = newer.skip_taskbar;
        }
        if newer.fullscreen.is_some() {
            self.fullscreen = newer.fullscreen;
        }
        if newer.maximized.is_some() {
            self.maximized = newer.maximized;
        }
        if newer.position.is_some() {
            self.position = newer.position;
        }
        if newer.size.is_some() {
            self.size = newer.size;
        }
        if newer.agent_visibility.is_some() {
            self.agent_visibility = newer.agent_visibility;
        }
    }
}

/// Workspace assignment requested by an application rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationWorkspace {
    /// Make the client visible on every workspace.
    All,
    /// One-based configured workspace.
    Index(NonZeroU32),
}

/// Initial maximization requested by an application rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationMaximized {
    /// Explicitly start unmaximized.
    None,
    /// Maximize only horizontally.
    Horizontal,
    /// Maximize only vertically.
    Vertical,
    /// Maximize along both axes.
    Both,
}

impl ApplicationMaximized {
    /// Returns the horizontal and vertical requested states.
    #[must_use]
    pub const fn axes(self) -> (bool, bool) {
        match self {
            Self::None => (false, false),
            Self::Horizontal => (true, false),
            Self::Vertical => (false, true),
            Self::Both => (true, true),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ApplicationWorkspaceInput {
    Index(u32),
    Text(String),
}

impl<'de> Deserialize<'de> for ApplicationWorkspace {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let input = ApplicationWorkspaceInput::deserialize(deserializer)?;
        let index = match input {
            ApplicationWorkspaceInput::Index(index) => index,
            ApplicationWorkspaceInput::Text(value) if value.eq_ignore_ascii_case("all") => {
                return Ok(Self::All);
            }
            ApplicationWorkspaceInput::Text(value) => value
                .parse::<u32>()
                .map_err(|_| serde::de::Error::custom("expected all or a one-based workspace"))?,
        };
        NonZeroU32::new(index)
            .map(Self::Index)
            .ok_or_else(|| serde::de::Error::custom("workspace numbers are one-based"))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ApplicationMaximizedInput {
    Enabled(bool),
    Axes(String),
}

impl<'de> Deserialize<'de> for ApplicationMaximized {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match ApplicationMaximizedInput::deserialize(deserializer)? {
            ApplicationMaximizedInput::Enabled(false) => Ok(Self::None),
            ApplicationMaximizedInput::Enabled(true) => Ok(Self::Both),
            ApplicationMaximizedInput::Axes(value) => match value.to_ascii_lowercase().as_str() {
                "none" => Ok(Self::None),
                "horizontal" | "horiz" => Ok(Self::Horizontal),
                "vertical" | "vert" => Ok(Self::Vertical),
                "both" => Ok(Self::Both),
                _ => Err(serde::de::Error::custom(
                    "expected none, horizontal, vertical, both, true, or false",
                )),
            },
        }
    }
}

/// Initial gravity-style application placement within a work area.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ApplicationPosition {
    /// Horizontal position; omitted preserves normal placement on this axis.
    pub x: Option<AxisPosition>,
    /// Vertical position; omitted preserves normal placement on this axis.
    pub y: Option<AxisPosition>,
    /// Work area used to resolve the position.
    #[serde(alias = "monitor")]
    pub output: OutputTarget,
    /// Override an application's explicit program/user position hint.
    pub force: bool,
}

/// Initial application dimensions relative to its selected work area.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ApplicationSize {
    /// Width in pixels or as a positive fraction of the work area.
    pub width: Option<PositiveRelativeAmount>,
    /// Height in pixels or as a positive fraction of the work area.
    pub height: Option<PositiveRelativeAmount>,
    /// Whether width describes the decorated outer or client content size.
    pub width_basis: SizeBasis,
    /// Whether height describes the decorated outer or client content size.
    pub height_basis: SizeBasis,
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
    /// One-based workspace selected on a new session.
    pub initial: u32,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            names: ["1", "2", "3", "4"].map(str::to_owned).to_vec(),
            columns: 0,
            wrap: true,
            initial: 1,
        }
    }
}

/// User-reserved screen edges independent of panels and display protocols.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MarginConfig {
    /// Reserved pixels at the top edge.
    pub top: u32,
    /// Reserved pixels at the right edge.
    pub right: u32,
    /// Reserved pixels at the bottom edge.
    pub bottom: u32,
    /// Reserved pixels at the left edge.
    pub left: u32,
}

/// Persistent connector preferences. An empty list leaves topology automatic.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct OutputsConfig {
    /// Rules keyed by stable DRM connector name, such as `DP-1` or `eDP-1`.
    pub entries: Vec<OutputConfig>,
}

impl OutputsConfig {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.entries.len() > MAX_OUTPUTS {
            return Err(ConfigError::TooManyOutputs(self.entries.len()));
        }
        let mut names = BTreeSet::new();
        let mut primary = None;
        for (index, output) in self.entries.iter().enumerate() {
            let position = index.saturating_add(1);
            if !valid_output_name(&output.name) {
                return Err(ConfigError::InvalidOutputName(position));
            }
            if !names.insert(output.name.as_str()) {
                return Err(ConfigError::DuplicateOutputName(output.name.clone()));
            }
            if output.primary {
                if let Some(first) = primary {
                    return Err(ConfigError::MultiplePrimaryOutputs {
                        first,
                        second: position,
                    });
                }
                if !output.enabled {
                    return Err(ConfigError::DisabledPrimaryOutput(output.name.clone()));
                }
                primary = Some(position);
            }
            if let Some(position) = output.position
                && (position.x.unsigned_abs() > 1_000_000 || position.y.unsigned_abs() > 1_000_000)
            {
                return Err(ConfigError::OutputPositionOutOfRange(output.name.clone()));
            }
        }
        Ok(())
    }

    /// Returns the rule for one exact connector name.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&OutputConfig> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

fn valid_output_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// One connector rule shared by direct display backends and Settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    /// Exact connector name reported by the backend.
    pub name: String,
    /// Whether the connector should be used when it is present.
    pub enabled: bool,
    /// Requested mode; `None` selects the connector's preferred mode.
    pub mode: Option<OutputModeConfig>,
    /// Logical desktop origin; `None` lays the connector out automatically.
    pub position: Option<OutputPosition>,
    /// Logical transform applied before layout.
    pub transform: OutputTransform,
    /// Exact logical scale in Wayland's 1/120 units.
    pub scale: OutputScale,
    /// Whether this connector is preferred as the primary output.
    pub primary: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            mode: None,
            position: None,
            transform: OutputTransform::Normal,
            scale: OutputScale::default(),
            primary: false,
        }
    }
}

/// Requested logical desktop origin.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OutputPosition {
    /// Horizontal logical coordinate.
    pub x: i32,
    /// Vertical logical coordinate.
    pub y: i32,
}

/// Output transform independent of any display-server enum.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OutputTransform {
    /// No transform.
    #[default]
    Normal,
    /// Rotate clockwise by 90 degrees.
    Rotate90,
    /// Rotate clockwise by 180 degrees.
    Rotate180,
    /// Rotate clockwise by 270 degrees.
    Rotate270,
    /// Mirror horizontally.
    Flipped,
    /// Mirror horizontally, then rotate clockwise by 90 degrees.
    Flipped90,
    /// Mirror horizontally, then rotate clockwise by 180 degrees.
    Flipped180,
    /// Mirror horizontally, then rotate clockwise by 270 degrees.
    Flipped270,
}

impl OutputTransform {
    /// Returns the canonical configuration spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Rotate90 => "rotate90",
            Self::Rotate180 => "rotate180",
            Self::Rotate270 => "rotate270",
            Self::Flipped => "flipped",
            Self::Flipped90 => "flipped90",
            Self::Flipped180 => "flipped180",
            Self::Flipped270 => "flipped270",
        }
    }
}

/// Exact fractional output scale represented in Wayland's 1/120 units.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OutputScale(u16);

impl OutputScale {
    /// Creates a scale from protocol units when it is within 0.5x..=8x.
    #[must_use]
    pub const fn from_units(units: u16) -> Option<Self> {
        if units >= 60 && units <= 960 {
            Some(Self(units))
        } else {
            None
        }
    }

    /// Returns the scale in protocol units, where 120 is 1x.
    #[must_use]
    pub const fn units(self) -> u16 {
        self.0
    }

    /// Returns the scale as a floating-point factor for backend APIs.
    #[must_use]
    pub fn factor(self) -> f64 {
        f64::from(self.0) / 120.0
    }
}

impl Default for OutputScale {
    fn default() -> Self {
        Self(120)
    }
}

impl<'de> Deserialize<'de> for OutputScale {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let factor = f64::deserialize(deserializer)?;
        if !factor.is_finite() {
            return Err(serde::de::Error::custom("output scale must be finite"));
        }
        let exact_units = factor * 120.0;
        let rounded_units = exact_units.round();
        if !(60.0..=960.0).contains(&rounded_units)
            || (exact_units - rounded_units).abs() > 0.000_001
        {
            return Err(serde::de::Error::custom(
                "output scale must be 0.5..=8 in exact 1/120 increments",
            ));
        }
        let units = u16::try_from(rounded_units as u64)
            .map_err(|_| serde::de::Error::custom("output scale is out of range"))?;
        Ok(Self(units))
    }
}

/// Exact connector mode requested as `WIDTHxHEIGHT` with optional `@HZ`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputModeConfig {
    /// Physical pixel width.
    pub width: u32,
    /// Physical pixel height.
    pub height: u32,
    /// Optional refresh rate in millihertz.
    pub refresh_millihz: Option<u32>,
}

impl fmt::Display for OutputModeConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}x{}", self.width, self.height)?;
        if let Some(refresh) = self.refresh_millihz {
            let whole = refresh / 1000;
            let fraction = refresh % 1000;
            if fraction == 0 {
                write!(formatter, "@{whole}")
            } else {
                let mut fraction = format!("{fraction:03}");
                while fraction.ends_with('0') {
                    fraction.pop();
                }
                write!(formatter, "@{whole}.{fraction}")
            }
        } else {
            Ok(())
        }
    }
}

impl FromStr for OutputModeConfig {
    type Err = OutputModeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (dimensions, refresh) = value
            .split_once('@')
            .map_or((value, None), |(dimensions, refresh)| {
                (dimensions, Some(refresh))
            });
        let (width, height) = dimensions.split_once('x').ok_or(OutputModeError)?;
        let width = width.parse::<u32>().map_err(|_| OutputModeError)?;
        let height = height.parse::<u32>().map_err(|_| OutputModeError)?;
        if !(1..=16_384).contains(&width) || !(1..=16_384).contains(&height) {
            return Err(OutputModeError);
        }
        let refresh_millihz = refresh
            .map(parse_refresh_millihz)
            .transpose()?
            .filter(|refresh| *refresh > 0);
        Ok(Self {
            width,
            height,
            refresh_millihz,
        })
    }
}

fn parse_refresh_millihz(value: &str) -> Result<u32, OutputModeError> {
    let refresh = value.parse::<f64>().map_err(|_| OutputModeError)?;
    let millihz = (refresh * 1000.0).round();
    if !refresh.is_finite()
        || !(1.0..=1_000_000.0).contains(&millihz)
        || ((refresh * 1000.0) - millihz).abs() > 0.000_001
    {
        return Err(OutputModeError);
    }
    u32::try_from(millihz as u64).map_err(|_| OutputModeError)
}

impl<'de> Deserialize<'de> for OutputModeConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Error returned for an invalid output mode string.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("expected WIDTHxHEIGHT or WIDTHxHEIGHT@HZ within supported bounds")]
pub struct OutputModeError;

/// Focus behavior.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct FocusConfig {
    /// Focus newly mapped windows.
    pub focus_new: bool,
    /// Focus a client when the pointer enters it.
    pub follow_mouse: bool,
    /// Reject stale application focus requests while preserving user requests.
    pub prevent_focus_stealing: bool,
    /// Raise a window whenever nobox focuses it.
    pub raise_on_focus: bool,
}

impl Default for FocusConfig {
    fn default() -> Self {
        Self {
            focus_new: true,
            follow_mouse: false,
            prevent_focus_stealing: true,
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

/// Menu presentation bounds and named definitions shared by every backend.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MenuConfig {
    /// Preferred menu width in pixels, clamped to the selected output.
    pub width: u32,
    /// Height of each title, item, and separator row in pixels.
    pub row_height: u32,
    /// Maximum entries per page, including an overflow continuation entry.
    pub max_rows: u32,
    /// Maximum time a command-backed menu may run before it is killed.
    pub command_timeout_ms: u32,
    /// Named menus referenced by bindings, actions, and submenus.
    pub definitions: Vec<MenuDefinition>,
}

impl Default for MenuConfig {
    fn default() -> Self {
        Self {
            width: 260,
            row_height: 26,
            max_rows: 20,
            command_timeout_ms: 1_000,
            definitions: vec![
                MenuDefinition {
                    id: "root".to_owned(),
                    title: "nobox".to_owned(),
                    source: MenuSource::Static,
                    command: None,
                    entries: vec![
                        MenuEntry::Submenu {
                            label: "_Applications".to_owned(),
                            menu: "applications".to_owned(),
                        },
                        MenuEntry::Item {
                            label: "_Terminal".to_owned(),
                            actions: vec![Action::LaunchTerminal],
                        },
                        MenuEntry::Submenu {
                            label: "_Windows".to_owned(),
                            menu: "windows".to_owned(),
                        },
                        MenuEntry::Submenu {
                            label: "_Session".to_owned(),
                            menu: "session".to_owned(),
                        },
                        MenuEntry::Separator { label: None },
                        MenuEntry::Item {
                            label: "_Exit nobox".to_owned(),
                            actions: vec![Action::Exit { prompt: true }],
                        },
                    ],
                },
                MenuDefinition {
                    id: "applications".to_owned(),
                    title: "Applications".to_owned(),
                    source: MenuSource::Applications,
                    command: None,
                    entries: Vec::new(),
                },
                MenuDefinition {
                    id: "windows".to_owned(),
                    title: "Windows".to_owned(),
                    source: MenuSource::Windows,
                    command: None,
                    entries: Vec::new(),
                },
                MenuDefinition {
                    id: "client".to_owned(),
                    title: "Window".to_owned(),
                    source: MenuSource::Client,
                    command: None,
                    entries: Vec::new(),
                },
                MenuDefinition {
                    id: "client-workspaces".to_owned(),
                    title: "Send to workspace".to_owned(),
                    source: MenuSource::ClientWorkspaces,
                    command: None,
                    entries: Vec::new(),
                },
                MenuDefinition {
                    id: "session".to_owned(),
                    title: "Session".to_owned(),
                    source: MenuSource::Static,
                    command: None,
                    entries: vec![
                        MenuEntry::Item {
                            label: "_Reconfigure".to_owned(),
                            actions: vec![Action::Reconfigure],
                        },
                        MenuEntry::Item {
                            label: "_Restart nobox".to_owned(),
                            actions: vec![Action::Restart { command: None }],
                        },
                        MenuEntry::Separator { label: None },
                        MenuEntry::Item {
                            label: "_Log out".to_owned(),
                            actions: vec![Action::SessionLogout { prompt: true }],
                        },
                    ],
                },
            ],
        }
    }
}

/// One named menu that may be opened directly or as a submenu.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MenuDefinition {
    /// Stable identifier used by `show_menu` actions and submenu entries.
    pub id: String,
    /// Heading rendered above the entries.
    pub title: String,
    /// Static or backend-populated menu content.
    #[serde(default)]
    pub source: MenuSource,
    /// Shell command producing strict command-menu TOML when `source = "command"`.
    #[serde(default)]
    pub command: Option<String>,
    /// Ordered interactive items, submenu links, and separators.
    #[serde(default)]
    pub entries: Vec<MenuEntry>,
}

/// Source used to populate a named menu when it opens.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MenuSource {
    /// Use the configured entries verbatim.
    #[default]
    Static,
    /// Execute a bounded command and parse its TOML entry document when opened.
    Command,
    /// Discover installed XDG desktop applications and group them by category.
    Applications,
    /// Generate operations for the target or focused client.
    Client,
    /// Generate destinations for the target client's workspace assignment.
    ClientWorkspaces,
    /// Generate a combined workspace-grouped list of managed clients.
    Windows,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandMenuDocument {
    entries: Vec<MenuEntry>,
}

/// One entry in a named menu.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MenuEntry {
    /// A selectable entry that runs one or more actions.
    Item {
        /// User-visible entry text.
        label: String,
        /// Ordered actions run after dismissing the menu.
        actions: Vec<Action>,
    },
    /// A selectable link to another named menu.
    Submenu {
        /// User-visible entry text.
        label: String,
        /// Referenced menu identifier.
        menu: String,
    },
    /// A non-selectable visual separator with an optional heading.
    Separator {
        /// Optional text rendered on the separator row.
        label: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum RawMenuEntry {
    Item {
        label: String,
        #[serde(default)]
        action: Option<Action>,
        #[serde(default)]
        actions: Vec<Action>,
    },
    Submenu {
        label: String,
        menu: String,
    },
    Separator {
        #[serde(default)]
        label: Option<String>,
    },
}

impl<'de> Deserialize<'de> for MenuEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match RawMenuEntry::deserialize(deserializer)? {
            RawMenuEntry::Item {
                label,
                action,
                actions,
            } => Self::Item {
                label,
                actions: deserialize_binding_actions(action, actions, "menu item")?,
            },
            RawMenuEntry::Submenu { label, menu } => Self::Submenu { label, menu },
            RawMenuEntry::Separator { label } => Self::Separator { label },
        })
    }
}

/// Horizontal alignment of a window title within its usable titlebar area.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TitleAlignment {
    /// Align titles to the leading edge.
    #[default]
    Left,
    /// Center titles between the leading edge and the window buttons.
    Center,
    /// Align titles immediately before the window buttons.
    Right,
}

/// Server-side decoration settings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeConfig {
    /// Border width in pixels.
    pub border_width: u32,
    /// Titlebar height in pixels; zero disables the titlebar.
    pub titlebar_height: u32,
    /// X11 core font name or XLFD used by titlebars, menus, and overlays.
    pub font: String,
    /// Horizontal title alignment within the space left by window buttons.
    pub title_alignment: TitleAlignment,
    /// Horizontal inset around title text in pixels.
    pub title_padding: u32,
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
    /// Glyph and interaction-outline color shared by titlebar buttons.
    pub button_glyph: RgbColor,
    /// Color of the manager's agent-activity markers: the standing indicator
    /// while a session holds input or capture, and the frame highlight on a
    /// window receiving agent input. Deliberately unlike every other theme
    /// color so it cannot be mistaken for ordinary decoration.
    pub agent_marker: RgbColor,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            border_width: 2,
            titlebar_height: 24,
            font: "-*-helvetica-medium-r-normal--12-*-*-*-p-*-iso10646-1".to_owned(),
            title_alignment: TitleAlignment::Left,
            title_padding: 8,
            active_border: RgbColor::new(0x5e, 0x81, 0xa2),
            inactive_border: RgbColor::new(0x2e, 0x32, 0x38),
            urgent_border: RgbColor::new(0xa8, 0x4c, 0x44),
            active_titlebar: RgbColor::new(0x2d, 0x32, 0x3b),
            inactive_titlebar: RgbColor::new(0x1f, 0x22, 0x28),
            urgent_titlebar: RgbColor::new(0x53, 0x30, 0x29),
            title_text: RgbColor::new(0xe2, 0xe6, 0xec),
            minimize_button: RgbColor::new(0x38, 0x3e, 0x48),
            maximize_button: RgbColor::new(0x38, 0x3e, 0x48),
            close_button: RgbColor::new(0x7d, 0x3b, 0x3b),
            button_glyph: RgbColor::new(0xdf, 0xe3, 0xea),
            agent_marker: RgbColor::new(0xd8, 0x7f, 0x1e),
        }
    }
}

/// Mouse-driven window-management bindings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MouseConfig {
    /// Include nobox's standard pointer bindings before applying user overrides.
    pub inherit_defaults: bool,
    /// Standard bindings to omit, identified by context, chord, and trigger.
    pub disabled_bindings: Vec<MouseBindingSelector>,
    /// Modifier held for window-management drags.
    pub modifier: MouseModifier,
    /// Additional conventional modifiers accepted for window-management drags.
    pub compatibility_modifiers: Vec<MouseModifier>,
    /// Button used to move a client.
    pub move_button: u8,
    /// Button used to resize a client from its bottom-right corner.
    pub resize_button: u8,
    /// Snap a moved window beside visible windows within edge resistance.
    pub snap_to_windows: bool,
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
            inherit_defaults: true,
            disabled_bindings: Vec::new(),
            modifier: MouseModifier::Super,
            compatibility_modifiers: vec![MouseModifier::Alt],
            move_button: 1,
            resize_button: 3,
            snap_to_windows: true,
            edge_resistance: 10,
            drag_threshold: 8,
            double_click_ms: 500,
            bindings: Vec::new(),
        }
    }
}

impl MouseConfig {
    /// Returns the primary and compatibility drag modifiers without duplicates.
    #[must_use]
    pub fn effective_modifiers(&self) -> Vec<MouseModifier> {
        let mut modifiers = vec![self.modifier];
        for modifier in &self.compatibility_modifiers {
            if !modifiers.contains(modifier) {
                modifiers.push(*modifier);
            }
        }
        modifiers
    }

    /// Resolves inherited defaults, explicit omissions, and user overrides.
    #[must_use]
    pub fn effective_bindings(&self) -> Vec<MouseBinding> {
        let mut bindings = if self.inherit_defaults {
            standard_mouse_bindings(&self.effective_modifiers())
        } else {
            Vec::new()
        };
        bindings.retain(|binding| {
            !self
                .disabled_bindings
                .iter()
                .any(|disabled| disabled.matches(binding))
        });
        for binding in &self.bindings {
            if let Some(existing) = bindings.iter_mut().find(|existing| {
                existing.context == binding.context
                    && existing.button == binding.button
                    && existing.trigger == binding.trigger
            }) {
                *existing = binding.clone();
            } else {
                bindings.push(binding.clone());
            }
        }
        bindings
    }
}

/// Identity of one inherited pointer binding to omit.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct MouseBindingSelector {
    /// Decoration or surface context where the gesture begins.
    pub context: MouseContext,
    /// Modifier and physical button chord.
    pub button: MouseChord,
    /// Gesture phase to omit.
    pub trigger: MouseTrigger,
}

impl MouseBindingSelector {
    fn matches(&self, binding: &MouseBinding) -> bool {
        self.context == binding.context
            && self.button == binding.button
            && self.trigger == binding.trigger
    }
}

impl std::fmt::Display for MouseBindingSelector {
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

fn standard_mouse_bindings(modifiers: &[MouseModifier]) -> Vec<MouseBinding> {
    let focus_raise_unshade = || {
        [
            Action::Focus { here: false },
            Action::Raise,
            Action::Unshade,
        ]
    };
    let mut bindings = vec![
        MouseBinding::new(
            MouseContext::Client,
            MouseChord::new([], MouseButton::Left),
            MouseTrigger::Press,
            [Action::Focus { here: false }, Action::Raise],
        ),
        MouseBinding::new(
            MouseContext::Client,
            MouseChord::new([], MouseButton::Middle),
            MouseTrigger::Press,
            [Action::Focus { here: false }, Action::Raise],
        ),
        MouseBinding::new(
            MouseContext::Client,
            MouseChord::new([], MouseButton::Right),
            MouseTrigger::Press,
            [Action::Focus { here: false }, Action::Raise],
        ),
        MouseBinding::new(
            MouseContext::Titlebar,
            MouseChord::new([], MouseButton::Left),
            MouseTrigger::Press,
            focus_raise_unshade(),
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
        MouseBinding::new(
            MouseContext::Titlebar,
            MouseChord::new([], MouseButton::Middle),
            MouseTrigger::Press,
            [Action::Lower, Action::FocusToBottom, Action::FocusFallback],
        ),
        MouseBinding::new(
            MouseContext::Titlebar,
            MouseChord::new([], MouseButton::Right),
            MouseTrigger::Press,
            [
                Action::Focus { here: false },
                Action::Raise,
                Action::Unshade,
                Action::ShowMenu {
                    menu: "client".to_owned(),
                },
            ],
        ),
        MouseBinding::single(
            MouseContext::Border,
            MouseChord::new([], MouseButton::Left),
            MouseTrigger::Drag,
            Action::Resize { edge: None },
        ),
        MouseBinding::new(
            MouseContext::Minimize,
            MouseChord::new([], MouseButton::Left),
            MouseTrigger::Press,
            focus_raise_unshade(),
        ),
        MouseBinding::single(
            MouseContext::Minimize,
            MouseChord::new([], MouseButton::Left),
            MouseTrigger::Click,
            Action::Minimize,
        ),
        MouseBinding::new(
            MouseContext::Maximize,
            MouseChord::new([], MouseButton::Left),
            MouseTrigger::Press,
            focus_raise_unshade(),
        ),
        MouseBinding::single(
            MouseContext::Maximize,
            MouseChord::new([], MouseButton::Left),
            MouseTrigger::Click,
            Action::ToggleMaximize,
        ),
        MouseBinding::single(
            MouseContext::Maximize,
            MouseChord::new([], MouseButton::Middle),
            MouseTrigger::Click,
            Action::ToggleMaximizeVertical,
        ),
        MouseBinding::single(
            MouseContext::Maximize,
            MouseChord::new([], MouseButton::Right),
            MouseTrigger::Click,
            Action::ToggleMaximizeHorizontal,
        ),
        MouseBinding::new(
            MouseContext::Close,
            MouseChord::new([], MouseButton::Left),
            MouseTrigger::Press,
            focus_raise_unshade(),
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
        MouseBinding::single(
            MouseContext::Root,
            MouseChord::new([], MouseButton::Right),
            MouseTrigger::Press,
            Action::ShowMenu {
                menu: "root".to_owned(),
            },
        ),
        MouseBinding::single(
            MouseContext::Root,
            MouseChord::new([], MouseButton::Middle),
            MouseTrigger::Press,
            Action::ShowMenu {
                menu: "windows".to_owned(),
            },
        ),
    ];

    for modifier in modifiers {
        let modifier = match modifier {
            MouseModifier::Alt => KeyboardModifier::Alt,
            MouseModifier::Super => KeyboardModifier::Super,
        };
        bindings.extend([
            MouseBinding::new(
                MouseContext::Frame,
                MouseChord::new([modifier], MouseButton::Left),
                MouseTrigger::Press,
                [Action::Focus { here: false }, Action::Raise],
            ),
            MouseBinding::single(
                MouseContext::Frame,
                MouseChord::new([modifier], MouseButton::Left),
                MouseTrigger::Drag,
                Action::Move,
            ),
            MouseBinding::new(
                MouseContext::Frame,
                MouseChord::new([modifier], MouseButton::Right),
                MouseTrigger::Press,
                focus_raise_unshade(),
            ),
            MouseBinding::single(
                MouseContext::Frame,
                MouseChord::new([modifier], MouseButton::Right),
                MouseTrigger::Drag,
                Action::Resize { edge: None },
            ),
            MouseBinding::new(
                MouseContext::Frame,
                MouseChord::new([modifier], MouseButton::Middle),
                MouseTrigger::Press,
                [Action::Lower, Action::FocusToBottom, Action::FocusFallback],
            ),
            MouseBinding::single(
                MouseContext::Frame,
                MouseChord::new([modifier], MouseButton::Up),
                MouseTrigger::Click,
                Action::PreviousWorkspace,
            ),
            MouseBinding::single(
                MouseContext::Frame,
                MouseChord::new([modifier], MouseButton::Down),
                MouseTrigger::Click,
                Action::NextWorkspace,
            ),
            MouseBinding::single(
                MouseContext::Frame,
                MouseChord::new([modifier, KeyboardModifier::Control], MouseButton::Up),
                MouseTrigger::Click,
                Action::PreviousWorkspace,
            ),
            MouseBinding::single(
                MouseContext::Frame,
                MouseChord::new([modifier, KeyboardModifier::Control], MouseButton::Down),
                MouseTrigger::Click,
                Action::NextWorkspace,
            ),
            MouseBinding::single(
                MouseContext::Frame,
                MouseChord::new([modifier, KeyboardModifier::Shift], MouseButton::Up),
                MouseTrigger::Click,
                Action::MoveToPreviousWorkspace { follow: true },
            ),
            MouseBinding::single(
                MouseContext::Frame,
                MouseChord::new([modifier, KeyboardModifier::Shift], MouseButton::Down),
                MouseTrigger::Click,
                Action::MoveToNextWorkspace { follow: true },
            ),
        ]);
    }
    bindings
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
    /// XKB keyboard model used by native Wayland sessions, or the environment default.
    pub model: String,
    /// Comma-separated XKB layouts used by native Wayland sessions.
    pub layout: String,
    /// Comma-separated XKB variants corresponding to `layout`.
    pub variant: String,
    /// Comma-separated XKB options such as a Compose-key assignment.
    pub options: String,
    /// Include nobox's standard key bindings before applying user overrides.
    pub inherit_defaults: bool,
    /// Standard bindings to omit, identified by their complete key sequence.
    pub disabled_bindings: Vec<KeySequence>,
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
            model: String::new(),
            layout: String::new(),
            variant: String::new(),
            options: String::new(),
            inherit_defaults: true,
            disabled_bindings: Vec::new(),
            chain_quit_key: KeyChord::new([KeyboardModifier::Control], "g"),
            chain_timeout_ms: 3_000,
            bindings: Vec::new(),
        }
    }
}

impl KeyboardConfig {
    /// Resolves inherited defaults, explicit omissions, and user overrides.
    #[must_use]
    pub fn effective_bindings(&self) -> Vec<KeyBinding> {
        self.effective_bindings_with(&ShortcutsConfig::default())
    }

    fn effective_bindings_with(&self, shortcuts: &ShortcutsConfig) -> Vec<KeyBinding> {
        let mut bindings = if self.inherit_defaults {
            standard_key_bindings(shortcuts)
        } else {
            Vec::new()
        };
        bindings.retain(|binding| !self.disabled_bindings.contains(&binding.key));
        for binding in &self.bindings {
            if let Some(existing) = bindings
                .iter_mut()
                .find(|existing| existing.key == binding.key)
            {
                *existing = binding.clone();
            } else {
                bindings.push(binding.clone());
            }
        }
        bindings
    }
}

fn standard_key_bindings(shortcuts: &ShortcutsConfig) -> Vec<KeyBinding> {
    vec![
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Alt], "Tab"),
            Action::NextWindow,
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Alt, KeyboardModifier::Shift], "Tab"),
            Action::PreviousWindow,
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Alt], "space"),
            Action::ShowMenu {
                menu: "client".to_owned(),
            },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Alt], "F11"),
            Action::ToggleFullscreen,
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super], "Return"),
            Action::LaunchTerminal,
        ),
        KeyBinding::single(shortcuts.terminal.clone(), Action::LaunchTerminal),
        KeyBinding::single(
            shortcuts.screenshot.clone(),
            Action::Screenshot {
                target: ScreenshotTarget::Screen,
            },
        ),
        KeyBinding::single(
            shortcuts.window_screenshot.clone(),
            Action::Screenshot {
                target: ScreenshotTarget::Window,
            },
        ),
        KeyBinding::single(KeyChord::new([KeyboardModifier::Super], "q"), Action::Close),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super], "d"),
            Action::ToggleShowDesktop { strict: false },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Escape"),
            Action::Exit { prompt: true },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Control, KeyboardModifier::Alt], "Left"),
            Action::WorkspaceLeft { wrap: Some(false) },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Control, KeyboardModifier::Alt], "Right"),
            Action::WorkspaceRight { wrap: Some(false) },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Alt, KeyboardModifier::Shift], "Left"),
            Action::MoveToWorkspaceLeft {
                follow: true,
                wrap: Some(false),
            },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Alt, KeyboardModifier::Shift], "Right"),
            Action::MoveToWorkspaceRight {
                follow: true,
                wrap: Some(false),
            },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super], "Left"),
            Action::WorkspaceLeft { wrap: Some(false) },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super], "Right"),
            Action::WorkspaceRight { wrap: Some(false) },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super], "Up"),
            Action::WorkspaceUp { wrap: Some(false) },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super], "Down"),
            Action::WorkspaceDown { wrap: Some(false) },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Left"),
            Action::MoveToWorkspaceLeft {
                follow: true,
                wrap: Some(false),
            },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Right"),
            Action::MoveToWorkspaceRight {
                follow: true,
                wrap: Some(false),
            },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Up"),
            Action::MoveToWorkspaceUp {
                follow: true,
                wrap: Some(false),
            },
        ),
        KeyBinding::single(
            KeyChord::new([KeyboardModifier::Super, KeyboardModifier::Shift], "Down"),
            Action::MoveToWorkspaceDown {
                follow: true,
                wrap: Some(false),
            },
        ),
    ]
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

const fn default_true() -> bool {
    true
}

/// Backend-neutral metadata for tracking one application launch.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct StartupNotification {
    /// Human-readable application or document name.
    pub name: Option<String>,
    /// Freedesktop icon name.
    pub icon: Option<String>,
    /// Application identity expected on the first mapped window.
    pub wm_class: Option<String>,
}

impl StartupNotification {
    fn is_valid(&self) -> bool {
        [
            self.name.as_deref(),
            self.icon.as_deref(),
            self.wm_class.as_deref(),
        ]
        .into_iter()
        .flatten()
        .all(|value| {
            !value.trim().is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
        })
    }
}

/// Fixed edge or corner used by an interactive pointer resize.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ResizeEdge {
    /// Top edge.
    Top,
    /// Bottom edge.
    Bottom,
    /// Left edge.
    Left,
    /// Right edge.
    Right,
    /// Top-left corner.
    #[serde(alias = "topleft")]
    TopLeft,
    /// Top-right corner.
    #[serde(alias = "topright")]
    TopRight,
    /// Bottom-left corner.
    #[serde(alias = "bottomleft")]
    BottomLeft,
    /// Bottom-right corner.
    #[serde(alias = "bottomright")]
    BottomRight,
}

/// An action dispatched by the window manager.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Start a command through `/bin/sh -c`.
    Execute {
        /// Shell command to start.
        command: String,
        /// Optional confirmation text shown before launching.
        #[serde(default)]
        prompt: Option<String>,
        /// Optional desktop-startup notification metadata.
        #[serde(default)]
        startup_notify: Option<StartupNotification>,
    },
    /// Start the configured preferred terminal command.
    LaunchTerminal,
    /// Start the configured screenshot command for one target.
    Screenshot {
        /// Whole screen or active window.
        #[serde(default)]
        target: ScreenshotTarget,
    },
    /// Open a named menu using the triggering input location when available.
    ShowMenu {
        /// Menu identifier from `menu.definitions`.
        menu: String,
    },
    /// Validate and reload the effective configuration in place.
    Reconfigure,
    /// Cleanly restart nobox, or hand X11 ownership to another command.
    Restart {
        /// Optional shell command that replaces nobox after clean shutdown.
        #[serde(default)]
        command: Option<String>,
    },
    /// Ask the external session manager to save and end the desktop session.
    SessionLogout {
        /// Show a grabbed confirmation prompt before requesting logout.
        #[serde(default = "default_true")]
        prompt: bool,
    },
    /// Write a bounded user-supplied message to the structured runtime log.
    Debug {
        /// Message to log.
        #[serde(alias = "string")]
        message: String,
    },
    /// Run one branch after every configured query matches.
    If {
        /// Conjunctive queries evaluated against action or focused targets.
        #[serde(rename = "query")]
        queries: Vec<ActionQuery>,
        /// Actions run when every query matches.
        #[serde(rename = "then")]
        then_actions: Vec<Action>,
        /// Actions run when any query does not match.
        #[serde(default, rename = "else")]
        else_actions: Vec<Action>,
    },
    /// Evaluate queries and branches once for every managed client.
    ForEach {
        /// Conjunctive queries evaluated for each action target.
        #[serde(rename = "query")]
        queries: Vec<ActionQuery>,
        /// Actions run for each matching client.
        #[serde(rename = "then")]
        then_actions: Vec<Action>,
        /// Actions run for each non-matching client.
        #[serde(default, rename = "else")]
        else_actions: Vec<Action>,
        /// Actions run once when no managed client matches.
        #[serde(default)]
        none: Vec<Action>,
    },
    /// Stop the current nested action list and enclosing `for_each` loop.
    Stop,
    /// Ask the focused client to close using ICCCM when supported.
    Close,
    /// Immediately disconnect the X11 connection that owns the action target.
    Kill,
    /// Activate the action target, following its workspace unless it is brought here.
    Focus {
        /// Move a target from another workspace to the active workspace.
        #[serde(default)]
        here: bool,
    },
    /// Move the action target to the least-recent end of focus history.
    FocusToBottom,
    /// Focus the most recent valid client other than the action target.
    Unfocus,
    /// Compatibility name for focusing a fallback away from the action target.
    FocusFallback,
    /// Raise the action target within its policy layer.
    Raise,
    /// Lower the action target within its policy layer.
    Lower,
    /// Raise when obscured, lower when obscuring, otherwise preserve stacking.
    RaiseLower,
    /// Minimize the action target through the shared iconic lifecycle.
    Minimize,
    /// Enable maximization on the selected axes without toggling them.
    Maximize {
        /// Axes to maximize.
        #[serde(default)]
        direction: MaximizeDirection,
    },
    /// Disable maximization on the selected axes without toggling them.
    Unmaximize {
        /// Axes to restore.
        #[serde(default)]
        direction: MaximizeDirection,
    },
    /// Toggle both maximize axes on the action target.
    ToggleMaximize,
    /// Toggle only the horizontal maximize axis on the action target.
    ToggleMaximizeHorizontal,
    /// Toggle only the vertical maximize axis on the action target.
    ToggleMaximizeVertical,
    /// Toggle whether the action target fills its output without decorations.
    ToggleFullscreen,
    /// Toggle whether the action target stays above ordinary windows and docks.
    ToggleAlwaysOnTop,
    /// Toggle whether the action target stays below ordinary windows.
    ToggleAlwaysOnBottom,
    /// Place the action target on an explicit policy layer.
    SendToLayer {
        /// Requested layer independent of display protocol.
        layer: LayerTarget,
    },
    /// Restore the action target's natural server-side decoration policy.
    Decorate,
    /// Suppress server-side decorations on the action target.
    Undecorate,
    /// Toggle a reversible user override for server-side decorations.
    ToggleDecorations,
    /// Toggle whether the action target appears on every workspace.
    ToggleSticky,
    /// Collapse the action target to its titlebar without toggling.
    Shade,
    /// Expand a shaded action target without toggling.
    Unshade,
    /// Collapse or restore the action target's titlebar-bearing frame.
    ToggleShade,
    /// Shade an expanded target, or lower one that is already shaded.
    ShadeLower,
    /// Unshade a shaded target, or raise one that is already expanded.
    UnshadeRaise,
    /// Temporarily hide or restore ordinary clients to expose the desktop.
    ToggleShowDesktop {
        /// Keep new ordinary windows hidden until show-desktop is toggled off.
        #[serde(default)]
        strict: bool,
    },
    /// Start an interactive pointer or keyboard move.
    Move,
    /// Start an interactive pointer or keyboard resize.
    Resize {
        /// Fixed pointer edge; omitted means infer it from the invocation.
        #[serde(default)]
        edge: Option<ResizeEdge>,
    },
    /// Move the action target by pixel or work-area-relative offsets.
    MoveRelative {
        /// Horizontal offset; percentages and fractions use work-area width.
        #[serde(default)]
        x: RelativeAmount,
        /// Vertical offset; percentages and fractions use work-area height.
        #[serde(default)]
        y: RelativeAmount,
    },
    /// Resize each edge by pixel or client-size-relative amounts.
    ResizeRelative {
        /// Amount to grow the left edge outward.
        #[serde(default)]
        left: RelativeAmount,
        /// Amount to grow the right edge outward.
        #[serde(default)]
        right: RelativeAmount,
        /// Amount to grow the top edge outward.
        #[serde(default)]
        top: RelativeAmount,
        /// Amount to grow the bottom edge outward.
        #[serde(default)]
        bottom: RelativeAmount,
    },
    /// Move the action target to the next work-area or client edge.
    MoveToEdge {
        /// Direction in which to search for the next edge.
        direction: EdgeDirection,
    },
    /// Grow one edge toward the next obstacle, shrinking when growth is blocked.
    GrowToEdge {
        /// Edge to grow and direction in which to search.
        direction: EdgeDirection,
    },
    /// Grow every edge into surrounding free space.
    GrowToFill,
    /// Shrink the edge opposite the requested direction toward an obstacle.
    ShrinkToEdge {
        /// Direction toward which the opposite edge moves.
        direction: EdgeDirection,
    },
    /// Move and optionally resize the target within a selected output area.
    MoveResizeTo {
        /// Horizontal gravity-style position; omitted preserves the source offset.
        #[serde(default)]
        x: Option<AxisPosition>,
        /// Vertical gravity-style position; omitted preserves the source offset.
        #[serde(default)]
        y: Option<AxisPosition>,
        /// Target width in pixels or as a fraction of the selected work area.
        #[serde(default)]
        width: Option<PositiveRelativeAmount>,
        /// Target height in pixels or as a fraction of the selected work area.
        #[serde(default)]
        height: Option<PositiveRelativeAmount>,
        /// Whether width describes decorated outer or client content size.
        #[serde(default)]
        width_basis: SizeBasis,
        /// Whether height describes decorated outer or client content size.
        #[serde(default)]
        height_basis: SizeBasis,
        /// Output area in which to place the target.
        #[serde(default, alias = "monitor")]
        output: OutputTarget,
    },
    /// Center the target without changing its size.
    MoveToCenter {
        /// Output area in which to center the target.
        #[serde(default, alias = "monitor")]
        output: OutputTarget,
    },
    /// Focus, unshade, and raise the nearest client in a spatial direction.
    #[serde(alias = "directional_target_window")]
    FocusDirection {
        /// Direction in which to search from the action target or focused client.
        direction: WindowDirection,
    },
    /// Preview spatial focus targets until the binding modifiers are released.
    #[serde(alias = "directional_cycle_windows")]
    CycleDirection {
        /// Direction in which to advance from the current preview target.
        direction: WindowDirection,
    },
    /// Focus the next client in the current most-recently-used cycle.
    NextWindow,
    /// Focus the previous client in the current most-recently-used cycle.
    PreviousWindow,
    /// Switch to the previous workspace, wrapping at the first.
    PreviousWorkspace,
    /// Switch to the next workspace, wrapping at the last.
    NextWorkspace,
    /// Switch to the previously active workspace.
    LastWorkspace,
    /// Insert an empty workspace at the current position or at the end.
    AddWorkspace {
        /// Position at which to insert the workspace.
        #[serde(default)]
        at: WorkspacePlacement,
    },
    /// Remove and merge the current or final workspace.
    RemoveWorkspace {
        /// Workspace position to remove.
        #[serde(default)]
        at: WorkspacePlacement,
    },
    /// Switch to the workspace geometrically left in the active layout.
    WorkspaceLeft {
        /// Override the configured grid-edge wrap policy.
        #[serde(default)]
        wrap: Option<bool>,
    },
    /// Switch to the workspace geometrically right in the active layout.
    WorkspaceRight {
        /// Override the configured grid-edge wrap policy.
        #[serde(default)]
        wrap: Option<bool>,
    },
    /// Switch to the workspace geometrically above in the active layout.
    WorkspaceUp {
        /// Override the configured grid-edge wrap policy.
        #[serde(default)]
        wrap: Option<bool>,
    },
    /// Switch to the workspace geometrically below in the active layout.
    WorkspaceDown {
        /// Override the configured grid-edge wrap policy.
        #[serde(default)]
        wrap: Option<bool>,
    },
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
        #[serde(default = "default_true")]
        follow: bool,
    },
    /// Move the focused client to the previous workspace.
    MoveToPreviousWorkspace {
        /// Switch to the destination after moving the client.
        #[serde(default = "default_true")]
        follow: bool,
    },
    /// Move the focused client to the next workspace.
    MoveToNextWorkspace {
        /// Switch to the destination after moving the client.
        #[serde(default = "default_true")]
        follow: bool,
    },
    /// Move the focused client to the previously active workspace.
    MoveToLastWorkspace {
        /// Switch to the destination after moving the client.
        #[serde(default = "default_true")]
        follow: bool,
    },
    /// Move the focused client left in the active workspace layout.
    MoveToWorkspaceLeft {
        /// Switch to the destination after moving the client.
        #[serde(default = "default_true")]
        follow: bool,
        /// Override the configured grid-edge wrap policy.
        #[serde(default)]
        wrap: Option<bool>,
    },
    /// Move the focused client right in the active workspace layout.
    MoveToWorkspaceRight {
        /// Switch to the destination after moving the client.
        #[serde(default = "default_true")]
        follow: bool,
        /// Override the configured grid-edge wrap policy.
        #[serde(default)]
        wrap: Option<bool>,
    },
    /// Move the focused client upward in the active workspace layout.
    MoveToWorkspaceUp {
        /// Switch to the destination after moving the client.
        #[serde(default = "default_true")]
        follow: bool,
        /// Override the configured grid-edge wrap policy.
        #[serde(default)]
        wrap: Option<bool>,
    },
    /// Move the focused client downward in the active workspace layout.
    MoveToWorkspaceDown {
        /// Switch to the destination after moving the client.
        #[serde(default = "default_true")]
        follow: bool,
        /// Override the configured grid-edge wrap policy.
        #[serde(default)]
        wrap: Option<bool>,
    },
    /// Exit the window manager without ending the surrounding desktop session.
    Exit {
        /// Show a grabbed confirmation prompt before releasing X11 ownership.
        #[serde(default = "default_true")]
        prompt: bool,
    },
}

/// Target selected by a configured screenshot action.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotTarget {
    /// Capture the complete screen.
    #[default]
    Screen,
    /// Capture the currently active window and its decoration.
    Window,
}

/// Client selected for a conditional action query.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ActionQueryTarget {
    /// The binding, menu, or current `for_each` action target.
    #[default]
    Action,
    /// The currently focused client, independently of the action target.
    Focused,
}

/// Relative workspace selector used by conditional action queries.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ActionQueryWorkspaceRelation {
    /// The currently active workspace, including sticky clients.
    Current,
    /// A workspace other than the active one; sticky clients do not match.
    Other,
    /// The previously active workspace; sticky clients do not match.
    Last,
}

/// Workspace predicate used by a conditional action query.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum ActionQueryWorkspace {
    /// A relative workspace name.
    Relative(ActionQueryWorkspaceRelation),
    /// An absolute one-based workspace number.
    Number(std::num::NonZeroU32),
}

/// Conjunctive, protocol-neutral predicate used by `if` and `for_each`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ActionQuery {
    /// Client against which this query is evaluated.
    pub target: ActionQueryTarget,
    /// Shaded state.
    pub shaded: Option<bool>,
    /// Both maximize axes are active.
    pub maximized: Option<bool>,
    /// Horizontal maximize state.
    pub maximized_horizontal: Option<bool>,
    /// Vertical maximize state.
    pub maximized_vertical: Option<bool>,
    /// Minimized/iconic state.
    pub minimized: Option<bool>,
    /// Fullscreen state.
    pub fullscreen: Option<bool>,
    /// Whether this client owns focus.
    pub focused: Option<bool>,
    /// Whether this client is permitted to receive focus.
    pub focusable: Option<bool>,
    /// Urgency or demands-attention state.
    pub urgent: Option<bool>,
    /// Whether a visible server-side titlebar is present.
    pub decorated: Option<bool>,
    /// All-workspaces/sticky state.
    pub sticky: Option<bool>,
    /// Client workspace relation or one-based number.
    pub workspace: Option<ActionQueryWorkspace>,
    /// One-based active workspace number, independent of client presence.
    pub active_workspace: Option<std::num::NonZeroU32>,
    /// One-based output number containing the client.
    pub output: Option<std::num::NonZeroU32>,
    /// Application instance/name wildcard.
    pub name: Option<String>,
    /// Application class wildcard.
    pub class: Option<String>,
    /// Application role wildcard.
    pub role: Option<String>,
    /// Window title wildcard.
    pub title: Option<String>,
    /// Functional client kind.
    pub kind: Option<ApplicationKind>,
}

/// Backend-supplied facts evaluated by an [`ActionQuery`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionQueryContext<'a> {
    /// Protocol-neutral application metadata.
    pub identity: ApplicationIdentity<'a>,
    /// Zero-based assigned workspace, or `None` for a sticky client.
    pub workspace: Option<u32>,
    /// Zero-based active workspace.
    pub active_workspace: u32,
    /// Zero-based previously active workspace.
    pub last_workspace: u32,
    /// One-based output number containing the client.
    pub output: u32,
    /// Current client-state facts.
    pub shaded: bool,
    /// Horizontal maximize state.
    pub maximized_horizontal: bool,
    /// Vertical maximize state.
    pub maximized_vertical: bool,
    /// Minimized/iconic state.
    pub minimized: bool,
    /// Fullscreen state.
    pub fullscreen: bool,
    /// Whether this client owns focus.
    pub focused: bool,
    /// Whether this client is permitted to receive focus.
    pub focusable: bool,
    /// Urgency or demands-attention state.
    pub urgent: bool,
    /// Whether a visible server-side titlebar is present.
    pub decorated: bool,
}

impl ActionQuery {
    /// Returns whether this query accepts the supplied client and active workspace.
    #[must_use]
    pub fn matches(&self, context: Option<ActionQueryContext<'_>>, active_workspace: u32) -> bool {
        if self
            .active_workspace
            .is_some_and(|workspace| workspace.get().saturating_sub(1) != active_workspace)
        {
            return false;
        }
        let Some(context) = context else {
            return self.active_workspace.is_some();
        };
        let maximized = context.maximized_horizontal && context.maximized_vertical;
        let sticky = context.workspace.is_none();
        self.shaded.is_none_or(|value| value == context.shaded)
            && self.maximized.is_none_or(|value| value == maximized)
            && self
                .maximized_horizontal
                .is_none_or(|value| value == context.maximized_horizontal)
            && self
                .maximized_vertical
                .is_none_or(|value| value == context.maximized_vertical)
            && self
                .minimized
                .is_none_or(|value| value == context.minimized)
            && self
                .fullscreen
                .is_none_or(|value| value == context.fullscreen)
            && self.focused.is_none_or(|value| value == context.focused)
            && self
                .focusable
                .is_none_or(|value| value == context.focusable)
            && self.urgent.is_none_or(|value| value == context.urgent)
            && self
                .decorated
                .is_none_or(|value| value == context.decorated)
            && self.sticky.is_none_or(|value| value == sticky)
            && self.workspace.is_none_or(|workspace| match workspace {
                ActionQueryWorkspace::Relative(ActionQueryWorkspaceRelation::Current) => {
                    sticky || context.workspace == Some(context.active_workspace)
                }
                ActionQueryWorkspace::Relative(ActionQueryWorkspaceRelation::Other) => context
                    .workspace
                    .is_some_and(|workspace| workspace != context.active_workspace),
                ActionQueryWorkspace::Relative(ActionQueryWorkspaceRelation::Last) => {
                    context.workspace == Some(context.last_workspace)
                }
                ActionQueryWorkspace::Number(workspace) => {
                    sticky || context.workspace == Some(workspace.get().saturating_sub(1))
                }
            })
            && self
                .output
                .is_none_or(|output| output.get() == context.output)
            && self
                .name
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, context.identity.name))
            && self
                .class
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, context.identity.class))
            && self
                .role
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, context.identity.role))
            && self
                .title
                .as_deref()
                .is_none_or(|pattern| wildcard_matches(pattern, context.identity.title))
            && self.kind.is_none_or(|kind| kind == context.identity.kind)
    }
}

/// Axes affected by an explicit maximize or unmaximize action.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MaximizeDirection {
    /// Change both axes.
    #[default]
    Both,
    /// Change only the horizontal axis.
    #[serde(alias = "horz")]
    Horizontal,
    /// Change only the vertical axis.
    #[serde(alias = "vert")]
    Vertical,
}

/// Explicit stacking layer selected by an action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LayerTarget {
    /// Keep the client below ordinary application windows.
    #[serde(alias = "bottom")]
    Below,
    /// Restore the role's normal layer.
    #[serde(alias = "middle")]
    Normal,
    /// Keep the client above ordinary windows and docks.
    #[serde(alias = "top")]
    Above,
}

/// Position used by runtime workspace insertion and removal actions.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePlacement {
    /// Insert or remove at the end of the workspace list.
    #[default]
    Last,
    /// Insert or remove at the currently active index.
    Current,
}

/// Cardinal direction used by edge-oriented geometry actions.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EdgeDirection {
    /// Move toward smaller horizontal coordinates.
    #[serde(alias = "west")]
    Left,
    /// Move toward larger horizontal coordinates.
    #[serde(alias = "east")]
    Right,
    /// Move toward smaller vertical coordinates.
    #[default]
    #[serde(alias = "north")]
    Up,
    /// Move toward larger vertical coordinates.
    #[serde(alias = "south")]
    Down,
}

/// Eight-way direction used for spatial window focus.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WindowDirection {
    /// Search toward smaller horizontal coordinates.
    #[serde(alias = "west")]
    Left,
    /// Search toward larger horizontal coordinates.
    #[serde(alias = "east")]
    Right,
    /// Search toward smaller vertical coordinates.
    #[serde(alias = "north")]
    Up,
    /// Search toward larger vertical coordinates.
    #[serde(alias = "south")]
    Down,
    /// Search diagonally toward smaller horizontal and vertical coordinates.
    #[serde(alias = "northwest", alias = "north_west")]
    UpLeft,
    /// Search diagonally toward larger horizontal and smaller vertical coordinates.
    #[serde(alias = "northeast", alias = "north_east")]
    UpRight,
    /// Search diagonally toward smaller horizontal and larger vertical coordinates.
    #[serde(alias = "southwest", alias = "south_west")]
    DownLeft,
    /// Search diagonally toward larger horizontal and vertical coordinates.
    #[serde(alias = "southeast", alias = "south_east")]
    DownRight,
}

/// Gravity-style position of one axis within a selected work area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AxisPosition {
    /// Offset from the starting edge.
    Start(RelativeAmount),
    /// Center on this axis.
    Center,
    /// Inset from the ending edge.
    End(RelativeAmount),
}

/// Work area selected by an absolute placement action.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputTarget {
    /// Output currently containing most of the client.
    #[default]
    Current,
    /// Backend-designated primary output.
    Primary,
    /// Output containing the pointer at action time.
    Pointer,
    /// Next output in stable discovery order, wrapping at the end.
    Next,
    /// Previous output in stable discovery order, wrapping at the beginning.
    Previous,
    /// Bounding work area across all outputs.
    All,
    /// One-based output in stable discovery order.
    Index(NonZeroU32),
}

/// Geometry dimension interpreted as decorated outer or client content size.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SizeBasis {
    /// Include server-side decoration extents.
    #[default]
    Outer,
    /// Describe application content only.
    Content,
}

/// Strictly positive pixel amount or fraction used for geometry dimensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PositiveRelativeAmount(RelativeAmount);

/// A signed pixel amount or fraction of a context-dependent reference size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelativeAmount {
    /// Fixed number of pixels.
    Pixels(i32),
    /// Signed fraction such as `50%` or `1/3`.
    Fraction {
        /// Signed numerator.
        numerator: i32,
        /// Strictly positive denominator.
        denominator: NonZeroU32,
    },
}

impl Default for RelativeAmount {
    fn default() -> Self {
        Self::Pixels(0)
    }
}

impl RelativeAmount {
    /// Resolves the amount against a width or height with saturating arithmetic.
    #[must_use]
    pub fn resolve(self, reference: u32) -> i32 {
        match self {
            Self::Pixels(pixels) => pixels,
            Self::Fraction {
                numerator,
                denominator,
            } => {
                let scaled = i64::from(numerator).saturating_mul(i64::from(reference));
                i32::try_from(scaled / i64::from(denominator.get())).unwrap_or(
                    if scaled.is_negative() {
                        i32::MIN
                    } else {
                        i32::MAX
                    },
                )
            }
        }
    }

    const fn is_positive(self) -> bool {
        match self {
            Self::Pixels(pixels) => pixels > 0,
            Self::Fraction { numerator, .. } => numerator > 0,
        }
    }

    const fn is_negative(self) -> bool {
        match self {
            Self::Pixels(pixels) => pixels < 0,
            Self::Fraction { numerator, .. } => numerator < 0,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RelativeAmountInput {
    Pixels(i32),
    Text(String),
}

impl<'de> Deserialize<'de> for RelativeAmount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match RelativeAmountInput::deserialize(deserializer)? {
            RelativeAmountInput::Pixels(pixels) => Ok(Self::Pixels(pixels)),
            RelativeAmountInput::Text(value) => value.parse().map_err(serde::de::Error::custom),
        }
    }
}

impl FromStr for RelativeAmount {
    type Err = RelativeAmountError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim() != value || value.is_empty() {
            return Err(RelativeAmountError(value.to_owned()));
        }
        if let Some(numerator) = value.strip_suffix('%') {
            return numerator
                .parse::<i32>()
                .map(|numerator| Self::Fraction {
                    numerator,
                    denominator: NonZeroU32::new(100).expect("100 is non-zero"),
                })
                .map_err(|_| RelativeAmountError(value.to_owned()));
        }
        if let Some((numerator, denominator)) = value.split_once('/') {
            let numerator = numerator
                .parse::<i32>()
                .map_err(|_| RelativeAmountError(value.to_owned()))?;
            let denominator = denominator
                .parse::<u32>()
                .map_err(|_| RelativeAmountError(value.to_owned()))?;
            let denominator = NonZeroU32::new(denominator)
                .ok_or_else(|| RelativeAmountError(value.to_owned()))?;
            return Ok(Self::Fraction {
                numerator,
                denominator,
            });
        }
        value
            .parse::<i32>()
            .map(Self::Pixels)
            .map_err(|_| RelativeAmountError(value.to_owned()))
    }
}

/// Invalid relative pixel, percentage, or fraction syntax.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("invalid relative amount {0:?}; expected an integer, N%, or N/D")]
pub struct RelativeAmountError(String);

impl PositiveRelativeAmount {
    /// Resolves the configured dimension to at least one pixel.
    #[must_use]
    pub fn resolve(self, reference: u32) -> u32 {
        u32::try_from(self.0.resolve(reference))
            .unwrap_or(u32::MAX)
            .max(1)
    }
}

impl TryFrom<RelativeAmount> for PositiveRelativeAmount {
    type Error = PositiveRelativeAmountError;

    fn try_from(amount: RelativeAmount) -> Result<Self, Self::Error> {
        amount
            .is_positive()
            .then_some(Self(amount))
            .ok_or(PositiveRelativeAmountError)
    }
}

impl<'de> Deserialize<'de> for PositiveRelativeAmount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::try_from(RelativeAmount::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A geometry dimension was zero or negative.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("geometry dimensions must be positive pixels, percentages, or fractions")]
pub struct PositiveRelativeAmountError;

#[derive(Deserialize)]
#[serde(untagged)]
enum AxisPositionInput {
    Pixels(i32),
    Text(String),
}

impl<'de> Deserialize<'de> for AxisPosition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match AxisPositionInput::deserialize(deserializer)? {
            AxisPositionInput::Pixels(pixels) => {
                axis_pixel_position(pixels).map_err(serde::de::Error::custom)
            }
            AxisPositionInput::Text(value) => value.parse().map_err(serde::de::Error::custom),
        }
    }
}

fn axis_pixel_position(pixels: i32) -> Result<AxisPosition, AxisPositionError> {
    if pixels < 0 {
        pixels
            .checked_abs()
            .map(|inset| AxisPosition::End(RelativeAmount::Pixels(inset)))
            .ok_or_else(|| AxisPositionError(pixels.to_string()))
    } else {
        Ok(AxisPosition::Start(RelativeAmount::Pixels(pixels)))
    }
}

impl FromStr for AxisPosition {
    type Err = AxisPositionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.eq_ignore_ascii_case("center") {
            return Ok(Self::Center);
        }
        let (ending_edge, amount) = if let Some(amount) = value.strip_prefix('-') {
            (true, amount)
        } else {
            (false, value.strip_prefix('+').unwrap_or(value))
        };
        let amount = amount
            .parse::<RelativeAmount>()
            .map_err(|_| AxisPositionError(value.to_owned()))?;
        if amount.is_negative() {
            return Err(AxisPositionError(value.to_owned()));
        }
        Ok(if ending_edge {
            Self::End(amount)
        } else {
            Self::Start(amount)
        })
    }
}

/// Invalid gravity-style axis position.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("invalid axis position {0:?}; expected center, +N, -N, N%, or N/D")]
pub struct AxisPositionError(String);

#[derive(Deserialize)]
#[serde(untagged)]
enum OutputTargetInput {
    Index(u32),
    Text(String),
}

impl<'de> Deserialize<'de> for OutputTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match OutputTargetInput::deserialize(deserializer)? {
            OutputTargetInput::Index(index) => output_index(index, index.to_string()),
            OutputTargetInput::Text(value) => value.parse(),
        }
        .map_err(serde::de::Error::custom)
    }
}

impl FromStr for OutputTarget {
    type Err = OutputTargetError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "current" => Ok(Self::Current),
            "primary" => Ok(Self::Primary),
            "pointer" | "mouse" => Ok(Self::Pointer),
            "next" => Ok(Self::Next),
            "previous" | "prev" => Ok(Self::Previous),
            "all" => Ok(Self::All),
            _ => value
                .parse::<u32>()
                .map_err(|_| OutputTargetError(value.to_owned()))
                .and_then(|index| output_index(index, value.to_owned())),
        }
    }
}

fn output_index(index: u32, original: String) -> Result<OutputTarget, OutputTargetError> {
    NonZeroU32::new(index)
        .map(OutputTarget::Index)
        .ok_or(OutputTargetError(original))
}

/// Invalid absolute-placement output target.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error(
    "invalid output target {0:?}; expected current, primary, pointer, next, previous, all, or N"
)]
pub struct OutputTargetError(String);

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

/// Returns the exact-to-general lookup order for pointer bindings.
///
/// Both display backends use this chain so a context-specific binding wins
/// before the shared border, frame, desktop, or root fallback.
#[must_use]
pub const fn mouse_context_chain(context: MouseContext) -> &'static [MouseContext] {
    match context {
        MouseContext::Root => &[MouseContext::Root, MouseContext::Desktop],
        MouseContext::Desktop => &[MouseContext::Desktop, MouseContext::Root],
        MouseContext::Client => &[MouseContext::Client, MouseContext::Frame],
        MouseContext::Frame => &[MouseContext::Frame],
        MouseContext::Titlebar => &[MouseContext::Titlebar, MouseContext::Frame],
        MouseContext::Border => &[MouseContext::Border, MouseContext::Frame],
        MouseContext::Top => &[MouseContext::Top, MouseContext::Border, MouseContext::Frame],
        MouseContext::Bottom => &[
            MouseContext::Bottom,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::Left => &[
            MouseContext::Left,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::Right => &[
            MouseContext::Right,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::TopLeft => &[
            MouseContext::TopLeft,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::TopRight => &[
            MouseContext::TopRight,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::BottomLeft => &[
            MouseContext::BottomLeft,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::BottomRight => &[
            MouseContext::BottomRight,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::Minimize => &[
            MouseContext::Minimize,
            MouseContext::Titlebar,
            MouseContext::Frame,
        ],
        MouseContext::Maximize => &[
            MouseContext::Maximize,
            MouseContext::Titlebar,
            MouseContext::Frame,
        ],
        MouseContext::Close => &[
            MouseContext::Close,
            MouseContext::Titlebar,
            MouseContext::Frame,
        ],
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

impl std::fmt::Display for RgbColor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "#{:06x}", self.0)
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

/// Returns the path used for persistent window-session state.
///
/// `NOBOX_STATE_FILE` overrides the XDG location.
///
/// # Errors
///
/// Returns an error when neither `XDG_STATE_HOME` nor `HOME` is usable.
pub fn state_path() -> Result<PathBuf, ConfigError> {
    if let Some(path) = env::var_os("NOBOX_STATE_FILE").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(config) = env::var_os("NOBOX_CONFIG_FILE").filter(|value| !value.is_empty()) {
        let config = PathBuf::from(config);
        return Ok(config.parent().map_or_else(
            || PathBuf::from("session.toml"),
            |parent| parent.join("session.toml"),
        ));
    }
    if let Some(path) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path).join("nobox/session.toml"));
    }
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| path.join(".local/state/nobox/session.toml"))
        .ok_or(ConfigError::NoStateHome)
}

/// Configuration failures with actionable context.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The operating environment has no configuration home.
    #[error("neither XDG_CONFIG_HOME nor HOME is set")]
    NoConfigHome,
    /// Neither the XDG state base nor a home-directory fallback is available.
    #[error("neither XDG_STATE_HOME nor HOME is set")]
    NoStateHome,
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
    /// The privileged input-method command must identify one exact executable.
    #[error("Wayland input method executable must be an absolute path of at most 4096 bytes")]
    InvalidInputMethodExecutable,
    /// Keep inherited process arguments bounded and free of NUL bytes.
    #[error("Wayland input method arguments exceed the 16384-byte bound or contain NUL")]
    InvalidInputMethodArguments,
    /// Bound the process argument vector before launch.
    #[error("Wayland input method has {0} arguments; at most 32 are accepted")]
    TooManyInputMethodArguments(usize),
    /// Keep the switcher readable without allowing oversized X11 requests.
    #[error("focus switcher width {0}px is outside 160..=1024px")]
    InvalidSwitcherWidth(u32),
    /// Keep rows readable and bound popup geometry.
    #[error("focus switcher row height {0}px is outside 16..=64px")]
    InvalidSwitcherRowHeight(u32),
    /// Bound rendering work and popup height.
    #[error("focus switcher row count {0} is outside 1..=32")]
    InvalidSwitcherRows(u32),
    /// Keep menus readable without allowing oversized backend requests.
    #[error("menu width {0}px is outside 160..=1024px")]
    InvalidMenuWidth(u32),
    /// Keep menu rows readable and bounded.
    #[error("menu row height {0}px is outside 16..=64px")]
    InvalidMenuRowHeight(u32),
    /// Bound menu rendering work and popup height.
    #[error("menu row count {0} is outside 2..=32")]
    InvalidMenuRows(u32),
    /// Bound how long command-backed menu creation may wait.
    #[error("menu command timeout {0}ms is outside 50..=5000ms")]
    InvalidMenuCommandTimeout(u32),
    /// Keep the optional panel usable and bound its reserved screen edge.
    #[error("panel height {0}px is outside 20..=96px")]
    InvalidPanelHeight(u32),
    /// Bound empty edge space and keep useful room for components.
    #[error("panel padding {0}px exceeds 48px")]
    InvalidPanelPadding(u32),
    /// Bound gaps so configured components cannot trivially exhaust the panel.
    #[error("panel spacing {0}px exceeds 32px")]
    InvalidPanelSpacing(u32),
    /// Keep task buttons useful while bounding label rendering.
    #[error("panel task width {0}px is outside 80..=512px")]
    InvalidPanelTaskWidth(u32),
    /// Require a small, useful ordered component list.
    #[error("panel item count {0} is outside 1..=16")]
    InvalidPanelItems(usize),
    /// A component can occupy at most one position in the panel.
    #[error("panel item {0:?} appears more than once")]
    DuplicatePanelItem(PanelItem),
    /// Keep clock formatting bounded and single-line.
    #[error("panel clock format must be 1..=128 bytes of single-line text")]
    InvalidPanelClockFormat,
    /// Bound application discovery and launcher rendering work.
    #[error("{0} panel launchers exceed the maximum of 32")]
    TooManyPanelLaunchers(usize),
    /// Launcher references must be safe desktop-entry identifiers.
    #[error("panel launcher has an invalid desktop-entry id: {0:?}")]
    InvalidPanelLauncher(String),
    /// Each application may appear only once in the launcher list.
    #[error("panel launcher appears more than once: {0:?}")]
    DuplicatePanelLauncher(String),
    /// Bound the number of persistent menu definitions.
    #[error("{0} menus exceed the maximum of 64")]
    TooManyMenus(usize),
    /// A menu identifier is empty, too long, or not portable.
    #[error("menu {0} has an invalid id")]
    InvalidMenuId(usize),
    /// Menu identifiers are stable lookup keys and must be unique.
    #[error("menu id {0:?} is defined more than once")]
    DuplicateMenuId(String),
    /// A menu title must contain bounded displayable text.
    #[error("menu {0:?} has an invalid title")]
    InvalidMenuTitle(String),
    /// Command menus require one bounded non-empty shell command.
    #[error("command menu {0:?} has a missing or invalid command")]
    InvalidMenuCommand(String),
    /// Only command-backed menus may configure a shell command.
    #[error("non-command menu {0:?} cannot configure a command")]
    UnexpectedMenuCommand(String),
    /// Bound generated data before TOML parsing and menu allocation.
    #[error("command menu output is {0} bytes; the maximum is 65536")]
    CommandMenuOutputTooLarge(usize),
    /// Empty menus provide no useful interaction target.
    #[error("menu {0:?} has no entries")]
    EmptyMenu(String),
    /// A separator-only menu cannot be navigated or activated.
    #[error("menu {0:?} has no selectable entries")]
    MenuHasNoSelectableEntry(String),
    /// Dynamic menu sources own their complete runtime entry list.
    #[error("dynamic menu {0:?} cannot also contain configured entries")]
    DynamicMenuHasEntries(String),
    /// Bound one menu's rendering and traversal work.
    #[error("menu {menu:?} has {count} entries; the maximum is 256")]
    TooManyMenuEntries {
        /// Menu identifier.
        menu: String,
        /// Configured entry count.
        count: usize,
    },
    /// Item and separator text must be bounded and displayable.
    #[error("menu {menu:?} entry {entry} has an invalid label")]
    InvalidMenuLabel {
        /// Menu identifier.
        menu: String,
        /// One-based entry position.
        entry: usize,
    },
    /// Bound the ordered work triggered by one selection.
    #[error("menu {menu:?} entry {entry} has {count} actions; the maximum is 16")]
    TooManyMenuEntryActions {
        /// Menu identifier.
        menu: String,
        /// One-based entry position.
        entry: usize,
        /// Configured action count.
        count: usize,
    },
    /// Named menu references must resolve at validation time.
    #[error("{context} references unknown menu {menu:?}")]
    UnknownMenu {
        /// Binding or menu entry that contains the reference.
        context: String,
        /// Missing menu identifier.
        menu: String,
    },
    /// Submenu graphs must terminate.
    #[error("menu graph contains a cycle through {0:?}")]
    CyclicMenu(String),
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
    /// Bound compatibility aliases before compiling passive grabs.
    #[error("mouse compatibility modifier count {0} exceeds the maximum of 4")]
    TooManyMouseCompatibilityModifiers(usize),
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
    /// Keep inherited pointer-binding omissions bounded.
    #[error("disabled mouse binding count {0} exceeds the maximum of 256")]
    TooManyDisabledMouseBindings(usize),
    /// One inherited pointer binding only needs to be omitted once.
    #[error("duplicate disabled mouse binding for {0}")]
    DuplicateDisabledMouseBinding(String),
    /// Key-chain timeouts must be responsive without permitting overflow-prone values.
    #[error("keyboard chain timeout {0}ms is outside 100..=60000ms")]
    InvalidChainTimeout(u32),
    /// XKB names must remain bounded, printable strings safe to pass to xkbcommon.
    #[error("keyboard XKB {0} must be at most 255 printable ASCII bytes")]
    InvalidKeyboardXkbField(&'static str),
    /// Keep passive grabs and the compiled input tree bounded.
    #[error("keyboard binding count {0} exceeds the maximum of 256")]
    TooManyKeyBindings(usize),
    /// Keep inherited-binding omissions bounded.
    #[error("disabled keyboard binding count {0} exceeds the maximum of 256")]
    TooManyDisabledKeyBindings(usize),
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
    /// Keep the X11 font request bounded and portable.
    #[error("theme font must be 1..=255 printable ASCII bytes")]
    InvalidThemeFont,
    /// Prevent title padding from consuming an entire normal titlebar.
    #[error("title padding {0}px exceeds the maximum of 64 pixels")]
    TitlePaddingTooWide(u32),
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
    /// The same inherited key binding was disabled more than once.
    #[error("keyboard binding {0} is disabled more than once")]
    DuplicateDisabledKeyBinding(String),
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
    /// Execute actions must contain one bounded, NUL-free command.
    #[error("execute action for {0} requires a non-empty command of at most 16384 bytes")]
    InvalidCommand(String),
    /// Standard semantic actions require bounded shell commands.
    #[error("configured {0} command must be at most 16384 bytes and non-empty when required")]
    InvalidConfiguredCommand(&'static str),
    /// Execute confirmation text must fit the native prompt.
    #[error("execute action for {0} has invalid confirmation text")]
    InvalidExecutePrompt(String),
    /// Startup notification fields must be bounded displayable text.
    #[error("execute action for {0} has invalid startup-notification metadata")]
    InvalidStartupNotification(String),
    /// Restart replacement commands must contain a command when specified.
    #[error("restart action for {0} has an empty command")]
    EmptyRestartCommand(String),
    /// Debug messages must be visible, bounded text.
    #[error("debug action for {0} requires a non-empty message of at most 1024 bytes")]
    InvalidDebugMessage(String),
    /// Conditional action trees must contain at least one explicit query.
    #[error("conditional action for {0} has no queries")]
    EmptyActionQueries(String),
    /// Conditional matcher patterns cannot be empty.
    #[error("conditional action for {context} has an empty {field} pattern")]
    EmptyActionQueryPattern {
        /// Binding or menu location containing the invalid query.
        context: String,
        /// Query field containing the empty pattern.
        field: &'static str,
    },
    /// Conditional workspace predicates must reference configured workspaces.
    #[error(
        "conditional action for {context} references workspace {workspace}, but count is {count}"
    )]
    InvalidActionQueryWorkspace {
        /// Binding or menu location containing the invalid query.
        context: String,
        /// Invalid one-based workspace number.
        workspace: u32,
        /// Configured workspace count.
        count: usize,
    },
    /// Recursive action trees are bounded to keep parsing and dispatch safe.
    #[error("action tree for {context} is nested too deeply at depth {depth}")]
    ActionNestingTooDeep {
        /// Binding or menu location containing the invalid tree.
        context: String,
        /// Observed zero-based nesting depth.
        depth: usize,
    },
    /// Recursive action trees have a strict total action limit.
    #[error("action tree for {0} contains too many nested actions")]
    ActionTreeTooLarge(String),
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
    /// Rules without a matcher would unintentionally affect every client.
    #[error("application rule {0} must contain at least one match field")]
    EmptyApplicationMatcher(usize),
    /// Empty patterns are ambiguous and almost always accidental.
    #[error("application rule {0} contains an empty match pattern")]
    EmptyApplicationPattern(usize),
    /// A size block without dimensions cannot change client geometry.
    #[error("application rule {0} size must contain width or height")]
    EmptyApplicationSize(usize),
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
    /// A socket path must be absolute for the manager to publish it.
    #[error("agent socket path {0} must be absolute")]
    AgentSocketNotAbsolute(PathBuf),
    /// UNIX socket paths are bounded by the platform, not by nobox.
    #[error("agent socket path is {length} bytes, above the {limit} byte platform limit")]
    AgentSocketTooLong {
        /// Configured path length.
        length: usize,
        /// Platform maximum.
        limit: usize,
    },
    /// Keep the human-suppression window usable.
    #[error("agent suppression window {0}ms is above the 60000ms maximum")]
    InvalidSuppressionWindow(u32),
    /// Bound the work a configuration reload performs.
    #[error("{0} agent grants exceed the supported maximum")]
    TooManyAgentGrants(usize),
    /// A grant binds to a verified executable, so it must name a real one.
    #[error("agent grant {0} must name an absolute executable path")]
    AgentGrantExecutable(usize),
    /// A grant that confers nothing is a configuration mistake, not a denial.
    #[error("agent grant {0} lists no capabilities")]
    AgentGrantWithoutCapabilities(usize),
    /// An empty scope would silently widen a grant to every client.
    #[error("agent grant {0} scope must contain a non-empty match pattern")]
    EmptyAgentGrantScope(usize),
    /// Bound launch-policy list sizes.
    #[error("{0} launch entries exceed the supported maximum")]
    TooManyLaunchEntries(usize),
    /// Launch policy names catalog identifiers, never paths.
    #[error("launch entry {0:?} is not a desktop-entry identifier")]
    InvalidLaunchEntry(String),
    /// The initial workspace must exist in the configured set.
    #[error("initial workspace {workspace} is invalid for {count} configured workspaces")]
    InvalidInitialWorkspace {
        /// Invalid one-based workspace number.
        workspace: u32,
        /// Configured workspace count.
        count: usize,
    },
    /// Bound reserved edges before they reach backend geometry.
    #[error("{edge} screen margin {pixels} exceeds the maximum of 16384 pixels")]
    MarginTooLarge {
        /// Margin edge name.
        edge: &'static str,
        /// Invalid pixel count.
        pixels: u32,
    },
    /// Bound persistent connector rules and topology work.
    #[error("output rule count {0} exceeds the maximum of 32")]
    TooManyOutputs(usize),
    /// Connector names must be bounded portable lookup keys.
    #[error("output rule {0} has an invalid connector name")]
    InvalidOutputName(usize),
    /// One connector can have only one persistent rule.
    #[error("output connector {0:?} is configured more than once")]
    DuplicateOutputName(String),
    /// Primary selection is unambiguous.
    #[error("output rules {first} and {second} are both marked primary")]
    MultiplePrimaryOutputs {
        /// First one-based primary rule.
        first: usize,
        /// Conflicting one-based primary rule.
        second: usize,
    },
    /// A disabled connector cannot be the preferred primary.
    #[error("disabled output {0:?} cannot be primary")]
    DisabledPrimaryOutput(String),
    /// Logical positions are bounded before backend arithmetic.
    #[error("output {0:?} position exceeds the +/-1000000 logical-pixel bound")]
    OutputPositionOutOfRange(String),
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
    fn mouse_drag_modifiers_include_alt_compatibility_without_duplicates() {
        let defaults = Config::default();
        assert_eq!(
            defaults.mouse.effective_modifiers(),
            [MouseModifier::Super, MouseModifier::Alt]
        );

        let configured =
            Config::parse("[mouse]\nmodifier = 'alt'\ncompatibility_modifiers = ['alt', 'super']")
                .expect("valid compatible mouse modifiers");
        assert_eq!(
            configured.mouse.effective_modifiers(),
            [MouseModifier::Alt, MouseModifier::Super]
        );
    }

    #[test]
    fn mouse_compatibility_modifier_count_is_bounded() {
        let error = Config::parse(
            "[mouse]\ncompatibility_modifiers = ['alt', 'super', 'alt', 'super', 'alt']",
        )
        .expect_err("excessive compatibility aliases must fail");
        assert!(matches!(
            error,
            ConfigError::TooManyMouseCompatibilityModifiers(5)
        ));
    }

    #[test]
    fn configured_mouse_bindings_layer_over_shared_defaults() {
        let config = Config::parse(
            "[mouse]\n\
             disabled_bindings = [{ context = 'client', button = 'Middle', trigger = 'press' }]\n\
             [[mouse.bindings]]\ncontext = 'client'\nbutton = 'Left'\ntrigger = 'press'\n\
             action = { type = 'lower' }",
        )
        .expect("valid layered mouse bindings");
        let bindings = config.mouse.effective_bindings();
        assert_eq!(bindings.len(), 42);
        assert!(!bindings.iter().any(|binding| {
            binding.context == MouseContext::Client
                && binding.button == MouseChord::new([], MouseButton::Middle)
                && binding.trigger == MouseTrigger::Press
        }));
        assert!(bindings.iter().any(|binding| {
            binding.context == MouseContext::Client
                && binding.button == MouseChord::new([], MouseButton::Left)
                && binding.trigger == MouseTrigger::Press
                && binding.actions == [Action::Lower]
        }));
    }

    #[test]
    fn mouse_context_fallbacks_are_shared_in_specific_to_general_order() {
        assert_eq!(
            mouse_context_chain(MouseContext::BottomRight),
            &[
                MouseContext::BottomRight,
                MouseContext::Border,
                MouseContext::Frame,
            ]
        );
        assert_eq!(
            mouse_context_chain(MouseContext::Close),
            &[
                MouseContext::Close,
                MouseContext::Titlebar,
                MouseContext::Frame,
            ]
        );
        assert_eq!(
            mouse_context_chain(MouseContext::Client),
            &[MouseContext::Client, MouseContext::Frame]
        );
    }

    #[test]
    fn inherited_mouse_bindings_can_be_replaced_or_disabled_once() {
        let replaced = Config::parse(
            "[mouse]\ninherit_defaults = false\n\
             [[mouse.bindings]]\ncontext = 'root'\nbutton = 'Up'\ntrigger = 'click'\n\
             action = { type = 'next_workspace' }",
        )
        .expect("valid replacement mouse map");
        assert_eq!(replaced.mouse.effective_bindings().len(), 1);

        let duplicate = Config::parse(
            "[mouse]\ndisabled_bindings = [\n\
             { context = 'client', button = 'Left', trigger = 'press' },\n\
             { context = 'client', button = 'Button1', trigger = 'press' }]",
        )
        .expect_err("duplicate inherited omissions must fail");
        assert!(matches!(
            duplicate,
            ConfigError::DuplicateDisabledMouseBinding(_)
        ));
    }

    #[test]
    fn excessive_edge_resistance_is_rejected() {
        let error = Config::parse("[mouse]\nedge_resistance = 257")
            .expect_err("oversized resistance must fail");
        assert!(matches!(error, ConfigError::EdgeResistanceTooStrong(257)));
    }

    #[test]
    fn window_snapping_defaults_on_and_can_be_disabled() {
        assert!(
            Config::parse("")
                .expect("defaults parse")
                .mouse
                .snap_to_windows
        );
        assert!(
            !Config::parse("[mouse]\nsnap_to_windows = false")
                .expect("window snapping can be disabled")
                .mouse
                .snap_to_windows
        );
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
        let interactive = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-m'\naction = { type = 'move' }\n\
             [[keyboard.bindings]]\nkey = 'W-r'\naction = { type = 'resize', edge = 'topleft' }",
        )
        .expect("interactive actions support keyboard mode and legacy edge names");
        assert_eq!(
            interactive.keyboard.bindings[1].actions,
            [Action::Resize {
                edge: Some(ResizeEdge::TopLeft),
            }]
        );
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
    fn panel_height_is_bounded_even_when_the_panel_is_disabled() {
        assert!(matches!(
            Config::parse("[panel]\nheight = 19"),
            Err(ConfigError::InvalidPanelHeight(19))
        ));
    }

    #[test]
    fn panel_layout_and_launchers_are_typed_and_bounded() {
        let config = Config::parse(
            "[panel]\nitems = ['workspaces', 'tasks', 'spacer', 'clock', 'launchers']\n\
             launchers = ['org.example.Terminal.desktop']\n\
             task_scope = 'all_workspaces'\nclock_format = '%a %H:%M'\n\
             padding = 12\nspacing = 6\ntask_max_width = 280",
        )
        .expect("valid panel customization");
        assert_eq!(config.panel.items[0], PanelItem::Workspaces);
        assert_eq!(config.panel.task_scope, PanelTaskScope::AllWorkspaces);
        assert_eq!(config.panel.launchers, ["org.example.Terminal.desktop"]);

        assert!(matches!(
            Config::parse("[panel]\nitems = ['clock', 'clock']"),
            Err(ConfigError::DuplicatePanelItem(PanelItem::Clock))
        ));
        assert!(matches!(
            Config::parse("[panel]\nlaunchers = ['../terminal.desktop']"),
            Err(ConfigError::InvalidPanelLauncher(_))
        ));
        assert!(matches!(
            Config::parse("[panel]\ntask_max_width = 79"),
            Err(ConfigError::InvalidPanelTaskWidth(79))
        ));
        assert!(matches!(
            Config::parse("[panel]\nclock_format = ''"),
            Err(ConfigError::InvalidPanelClockFormat)
        ));
        assert!(matches!(
            Config::parse("[panel]\nclock_format = '%Q'"),
            Err(ConfigError::InvalidPanelClockFormat)
        ));
    }

    #[test]
    fn theme_typography_is_typed_and_bounded() {
        let config = Config::parse(
            "[theme]\nfont = '-misc-fixed-medium-r-normal--13-120-75-75-c-70-iso8859-1'\n\
             title_alignment = 'center'\ntitle_padding = 12",
        )
        .expect("valid theme typography");
        assert_eq!(config.theme.title_alignment, TitleAlignment::Center);
        assert_eq!(config.theme.title_padding, 12);

        for font in ["", "   ", "bad\nfont", "å"] {
            assert!(matches!(
                Config::parse(&format!("[theme]\nfont = {font:?}")),
                Err(ConfigError::InvalidThemeFont)
            ));
        }
        assert!(matches!(
            Config::parse("[theme]\ntitle_padding = 65"),
            Err(ConfigError::TitlePaddingTooWide(65))
        ));
    }

    #[test]
    fn wayland_xkb_names_are_typed_and_bounded() {
        let config = Config::parse(
            "[keyboard]\nmodel = 'pc105'\nlayout = 'no'\nvariant = ''\noptions = 'compose:rwin'",
        )
        .expect("valid Norwegian XKB configuration");
        assert_eq!(config.keyboard.model, "pc105");
        assert_eq!(config.keyboard.layout, "no");
        assert_eq!(config.keyboard.options, "compose:rwin");

        assert!(matches!(
            Config::parse("[keyboard]\nlayout = \"no\\n\""),
            Err(ConfigError::InvalidKeyboardXkbField("layout"))
        ));
        assert!(matches!(
            Config::parse(&format!("[keyboard]\noptions = {:?}", "x".repeat(256))),
            Err(ConfigError::InvalidKeyboardXkbField("options"))
        ));
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
    fn menus_are_typed_and_validate_the_complete_graph() {
        let config = Config::parse(
            "[mouse]\ninherit_defaults = false\nbindings = []\n[keyboard]\ninherit_defaults = false\nbindings = []\n\
             [menu]\nwidth = 300\n\
             [[menu.definitions]]\nid = 'root'\ntitle = 'Root'\n\
             [[menu.definitions.entries]]\ntype = 'item'\nlabel = 'Terminal'\n\
             actions = [{ type = 'execute', command = 'xterm' }, { type = 'next_workspace' }]\n\
             [[menu.definitions.entries]]\ntype = 'submenu'\nlabel = 'More'\nmenu = 'more'\n\
             [[menu.definitions]]\nid = 'more'\ntitle = 'More'\n\
             [[menu.definitions.entries]]\ntype = 'item'\nlabel = 'Back to root'\n\
             action = { type = 'show_menu', menu = 'root' }",
        )
        .expect("valid named menu graph");
        assert_eq!(config.menu.width, 300);
        assert_eq!(config.menu.definitions.len(), 2);
        assert!(matches!(
            &config.menu.definitions[0].entries[0],
            MenuEntry::Item { actions, .. } if actions.len() == 2
        ));

        let unknown = Config::parse(
            "[mouse]\ninherit_defaults = false\nbindings = []\n[keyboard]\ninherit_defaults = false\nbindings = []\n\
             [[menu.definitions]]\nid = 'root'\ntitle = 'Root'\n\
             [[menu.definitions.entries]]\ntype = 'submenu'\nlabel = 'Missing'\nmenu = 'missing'",
        )
        .expect_err("unknown submenu must fail");
        assert!(matches!(unknown, ConfigError::UnknownMenu { .. }));

        let cycle = Config::parse(
            "[mouse]\ninherit_defaults = false\nbindings = []\n[keyboard]\ninherit_defaults = false\nbindings = []\n\
             [[menu.definitions]]\nid = 'one'\ntitle = 'One'\n\
             [[menu.definitions.entries]]\ntype = 'submenu'\nlabel = 'Two'\nmenu = 'two'\n\
             [[menu.definitions]]\nid = 'two'\ntitle = 'Two'\n\
             [[menu.definitions.entries]]\ntype = 'submenu'\nlabel = 'One'\nmenu = 'one'",
        )
        .expect_err("cyclic submenu graph must fail");
        assert!(matches!(cycle, ConfigError::CyclicMenu(_)));
    }

    #[test]
    fn menu_bounds_and_selectability_are_enforced() {
        assert!(matches!(
            Config::parse("[menu]\nmax_rows = 1"),
            Err(ConfigError::InvalidMenuRows(1))
        ));
        let separators = Config::parse(
            "[mouse]\ninherit_defaults = false\nbindings = []\n[keyboard]\ninherit_defaults = false\nbindings = []\n\
             [[menu.definitions]]\nid = 'root'\ntitle = 'Root'\n\
             [[menu.definitions.entries]]\ntype = 'separator'\nlabel = 'Nothing'",
        )
        .expect_err("separator-only menu must fail");
        assert!(matches!(
            separators,
            ConfigError::MenuHasNoSelectableEntry(_)
        ));

        let dynamic = Config::parse(
            "[mouse]\ninherit_defaults = false\nbindings = []\n[keyboard]\ninherit_defaults = false\nbindings = []\n\
             [[menu.definitions]]\nid = 'client'\ntitle = 'Window'\nsource = 'client'",
        )
        .expect("dynamic menus may omit configured entries");
        assert_eq!(dynamic.menu.definitions[0].source, MenuSource::Client);
        assert!(dynamic.menu.definitions[0].entries.is_empty());

        let dynamic_entries = Config::parse(
            "[mouse]\ninherit_defaults = false\nbindings = []\n[keyboard]\ninherit_defaults = false\nbindings = []\n\
             [[menu.definitions]]\nid = 'windows'\ntitle = 'Windows'\nsource = 'windows'\n\
             [[menu.definitions.entries]]\ntype = 'item'\nlabel = 'Invalid'\n\
             action = { type = 'exit' }",
        )
        .expect_err("dynamic menu entries must come from the selected source");
        assert!(matches!(
            dynamic_entries,
            ConfigError::DynamicMenuHasEntries(menu) if menu == "windows"
        ));
    }

    #[test]
    fn command_menus_reuse_the_strict_menu_and_action_schema() {
        let config = Config::parse(
            "[mouse]\ninherit_defaults = false\nbindings = []\n[keyboard]\ninherit_defaults = false\nbindings = []\n\
             [menu]\ncommand_timeout_ms = 250\n\
             [[menu.definitions]]\nid = 'generated'\ntitle = 'Generated'\n\
             source = 'command'\ncommand = 'menu-generator'\n\
             [[menu.definitions]]\nid = 'session'\ntitle = 'Session'\n\
             [[menu.definitions.entries]]\ntype = 'item'\nlabel = 'Exit'\n\
             action = { type = 'exit' }",
        )
        .expect("valid command-menu definition");
        let entries = config
            .parse_command_menu(
                "generated",
                "[[entries]]\ntype = 'item'\nlabel = '_Terminal'\n\
                 actions = [{ type = 'execute', command = 'xterm' }]\n\
                 [[entries]]\ntype = 'submenu'\nlabel = '_Session'\nmenu = 'session'",
            )
            .expect("generated entries use the configured menu schema");
        assert_eq!(entries.len(), 2);
        assert!(matches!(
            &entries[0],
            MenuEntry::Item { actions, .. }
                if matches!(actions.as_slice(), [Action::Execute { command, .. }] if command == "xterm")
        ));

        let cycle = config
            .parse_command_menu(
                "generated",
                "[[entries]]\ntype = 'submenu'\nlabel = 'Again'\nmenu = 'generated'",
            )
            .expect_err("generated submenu cycles must fail");
        assert!(matches!(cycle, ConfigError::CyclicMenu(menu) if menu == "generated"));

        let unknown_field = config
            .parse_command_menu(
                "generated",
                "[[entries]]\ntype = 'item'\nlabel = 'Exit'\naction = { type = 'exit' }\nextra = true",
            )
            .expect_err("generated entries remain strict TOML");
        assert!(matches!(unknown_field, ConfigError::Toml(_)));

        let oversized = "x".repeat(MAX_COMMAND_MENU_BYTES.saturating_add(1));
        assert!(matches!(
            config.parse_command_menu("generated", &oversized),
            Err(ConfigError::CommandMenuOutputTooLarge(size))
                if size == MAX_COMMAND_MENU_BYTES.saturating_add(1)
        ));
    }

    #[test]
    fn command_menu_definition_and_timeout_are_bounded() {
        assert!(matches!(
            Config::parse("[menu]\ncommand_timeout_ms = 49"),
            Err(ConfigError::InvalidMenuCommandTimeout(49))
        ));
        let missing = Config::parse(
            "[mouse]\nbindings = []\n[keyboard]\ninherit_defaults = false\nbindings = []\n\
             [[menu.definitions]]\nid = 'generated'\ntitle = 'Generated'\nsource = 'command'",
        )
        .expect_err("command source requires a command");
        assert!(matches!(
            missing,
            ConfigError::InvalidMenuCommand(menu) if menu == "generated"
        ));
        let static_command = Config::parse(
            "[mouse]\nbindings = []\n[keyboard]\ninherit_defaults = false\nbindings = []\n\
             [[menu.definitions]]\nid = 'root'\ntitle = 'Root'\ncommand = 'false'\n\
             [[menu.definitions.entries]]\ntype = 'item'\nlabel = 'Exit'\n\
             action = { type = 'exit' }",
        )
        .expect_err("static source cannot silently execute a command");
        assert!(matches!(
            static_command,
            ConfigError::UnexpectedMenuCommand(menu) if menu == "root"
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
    fn configured_keyboard_bindings_layer_over_shared_defaults() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-Left'\naction = { type = 'workspace_right' }",
        )
        .expect("valid override");
        let bindings = config.keyboard.effective_bindings();
        let action_for = |key: &str| {
            let key = key.parse::<KeySequence>().expect("valid test key");
            bindings
                .iter()
                .find(|binding| binding.key == key)
                .map(|binding| binding.actions.as_slice())
        };
        assert_eq!(
            action_for("W-Left"),
            Some([Action::WorkspaceRight { wrap: None }].as_slice())
        );
        assert_eq!(
            action_for("C-A-Left"),
            Some([Action::WorkspaceLeft { wrap: Some(false) }].as_slice())
        );
        assert_eq!(
            action_for("A-S-Right"),
            Some(
                [Action::MoveToWorkspaceRight {
                    follow: true,
                    wrap: Some(false),
                }]
                .as_slice()
            )
        );
    }

    #[test]
    fn common_commands_and_shortcuts_share_semantic_actions() {
        let config = Config::parse(
            "[commands]\nterminal = 'kitty'\nscreenshot = 'shot screen'\n\
             window_screenshot = 'shot window'\nsession = 'ssdd'\n\
             [shortcuts]\nterminal = 'W-F5'\nscreenshot = 'W-F6'\n\
             window_screenshot = 'W-F7'",
        )
        .expect("valid common command configuration");
        assert_eq!(config.commands.terminal, "kitty");
        assert_eq!(config.commands.session, "ssdd");
        let bindings = config.effective_key_bindings();
        let action_for = |key: &str| {
            let key = key.parse::<KeySequence>().expect("valid test key");
            bindings
                .iter()
                .find(|binding| binding.key == key)
                .map(|binding| binding.actions.as_slice())
        };
        assert_eq!(
            action_for("W-F5"),
            Some([Action::LaunchTerminal].as_slice())
        );
        assert_eq!(
            action_for("W-F6"),
            Some(
                [Action::Screenshot {
                    target: ScreenshotTarget::Screen,
                }]
                .as_slice()
            )
        );
        assert_eq!(
            action_for("W-F7"),
            Some(
                [Action::Screenshot {
                    target: ScreenshotTarget::Window,
                }]
                .as_slice()
            )
        );
        assert!(matches!(
            &config.menu.definitions[0].entries[1],
            MenuEntry::Item { actions, .. }
                if actions.as_slice() == [Action::LaunchTerminal]
        ));

        let legacy = Config::parse(
            "[commands]\nterminal = 'kitty'\n\
             [mouse]\ninherit_defaults = false\n\
             [keyboard]\ninherit_defaults = false\n\
             [[menu.definitions]]\nid = 'root'\ntitle = 'Root'\n\
             [[menu.definitions.entries]]\ntype = 'item'\nlabel = '_Terminal'\n\
             action = { type = 'execute', command = 'xterm' }",
        )
        .expect("legacy shipped terminal menu remains compatible");
        assert!(matches!(
            &legacy.menu.definitions[0].entries[0],
            MenuEntry::Item { actions, .. }
                if actions.as_slice() == [Action::LaunchTerminal]
        ));

        assert!(matches!(
            Config::parse("[commands]\nterminal = ''"),
            Err(ConfigError::InvalidConfiguredCommand("terminal"))
        ));
        assert!(matches!(
            Config::parse("[shortcuts]\nterminal = 'Print'"),
            Err(ConfigError::DuplicateKeyBinding(key)) if key == "Print"
        ));
    }

    #[test]
    fn inherited_keyboard_bindings_can_be_disabled_or_replaced_wholesale() {
        let selective = Config::parse("[keyboard]\ndisabled_bindings = ['C-A-Left']\n")
            .expect("valid inherited omission");
        assert!(
            selective
                .keyboard
                .effective_bindings()
                .iter()
                .all(|binding| binding.key.to_string() != "C-A-Left")
        );

        let replacement = Config::parse(
            "[keyboard]\ninherit_defaults = false\n\n\
             [[keyboard.bindings]]\nkey = 'W-F12'\naction = { type = 'close' }",
        )
        .expect("valid replacement keymap");
        let bindings = replacement.keyboard.effective_bindings();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].key.to_string(), "W-F12");
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
    fn polite_close_and_forced_kill_are_distinct_actions() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F11'\n\
             action = { type = 'close' }\n\
             [[keyboard.bindings]]\nkey = 'W-F12'\n\
             action = { type = 'kill' }",
        )
        .expect("valid close and kill actions");
        assert_eq!(config.keyboard.bindings[0].actions, [Action::Close]);
        assert_eq!(config.keyboard.bindings[1].actions, [Action::Kill]);
    }

    #[test]
    fn restart_action_supports_self_restart_and_validated_handoff() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F9'\n\
             action = { type = 'restart' }\n\
             [[keyboard.bindings]]\nkey = 'W-F10'\n\
             action = { type = 'restart', command = 'openbox --replace' }",
        )
        .expect("valid restart actions");
        assert_eq!(
            config.keyboard.bindings[0].actions,
            [Action::Restart { command: None }]
        );
        assert_eq!(
            config.keyboard.bindings[1].actions,
            [Action::Restart {
                command: Some("openbox --replace".to_owned()),
            }]
        );
        assert!(matches!(
            Config::parse(
                "[[keyboard.bindings]]\nkey = 'W-F9'\n\
                 action = { type = 'restart', command = '   ' }"
            ),
            Err(ConfigError::EmptyRestartCommand(_))
        ));
    }

    #[test]
    fn session_logout_defaults_to_confirmation_and_can_be_explicit() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F10'\n\
             action = { type = 'session_logout' }\n\
             [[keyboard.bindings]]\nkey = 'W-F11'\n\
             action = { type = 'session_logout', prompt = false }",
        )
        .expect("valid session logout actions");
        assert_eq!(
            config.keyboard.bindings[0].actions,
            [Action::SessionLogout { prompt: true }]
        );
        assert_eq!(
            config.keyboard.bindings[1].actions,
            [Action::SessionLogout { prompt: false }]
        );
    }

    #[test]
    fn local_exit_defaults_to_confirmation_and_can_be_immediate() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F10'\n\
             action = { type = 'exit' }\n\
             [[keyboard.bindings]]\nkey = 'W-F11'\n\
             action = { type = 'exit', prompt = false }",
        )
        .expect("valid local exit actions");
        assert_eq!(
            config.keyboard.bindings[0].actions,
            [Action::Exit { prompt: true }]
        );
        assert_eq!(
            config.keyboard.bindings[1].actions,
            [Action::Exit { prompt: false }]
        );
    }

    #[test]
    fn default_root_menu_ends_with_confirmed_local_exit() {
        let config = Config::default();
        let root = config
            .menu
            .definitions
            .iter()
            .find(|definition| definition.id == "root")
            .expect("default root menu");
        assert!(matches!(
            root.entries.as_slice(),
            [.., MenuEntry::Separator { label: None }, MenuEntry::Item { label, actions }]
                if label == "_Exit nobox"
                    && actions.as_slice() == [Action::Exit { prompt: true }]
        ));

        let session = config
            .menu
            .definitions
            .iter()
            .find(|definition| definition.id == "session")
            .expect("default session menu");
        assert!(!session.entries.iter().any(|entry| matches!(
            entry,
            MenuEntry::Item { actions, .. }
                if actions.iter().any(|action| matches!(action, Action::Exit { .. }))
        )));
    }

    #[test]
    fn show_desktop_defaults_to_launch_restoration_and_supports_strict_mode() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-d'\n\
             action = { type = 'toggle_show_desktop' }\n\
             [[keyboard.bindings]]\nkey = 'W-S-d'\n\
             action = { type = 'toggle_show_desktop', strict = true }",
        )
        .expect("valid show-desktop actions");
        assert_eq!(
            config.keyboard.bindings[0].actions,
            [Action::ToggleShowDesktop { strict: false }]
        );
        assert_eq!(
            config.keyboard.bindings[1].actions,
            [Action::ToggleShowDesktop { strict: true }]
        );
    }

    #[test]
    fn execute_supports_confirmation_and_portable_launch_metadata() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-Return'\n\
             action = { type = 'execute', command = 'terminal --cwd $pid', prompt = 'Open terminal here?', startup_notify = { name = 'Terminal', icon = 'utilities-terminal', wm_class = 'Terminal' } }",
        )
        .expect("valid enriched execute action");
        assert_eq!(
            config.keyboard.bindings[0].actions,
            [Action::Execute {
                command: "terminal --cwd $pid".to_owned(),
                prompt: Some("Open terminal here?".to_owned()),
                startup_notify: Some(StartupNotification {
                    name: Some("Terminal".to_owned()),
                    icon: Some("utilities-terminal".to_owned()),
                    wm_class: Some("Terminal".to_owned()),
                }),
            }]
        );

        for source in [
            "[[keyboard.bindings]]\nkey = 'W-Return'\naction = { type = 'execute', command = '   ' }",
            "[[keyboard.bindings]]\nkey = 'W-Return'\naction = { type = 'execute', command = 'xterm', prompt = '' }",
            "[[keyboard.bindings]]\nkey = 'W-Return'\naction = { type = 'execute', command = 'xterm', startup_notify = { wm_class = '' } }",
        ] {
            assert!(Config::parse(source).is_err());
        }
    }

    #[test]
    fn debug_action_requires_a_bounded_visible_message() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F10'\n\
             action = { type = 'debug', message = 'workspace switched' }",
        )
        .expect("valid debug action");
        assert_eq!(
            config.keyboard.bindings[0].actions,
            [Action::Debug {
                message: "workspace switched".to_owned(),
            }]
        );
        for message in [String::new(), "x".repeat(1_025)] {
            let source = format!(
                "[[keyboard.bindings]]\nkey = 'W-F10'\n\
                 action = {{ type = 'debug', message = {message:?} }}"
            );
            assert!(matches!(
                Config::parse(&source),
                Err(ConfigError::InvalidDebugMessage(_))
            ));
        }
    }

    #[test]
    fn conditional_action_trees_are_typed_and_bounded() {
        let config = Config::parse(
            "[workspaces]\nnames = ['one', 'two']\n\
             [[keyboard.bindings]]\nkey = 'W-F8'\n\
             action = { type = 'if', query = [{ class = 'Nobox*', shaded = true, workspace = 'current' }, { target = 'focused', active_workspace = 2 }], then = [{ type = 'raise' }, { type = 'stop' }], else = [{ type = 'lower' }] }\n\
             [[keyboard.bindings]]\nkey = 'W-F9'\n\
             action = { type = 'for_each', query = [{ kind = 'normal' }], then = [{ type = 'focus' }], none = [{ type = 'execute', command = 'notify-send none' }] }",
        )
        .expect("valid conditional actions");
        assert!(matches!(
            &config.keyboard.bindings[0].actions[0],
            Action::If {
                queries,
                then_actions,
                else_actions,
            } if queries.len() == 2
                && then_actions == &[Action::Raise, Action::Stop]
                && else_actions == &[Action::Lower]
        ));
        assert!(matches!(
            &config.keyboard.bindings[1].actions[0],
            Action::ForEach {
                queries,
                then_actions,
                none,
                ..
            } if queries.len() == 1
                && then_actions == &[Action::Focus { here: false }]
                && none.len() == 1
        ));

        assert!(matches!(
            Config::parse(
                "[[keyboard.bindings]]\nkey = 'W-F8'\n\
                 action = { type = 'if', query = [], then = [] }"
            ),
            Err(ConfigError::EmptyActionQueries(_))
        ));
        assert!(matches!(
            Config::parse(
                "[[keyboard.bindings]]\nkey = 'W-F8'\n\
                 action = { type = 'if', query = [{ class = '' }], then = [] }"
            ),
            Err(ConfigError::EmptyActionQueryPattern { .. })
        ));
        Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F8'\n\
             action = { type = 'if', query = [{}], then = [{ type = 'move' }] }",
        )
        .expect("nested keyboard move enters interactive keyboard mode");

        let mut nested = Action::Raise;
        for _ in 0..10 {
            nested = Action::If {
                queries: vec![ActionQuery::default()],
                then_actions: vec![nested],
                else_actions: Vec::new(),
            };
        }
        assert!(matches!(
            Config::default().validate_action(&nested, &|| "nested".to_owned()),
            Err(ConfigError::ActionNestingTooDeep { .. })
        ));
        let oversized = Action::If {
            queries: vec![ActionQuery::default()],
            then_actions: vec![Action::Raise; 129],
            else_actions: Vec::new(),
        };
        assert!(matches!(
            Config::default().validate_action(&oversized, &|| "oversized".to_owned()),
            Err(ConfigError::ActionTreeTooLarge(_))
        ));
    }

    #[test]
    fn action_queries_match_complete_protocol_neutral_facts() {
        let identity = ApplicationIdentity {
            name: "terminal",
            class: "NoboxTerm",
            group_name: "terminal-group",
            group_class: "NoboxTerm",
            role: "document",
            title: "Editor — notes",
            kind: ApplicationKind::Normal,
        };
        let context = ActionQueryContext {
            identity,
            workspace: Some(1),
            active_workspace: 1,
            last_workspace: 0,
            output: 2,
            shaded: false,
            maximized_horizontal: true,
            maximized_vertical: true,
            minimized: false,
            fullscreen: false,
            focused: true,
            focusable: true,
            urgent: true,
            decorated: true,
        };
        let query = ActionQuery {
            maximized: Some(true),
            focused: Some(true),
            urgent: Some(true),
            decorated: Some(true),
            sticky: Some(false),
            workspace: Some(ActionQueryWorkspace::Relative(
                ActionQueryWorkspaceRelation::Current,
            )),
            active_workspace: std::num::NonZeroU32::new(2),
            output: std::num::NonZeroU32::new(2),
            class: Some("nobox*".to_owned()),
            title: Some("*notes".to_owned()),
            kind: Some(ApplicationKind::Normal),
            ..ActionQuery::default()
        };
        assert!(query.matches(Some(context), 1));
        assert!(!query.matches(
            Some(ActionQueryContext {
                urgent: false,
                ..context
            }),
            1
        ));
        assert!(!query.matches(Some(context), 0));

        let active_only = ActionQuery {
            active_workspace: std::num::NonZeroU32::new(2),
            ..ActionQuery::default()
        };
        assert!(active_only.matches(None, 1));
        assert!(!ActionQuery::default().matches(None, 1));
    }

    #[test]
    fn client_state_actions_are_typed() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'A-F11'\n\
             action = { type = 'toggle_fullscreen' }\n\
             [[keyboard.bindings]]\nkey = 'A-F12'\n\
             action = { type = 'toggle_always_on_top' }\n\
             [[keyboard.bindings]]\nkey = 'A-S-F12'\n\
             action = { type = 'toggle_always_on_bottom' }\n\
             [[keyboard.bindings]]\nkey = 'A-F10'\n\
             action = { type = 'toggle_decorations' }\n\
             [[keyboard.bindings]]\nkey = 'A-F8'\n\
             action = { type = 'toggle_maximize_horizontal' }\n\
             [[keyboard.bindings]]\nkey = 'A-F9'\n\
             action = { type = 'toggle_maximize_vertical' }\n\
             [[keyboard.bindings]]\nkey = 'A-F5'\n\
             action = { type = 'raise_lower' }\n\
             [[keyboard.bindings]]\nkey = 'A-F6'\n\
             action = { type = 'shade_lower' }\n\
             [[keyboard.bindings]]\nkey = 'A-F7'\n\
             action = { type = 'unshade_raise' }",
        )
        .expect("valid typed client-state actions");
        assert_eq!(
            config
                .keyboard
                .bindings
                .iter()
                .map(|binding| binding.actions.as_slice())
                .collect::<Vec<_>>(),
            [
                [Action::ToggleFullscreen].as_slice(),
                [Action::ToggleAlwaysOnTop].as_slice(),
                [Action::ToggleAlwaysOnBottom].as_slice(),
                [Action::ToggleDecorations].as_slice(),
                [Action::ToggleMaximizeHorizontal].as_slice(),
                [Action::ToggleMaximizeVertical].as_slice(),
                [Action::RaiseLower].as_slice(),
                [Action::ShadeLower].as_slice(),
                [Action::UnshadeRaise].as_slice(),
            ]
        );
    }

    #[test]
    fn explicit_client_state_actions_are_typed() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F1'\n\
             action = { type = 'maximize' }\n\
             [[keyboard.bindings]]\nkey = 'W-F2'\n\
             action = { type = 'unmaximize', direction = 'horz' }\n\
             [[keyboard.bindings]]\nkey = 'W-F3'\n\
             action = { type = 'maximize', direction = 'vert' }\n\
             [[keyboard.bindings]]\nkey = 'W-F4'\n\
             action = { type = 'send_to_layer', layer = 'top' }\n\
             [[keyboard.bindings]]\nkey = 'W-F5'\n\
             action = { type = 'decorate' }\n\
             [[keyboard.bindings]]\nkey = 'W-F6'\n\
             action = { type = 'undecorate' }\n\
             [[keyboard.bindings]]\nkey = 'W-F7'\n\
             action = { type = 'shade' }\n\
             [[keyboard.bindings]]\nkey = 'W-F8'\n\
             action = { type = 'unshade' }",
        )
        .expect("valid explicit client-state actions");
        assert_eq!(
            config
                .keyboard
                .bindings
                .iter()
                .map(|binding| binding.actions.as_slice())
                .collect::<Vec<_>>(),
            [
                [Action::Maximize {
                    direction: MaximizeDirection::Both,
                }]
                .as_slice(),
                [Action::Unmaximize {
                    direction: MaximizeDirection::Horizontal,
                }]
                .as_slice(),
                [Action::Maximize {
                    direction: MaximizeDirection::Vertical,
                }]
                .as_slice(),
                [Action::SendToLayer {
                    layer: LayerTarget::Above,
                }]
                .as_slice(),
                [Action::Decorate].as_slice(),
                [Action::Undecorate].as_slice(),
                [Action::Shade].as_slice(),
                [Action::Unshade].as_slice(),
            ]
        );
    }

    #[test]
    fn focus_order_actions_are_typed() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F4'\n\
             action = { type = 'focus' }\n\
             [[keyboard.bindings]]\nkey = 'W-F5'\n\
             action = { type = 'focus_to_bottom' }\n\
             [[keyboard.bindings]]\nkey = 'W-F6'\n\
             action = { type = 'unfocus' }\n\
             [[keyboard.bindings]]\nkey = 'W-F7'\n\
             action = { type = 'focus_fallback' }",
        )
        .expect("valid focus-order actions");
        assert_eq!(
            config
                .keyboard
                .bindings
                .iter()
                .map(|binding| binding.actions.as_slice())
                .collect::<Vec<_>>(),
            [
                [Action::Focus { here: false }].as_slice(),
                [Action::FocusToBottom].as_slice(),
                [Action::Unfocus].as_slice(),
                [Action::FocusFallback].as_slice(),
            ]
        );

        let here = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F4'\n\
             action = { type = 'focus', here = true }",
        )
        .expect("valid focus-here action");
        assert_eq!(
            here.keyboard.bindings[0].actions,
            [Action::Focus { here: true }]
        );
    }

    #[test]
    fn directional_focus_actions_parse_all_spatial_directions() {
        let directions = [
            ("left", WindowDirection::Left),
            ("east", WindowDirection::Right),
            ("north", WindowDirection::Up),
            ("down", WindowDirection::Down),
            ("northwest", WindowDirection::UpLeft),
            ("up_right", WindowDirection::UpRight),
            ("south_west", WindowDirection::DownLeft),
            ("southeast", WindowDirection::DownRight),
        ];
        for (configured, expected) in directions {
            let source = format!(
                "[[keyboard.bindings]]\nkey = 'W-F5'\n\
                 action = {{ type = 'focus_direction', direction = '{configured}' }}"
            );
            let config = Config::parse(&source).expect("valid directional focus action");
            assert_eq!(
                config.keyboard.bindings[0].actions,
                [Action::FocusDirection {
                    direction: expected,
                }]
            );
        }
        let aliased = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F5'\n\
             action = { type = 'directional_target_window', direction = 'right' }",
        )
        .expect("Openbox-style action alias");
        assert_eq!(
            aliased.keyboard.bindings[0].actions,
            [Action::FocusDirection {
                direction: WindowDirection::Right,
            }]
        );
        let cycle = Config::parse(
            "[[keyboard.bindings]]\nkey = 'A-h'\n\
             action = { type = 'directional_cycle_windows', direction = 'northwest' }",
        )
        .expect("Openbox-style directional cycle alias");
        assert_eq!(
            cycle.keyboard.bindings[0].actions,
            [Action::CycleDirection {
                direction: WindowDirection::UpLeft,
            }]
        );
        assert!(
            Config::parse(
                "[[keyboard.bindings]]\nkey = 'W-F5'\n\
                 action = { type = 'focus_direction', direction = 'somewhere' }"
            )
            .is_err()
        );
    }

    #[test]
    fn relative_geometry_actions_parse_pixels_percentages_and_fractions() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F5'\n\
             action = { type = 'move_relative', x = 10, y = '-25%' }\n\
             [[keyboard.bindings]]\nkey = 'W-F6'\n\
             action = { type = 'resize_relative', left = '1/4', right = -5, bottom = '10%' }\n\
             [[keyboard.bindings]]\nkey = 'W-F7'\n\
             action = { type = 'move_to_edge', direction = 'west' }",
        )
        .expect("valid relative geometry actions");
        assert_eq!(
            config.keyboard.bindings[0].actions,
            [Action::MoveRelative {
                x: RelativeAmount::Pixels(10),
                y: RelativeAmount::Fraction {
                    numerator: -25,
                    denominator: NonZeroU32::new(100).unwrap(),
                },
            }]
        );
        assert_eq!(
            config.keyboard.bindings[2].actions,
            [Action::MoveToEdge {
                direction: EdgeDirection::Left,
            }]
        );
        assert_eq!(
            config.keyboard.bindings[1].actions,
            [Action::ResizeRelative {
                left: RelativeAmount::Fraction {
                    numerator: 1,
                    denominator: NonZeroU32::new(4).unwrap(),
                },
                right: RelativeAmount::Pixels(-5),
                top: RelativeAmount::Pixels(0),
                bottom: RelativeAmount::Fraction {
                    numerator: 10,
                    denominator: NonZeroU32::new(100).unwrap(),
                },
            }]
        );
        assert_eq!(RelativeAmount::Pixels(-12).resolve(800), -12);
        assert_eq!("-25%".parse::<RelativeAmount>().unwrap().resolve(600), -150);
        assert_eq!("1/4".parse::<RelativeAmount>().unwrap().resolve(801), 200);
    }

    #[test]
    fn malformed_relative_amounts_are_rejected() {
        for value in ["", " 10%", "10% ", "x%", "1/0", "1/-2", "1/2/3"] {
            assert!(
                value.parse::<RelativeAmount>().is_err(),
                "accepted {value:?}"
            );
        }
        assert!(
            Config::parse(
                "[[keyboard.bindings]]\nkey = 'W-F5'\n\
                 action = { type = 'move_relative', x = '1/0' }"
            )
            .is_err()
        );
    }

    #[test]
    fn directional_resize_actions_are_typed() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F8'\n\
             action = { type = 'grow_to_edge', direction = 'south' }\n\
             [[keyboard.bindings]]\nkey = 'W-F9'\n\
             action = { type = 'grow_to_fill' }\n\
             [[keyboard.bindings]]\nkey = 'W-F10'\n\
             action = { type = 'shrink_to_edge', direction = 'left' }",
        )
        .expect("valid directional resize actions");
        assert_eq!(
            config
                .keyboard
                .bindings
                .iter()
                .map(|binding| binding.actions.as_slice())
                .collect::<Vec<_>>(),
            [
                [Action::GrowToEdge {
                    direction: EdgeDirection::Down,
                }]
                .as_slice(),
                [Action::GrowToFill].as_slice(),
                [Action::ShrinkToEdge {
                    direction: EdgeDirection::Left,
                }]
                .as_slice(),
            ]
        );
    }

    #[test]
    fn absolute_geometry_actions_parse_typed_coordinates_sizes_and_outputs() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F3'\n\
             action = { type = 'move_resize_to', x = 'center', y = '-10%', width = '50%', height = 300, height_basis = 'content', output = 'next' }\n\
             [[keyboard.bindings]]\nkey = 'W-F4'\n\
             action = { type = 'move_resize_to', x = -25, output = 2 }\n\
             [[keyboard.bindings]]\nkey = 'W-F5'\n\
             action = { type = 'move_to_center' }",
        )
        .expect("valid absolute geometry actions");
        assert_eq!(
            config.keyboard.bindings[0].actions,
            [Action::MoveResizeTo {
                x: Some(AxisPosition::Center),
                y: Some(AxisPosition::End(RelativeAmount::Fraction {
                    numerator: 10,
                    denominator: NonZeroU32::new(100).unwrap(),
                })),
                width: Some(
                    PositiveRelativeAmount::try_from(RelativeAmount::Fraction {
                        numerator: 50,
                        denominator: NonZeroU32::new(100).unwrap(),
                    })
                    .unwrap(),
                ),
                height: Some(
                    PositiveRelativeAmount::try_from(RelativeAmount::Pixels(300)).unwrap(),
                ),
                width_basis: SizeBasis::Outer,
                height_basis: SizeBasis::Content,
                output: OutputTarget::Next,
            }]
        );
        assert_eq!(
            config.keyboard.bindings[1].actions,
            [Action::MoveResizeTo {
                x: Some(AxisPosition::End(RelativeAmount::Pixels(25))),
                y: None,
                width: None,
                height: None,
                width_basis: SizeBasis::Outer,
                height_basis: SizeBasis::Outer,
                output: OutputTarget::Index(NonZeroU32::new(2).unwrap()),
            }]
        );
        assert_eq!(
            config.keyboard.bindings[2].actions,
            [Action::MoveToCenter {
                output: OutputTarget::Current,
            }]
        );
    }

    #[test]
    fn absolute_geometry_actions_reject_invalid_dimensions_and_targets() {
        for source in [
            "[[keyboard.bindings]]\nkey = 'W-F3'\naction = { type = 'move_resize_to', width = 0 }",
            "[[keyboard.bindings]]\nkey = 'W-F3'\naction = { type = 'move_resize_to', height = '-10%' }",
            "[[keyboard.bindings]]\nkey = 'W-F3'\naction = { type = 'move_resize_to', x = '--10' }",
            "[[keyboard.bindings]]\nkey = 'W-F3'\naction = { type = 'move_resize_to', output = 0 }",
            "[[keyboard.bindings]]\nkey = 'W-F3'\naction = { type = 'move_to_center', output = 'unknown' }",
        ] {
            assert!(Config::parse(source).is_err(), "accepted {source:?}");
        }
        let tiny =
            PositiveRelativeAmount::try_from("1/100".parse::<RelativeAmount>().unwrap()).unwrap();
        assert_eq!(tiny.resolve(10), 1);
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
    fn runtime_workspace_actions_are_typed() {
        let config = Config::parse(
            "[[keyboard.bindings]]\nkey = 'W-F1'\n\
             action = { type = 'last_workspace' }\n\
             [[keyboard.bindings]]\nkey = 'W-F2'\n\
             action = { type = 'move_to_last_workspace' }\n\
             [[keyboard.bindings]]\nkey = 'W-F3'\n\
             action = { type = 'add_workspace' }\n\
             [[keyboard.bindings]]\nkey = 'W-F4'\n\
             action = { type = 'add_workspace', at = 'current' }\n\
             [[keyboard.bindings]]\nkey = 'W-F5'\n\
             action = { type = 'remove_workspace' }\n\
             [[keyboard.bindings]]\nkey = 'W-F6'\n\
             action = { type = 'remove_workspace', at = 'current' }",
        )
        .expect("valid runtime workspace actions");
        assert_eq!(
            config
                .keyboard
                .bindings
                .iter()
                .map(|binding| binding.actions.as_slice())
                .collect::<Vec<_>>(),
            [
                [Action::LastWorkspace].as_slice(),
                [Action::MoveToLastWorkspace { follow: true }].as_slice(),
                [Action::AddWorkspace {
                    at: WorkspacePlacement::Last,
                }]
                .as_slice(),
                [Action::AddWorkspace {
                    at: WorkspacePlacement::Current,
                }]
                .as_slice(),
                [Action::RemoveWorkspace {
                    at: WorkspacePlacement::Last,
                }]
                .as_slice(),
                [Action::RemoveWorkspace {
                    at: WorkspacePlacement::Current,
                }]
                .as_slice(),
            ]
        );
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
            "[workspaces]\nnames = ['code', 'web', 'chat', 'misc']\ncolumns = 2\nwrap = false\ninitial = 3",
        )
        .expect("valid two-column grid");
        assert_eq!(config.workspaces.columns, 2);
        assert!(!config.workspaces.wrap);
        assert_eq!(config.workspaces.initial, 3);

        let error = Config::parse("[workspaces]\nnames = ['one', 'two']\ncolumns = 3")
            .expect_err("oversized grid must fail");
        assert!(matches!(
            error,
            ConfigError::TooManyWorkspaceColumns {
                columns: 3,
                count: 2
            }
        ));

        let initial = Config::parse("[workspaces]\nnames = ['one', 'two']\ninitial = 3")
            .expect_err("missing initial workspace must fail");
        assert!(matches!(
            initial,
            ConfigError::InvalidInitialWorkspace {
                workspace: 3,
                count: 2
            }
        ));
    }

    #[test]
    fn configured_screen_margins_are_typed_and_bounded() {
        let config = Config::parse("[margins]\ntop = 10\nright = 20\nbottom = 30\nleft = 40")
            .expect("valid margins");
        assert_eq!(
            config.margins,
            MarginConfig {
                top: 10,
                right: 20,
                bottom: 30,
                left: 40,
            }
        );
        let error = Config::parse("[margins]\nleft = 16385").expect_err("hostile margin must fail");
        assert!(matches!(
            error,
            ConfigError::MarginTooLarge {
                edge: "left",
                pixels: 16_385
            }
        ));
    }

    #[test]
    fn output_rules_are_typed_exact_and_lookup_by_connector() {
        let config = Config::parse(
            "[[outputs.entries]]\n\
             name = 'eDP-1'\nenabled = true\nmode = '1920x1080@60'\n\
             position = { x = -1920, y = 0 }\ntransform = 'normal'\nscale = 1.25\nprimary = true\n\
             [[outputs.entries]]\nname = 'DP-1'\nenabled = false\ntransform = 'rotate90'\nscale = 1",
        )
        .expect("valid output rules");
        let internal = config.outputs.entry("eDP-1").expect("internal output");
        assert_eq!(
            internal.mode,
            Some(OutputModeConfig {
                width: 1920,
                height: 1080,
                refresh_millihz: Some(60_000),
            })
        );
        assert_eq!(internal.position, Some(OutputPosition { x: -1920, y: 0 }));
        assert_eq!(internal.scale.units(), 150);
        assert_eq!(internal.scale.factor(), 1.25);
        assert!(internal.primary);
        assert_eq!(
            config.outputs.entry("DP-1").map(|output| output.transform),
            Some(OutputTransform::Rotate90)
        );
        assert!(config.outputs.entry("HDMI-A-1").is_none());
    }

    #[test]
    fn output_rules_reject_ambiguous_or_hostile_topologies() {
        for source in [
            "[[outputs.entries]]\nname = '../card0'",
            "[[outputs.entries]]\nname = 'DP-1'\n[[outputs.entries]]\nname = 'DP-1'",
            "[[outputs.entries]]\nname = 'DP-1'\nprimary = true\n[[outputs.entries]]\nname = 'DP-2'\nprimary = true",
            "[[outputs.entries]]\nname = 'DP-1'\nenabled = false\nprimary = true",
            "[[outputs.entries]]\nname = 'DP-1'\nposition = { x = 1000001, y = 0 }",
            "[[outputs.entries]]\nname = 'DP-1'\nscale = 1.234",
            "[[outputs.entries]]\nname = 'DP-1'\nmode = '0x1080@60'",
            "[[outputs.entries]]\nname = 'DP-1'\nmode = '1920x1080@0'",
        ] {
            assert!(Config::parse(source).is_err(), "accepted {source:?}");
        }
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
            group_name: "navigator",
            group_class: "Firefox",
            role: "browser",
            title: "Private1",
            kind: ApplicationKind::Normal,
        });
        assert_eq!(
            settings.workspace,
            Some(ApplicationWorkspace::Index(NonZeroU32::new(2).unwrap()))
        );
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

        let size = Config::parse("[[applications]]\nmatch = { class = '*' }\nsize = {}")
            .expect_err("empty application size must fail");
        assert!(matches!(size, ConfigError::EmptyApplicationSize(1)));
    }

    #[test]
    fn application_rules_cover_group_state_and_geometry_policy() {
        let config = Config::parse(
            "[[applications]]\n\
             match = { group_name = 'suite-*', group_class = 'NoboxGroup' }\n\
             workspace = 'all'\nminimized = true\nshaded = false\n\
             skip_pager = true\nskip_taskbar = true\nfullscreen = false\n\
             maximized = 'vertical'\n\
             position = { x = 'center', y = -20, output = 'pointer', force = true }\n\
             size = { width = '75%', height = 480, width_basis = 'content' }",
        )
        .expect("complete application policy");
        let settings = config.application_settings(ApplicationIdentity {
            name: "dialog",
            class: "Client",
            group_name: "suite-editor",
            group_class: "noboxgroup",
            role: "document",
            title: "Notes",
            kind: ApplicationKind::Dialog,
        });

        assert_eq!(settings.workspace, Some(ApplicationWorkspace::All));
        assert_eq!(settings.minimized, Some(true));
        assert_eq!(settings.shaded, Some(false));
        assert_eq!(settings.skip_pager, Some(true));
        assert_eq!(settings.skip_taskbar, Some(true));
        assert_eq!(settings.fullscreen, Some(false));
        assert_eq!(settings.maximized, Some(ApplicationMaximized::Vertical));
        let position = settings.position.expect("position");
        assert_eq!(position.x, Some(AxisPosition::Center));
        assert_eq!(
            position.y,
            Some(AxisPosition::End(RelativeAmount::Pixels(20)))
        );
        assert_eq!(position.output, OutputTarget::Pointer);
        assert!(position.force);
        let size = settings.size.expect("size");
        assert_eq!(size.width.expect("width").resolve(800), 600);
        assert_eq!(size.height.expect("height").resolve(800), 480);
        assert_eq!(size.width_basis, SizeBasis::Content);
        assert_eq!(size.height_basis, SizeBasis::Outer);
    }

    #[test]
    fn wayland_input_method_is_an_absolute_bounded_argv() {
        assert!(!Config::default().wayland.xwayland);
        assert!(
            Config::parse("[wayland]\nxwayland = true")
                .expect("XWayland runtime opt-in")
                .wayland
                .xwayland
        );
        let config = Config::parse("[wayland]\ninput_method = ['/usr/bin/fcitx5', '--replace']")
            .expect("absolute input method argv");
        assert_eq!(
            config.wayland.input_method,
            ["/usr/bin/fcitx5", "--replace"]
        );

        assert!(matches!(
            Config::parse("[wayland]\ninput_method = ['fcitx5']"),
            Err(ConfigError::InvalidInputMethodExecutable)
        ));
        let arguments = std::iter::repeat_n("'x'", 33).collect::<Vec<_>>().join(",");
        assert!(matches!(
            Config::parse(&format!("[wayland]\ninput_method = [{arguments}]")),
            Err(ConfigError::TooManyInputMethodArguments(33))
        ));
        let oversized = "x".repeat(4_097);
        assert!(matches!(
            Config::parse(&format!(
                "[wayland]\ninput_method = ['/usr/bin/fcitx5', '{oversized}']"
            )),
            Err(ConfigError::InvalidInputMethodArguments)
        ));
    }
}
