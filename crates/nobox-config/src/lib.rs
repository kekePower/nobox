//! Loading, validation, discovery, and compatibility import for nobox configuration.

mod openbox_theme;

pub use openbox_theme::{OpenboxThemeImport, OpenboxThemeImportError};

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
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
        let definition = candidate
            .menu
            .definitions
            .iter_mut()
            .find(|definition| definition.id == menu)
            .ok_or_else(|| ConfigError::UnknownMenu {
                context: "command menu output".to_owned(),
                menu: menu.to_owned(),
            })?;
        definition.source = MenuSource::Static;
        definition.command = None;
        definition.entries = generated.entries.clone();
        candidate.validate()?;
        Ok(generated.entries)
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
        self.validate_menus()?;
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
                self.validate_action(action, binding.to_string(), true)?;
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
                self.validate_action(action, binding.key.to_string(), false)?;
            }
        }
        Ok(())
    }

    fn validate_action(
        &self,
        action: &Action,
        binding: String,
        pointer_allowed: bool,
    ) -> Result<(), ConfigError> {
        let mut actions = 0_usize;
        self.validate_action_tree(action, &binding, pointer_allowed, 0, &mut actions)
    }

    fn validate_action_tree(
        &self,
        action: &Action,
        binding: &str,
        pointer_allowed: bool,
        depth: usize,
        actions: &mut usize,
    ) -> Result<(), ConfigError> {
        const MAX_ACTION_DEPTH: usize = 8;
        const MAX_ACTION_TREE_ACTIONS: usize = 128;
        if depth > MAX_ACTION_DEPTH {
            return Err(ConfigError::ActionNestingTooDeep {
                context: binding.to_owned(),
                depth,
            });
        }
        *actions = actions.saturating_add(1);
        if *actions > MAX_ACTION_TREE_ACTIONS {
            return Err(ConfigError::ActionTreeTooLarge(binding.to_owned()));
        }
        if !pointer_allowed && matches!(action, Action::Move | Action::Resize) {
            return Err(ConfigError::PointerActionInKeyBinding {
                key: binding.to_owned(),
                action: match action {
                    Action::Move => "move",
                    Action::Resize => "resize",
                    _ => unreachable!(),
                },
            });
        }
        if let Action::Execute {
            command,
            prompt,
            startup_notify,
        } = action
        {
            if command.trim().is_empty() || command.contains('\0') || command.len() > 16_384 {
                return Err(ConfigError::InvalidCommand(binding.to_owned()));
            }
            if prompt.as_deref().is_some_and(|prompt| {
                prompt.trim().is_empty() || prompt.contains('\0') || prompt.len() > 255
            }) {
                return Err(ConfigError::InvalidExecutePrompt(binding.to_owned()));
            }
            if startup_notify
                .as_ref()
                .is_some_and(|notification| !notification.is_valid())
            {
                return Err(ConfigError::InvalidStartupNotification(binding.to_owned()));
            }
        }
        if let Action::Restart {
            command: Some(command),
        } = action
            && command.trim().is_empty()
        {
            return Err(ConfigError::EmptyRestartCommand(binding.to_owned()));
        }
        if let Action::Debug { message } = action
            && (message.trim().is_empty() || message.contains('\0') || message.len() > 1_024)
        {
            return Err(ConfigError::InvalidDebugMessage(binding.to_owned()));
        }
        match action {
            Action::If {
                queries,
                then_actions,
                else_actions,
            } => {
                self.validate_action_queries(queries, binding)?;
                for action in then_actions.iter().chain(else_actions) {
                    self.validate_action_tree(
                        action,
                        binding,
                        pointer_allowed,
                        depth.saturating_add(1),
                        actions,
                    )?;
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
                    self.validate_action_tree(
                        action,
                        binding,
                        pointer_allowed,
                        depth.saturating_add(1),
                        actions,
                    )?;
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
                        context: binding.to_owned(),
                        menu: menu.clone(),
                    });
                }
            }
            Action::Execute { .. }
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
            | Action::Resize
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
            | Action::WorkspaceLeft
            | Action::WorkspaceRight
            | Action::WorkspaceUp
            | Action::WorkspaceDown
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
                key: binding.to_owned(),
                workspace: workspace.unwrap_or_default(),
                count: self.workspaces.names.len(),
            });
        }
        Ok(())
    }

    fn validate_action_queries(
        &self,
        queries: &[ActionQuery],
        context: &str,
    ) -> Result<(), ConfigError> {
        if queries.is_empty() {
            return Err(ConfigError::EmptyActionQueries(context.to_owned()));
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
                        context: context.to_owned(),
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
                        context: context.to_owned(),
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
        if !(1..=32).contains(&self.menu.max_rows) {
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
                MenuSource::Client | MenuSource::ClientWorkspaces | MenuSource::Windows => {
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
                let context = format!("menu {} entry {}", definition.id, entry_index + 1);
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
                            self.validate_action(action, context.clone(), true)?;
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
                                context,
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
    /// Maximum visible rows before the active entry scrolls.
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
                        MenuEntry::Item {
                            label: "_Terminal".to_owned(),
                            actions: vec![Action::Execute {
                                command: "xterm".to_owned(),
                                prompt: None,
                                startup_notify: None,
                            }],
                        },
                        MenuEntry::Submenu {
                            label: "_Windows".to_owned(),
                            menu: "windows".to_owned(),
                        },
                        MenuEntry::Submenu {
                            label: "_Session".to_owned(),
                            menu: "session".to_owned(),
                        },
                    ],
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
                        MenuEntry::Item {
                            label: "_Exit nobox".to_owned(),
                            actions: vec![Action::Exit { prompt: true }],
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
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            border_width: 2,
            titlebar_height: 24,
            font: "fixed".to_owned(),
            title_alignment: TitleAlignment::Left,
            title_padding: 6,
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
            button_glyph: RgbColor::new(0x24, 0x27, 0x3a),
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
                    Action::Focus { here: false },
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
                    Action::Focus { here: false },
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
                MouseBinding::new(
                    MouseContext::Titlebar,
                    MouseChord::new([], MouseButton::Right),
                    MouseTrigger::Press,
                    [
                        Action::Focus { here: false },
                        Action::Raise,
                        Action::ShowMenu {
                            menu: "client".to_owned(),
                        },
                    ],
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
                    Action::Execute {
                        command: "xterm".to_owned(),
                        prompt: None,
                        startup_notify: None,
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
    /// Start an interactive move from the triggering pointer gesture.
    Move,
    /// Start an interactive resize from the triggering pointer gesture.
    Resize,
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
    /// Move the focused client to the previously active workspace.
    MoveToLastWorkspace {
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
    /// Exit the window manager without ending the surrounding desktop session.
    Exit {
        /// Show a grabbed confirmation prompt before releasing X11 ownership.
        #[serde(default = "default_true")]
        prompt: bool,
    },
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
#[error("invalid output target {0:?}; expected current, primary, next, previous, all, or N")]
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
    #[error("menu row count {0} is outside 1..=32")]
    InvalidMenuRows(u32),
    /// Bound how long command-backed menu creation may wait.
    #[error("menu command timeout {0}ms is outside 50..=5000ms")]
    InvalidMenuCommandTimeout(u32),
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
            "[mouse]\nbindings = []\n[keyboard]\nbindings = []\n\
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
            "[mouse]\nbindings = []\n[keyboard]\nbindings = []\n\
             [[menu.definitions]]\nid = 'root'\ntitle = 'Root'\n\
             [[menu.definitions.entries]]\ntype = 'submenu'\nlabel = 'Missing'\nmenu = 'missing'",
        )
        .expect_err("unknown submenu must fail");
        assert!(matches!(unknown, ConfigError::UnknownMenu { .. }));

        let cycle = Config::parse(
            "[mouse]\nbindings = []\n[keyboard]\nbindings = []\n\
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
            Config::parse("[menu]\nmax_rows = 0"),
            Err(ConfigError::InvalidMenuRows(0))
        ));
        let separators = Config::parse(
            "[mouse]\nbindings = []\n[keyboard]\nbindings = []\n\
             [[menu.definitions]]\nid = 'root'\ntitle = 'Root'\n\
             [[menu.definitions.entries]]\ntype = 'separator'\nlabel = 'Nothing'",
        )
        .expect_err("separator-only menu must fail");
        assert!(matches!(
            separators,
            ConfigError::MenuHasNoSelectableEntry(_)
        ));

        let dynamic = Config::parse(
            "[mouse]\nbindings = []\n[keyboard]\nbindings = []\n\
             [[menu.definitions]]\nid = 'client'\ntitle = 'Window'\nsource = 'client'",
        )
        .expect("dynamic menus may omit configured entries");
        assert_eq!(dynamic.menu.definitions[0].source, MenuSource::Client);
        assert!(dynamic.menu.definitions[0].entries.is_empty());

        let dynamic_entries = Config::parse(
            "[mouse]\nbindings = []\n[keyboard]\nbindings = []\n\
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
            "[mouse]\nbindings = []\n[keyboard]\nbindings = []\n\
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
            "[mouse]\nbindings = []\n[keyboard]\nbindings = []\n\
             [[menu.definitions]]\nid = 'generated'\ntitle = 'Generated'\nsource = 'command'",
        )
        .expect_err("command source requires a command");
        assert!(matches!(
            missing,
            ConfigError::InvalidMenuCommand(menu) if menu == "generated"
        ));
        let static_command = Config::parse(
            "[mouse]\nbindings = []\n[keyboard]\nbindings = []\n\
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
        assert!(matches!(
            Config::parse(
                "[[keyboard.bindings]]\nkey = 'W-F8'\n\
                 action = { type = 'if', query = [{}], then = [{ type = 'move' }] }"
            ),
            Err(ConfigError::PointerActionInKeyBinding { .. })
        ));

        let mut nested = Action::Raise;
        for _ in 0..10 {
            nested = Action::If {
                queries: vec![ActionQuery::default()],
                then_actions: vec![nested],
                else_actions: Vec::new(),
            };
        }
        assert!(matches!(
            Config::default().validate_action(&nested, "nested".to_owned(), false),
            Err(ConfigError::ActionNestingTooDeep { .. })
        ));
        let oversized = Action::If {
            queries: vec![ActionQuery::default()],
            then_actions: vec![Action::Raise; 129],
            else_actions: Vec::new(),
        };
        assert!(matches!(
            Config::default().validate_action(&oversized, "oversized".to_owned(), false),
            Err(ConfigError::ActionTreeTooLarge(_))
        ));
    }

    #[test]
    fn action_queries_match_complete_protocol_neutral_facts() {
        let identity = ApplicationIdentity {
            name: "terminal",
            class: "NoboxTerm",
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
             action = { type = 'move_to_last_workspace', follow = true }\n\
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
