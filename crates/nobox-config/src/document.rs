//! Format-preserving, validated editing for the canonical nobox configuration.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use crate::{
    Config, ConfigError, DEFAULT_CONFIG, LaunchPolicy, MAX_LAUNCH_ENTRIES, MAX_WORKSPACES,
    agent::is_desktop_entry_id,
};
use thiserror::Error;
use toml_edit::{Array, DocumentMut, Item, Table, Value, value as toml_value};

/// Maximum source size accepted by the graphical editor.
pub const MAX_SETTINGS_SOURCE_BYTES: usize = 1_048_576;

/// One setting exposed by the friendly editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingKey {
    /// Preferred terminal command.
    TerminalCommand,
    /// Full-screen screenshot command.
    ScreenshotCommand,
    /// Active-window screenshot command.
    WindowScreenshotCommand,
    /// Optional external session/logout dialog command.
    SessionCommand,
    /// Whether the window manager offers an agent seat at all.
    AgentEnabled,
    /// What happens when a companion connects with no stored grant.
    AgentPolicy,
    /// How long human input keeps agent input out, in milliseconds.
    AgentSuppressionMs,
    /// Chord that freezes every agent session at once.
    AgentKillChord,
    /// Additional traditional terminal shortcut.
    TerminalShortcut,
    /// Full-screen screenshot shortcut.
    ScreenshotShortcut,
    /// Active-window screenshot shortcut.
    WindowScreenshotShortcut,
    /// Focus newly opened windows.
    FocusNew,
    /// Focus windows as the pointer enters them.
    FollowMouse,
    /// Reject stale application focus requests.
    PreventFocusStealing,
    /// Raise windows when focus changes.
    RaiseOnFocus,
    /// Center smart placement inside a free field.
    CenterFreeSpace,
    /// Ordered workspace names.
    WorkspaceNames,
    /// Number of workspace grid columns.
    WorkspaceColumns,
    /// Wrap workspace navigation.
    WorkspaceWrap,
    /// Workspace selected when no saved session overrides it.
    InitialWorkspace,
    /// Reserved top screen edge.
    MarginTop,
    /// Reserved right screen edge.
    MarginRight,
    /// Reserved bottom screen edge.
    MarginBottom,
    /// Reserved left screen edge.
    MarginLeft,
    /// Show the focus-cycle overlay.
    SwitcherEnabled,
    /// Focus-cycle overlay width.
    SwitcherWidth,
    /// Focus-cycle overlay row height.
    SwitcherRowHeight,
    /// Maximum visible focus-cycle rows.
    SwitcherMaxRows,
    /// Menu width.
    MenuWidth,
    /// Menu row height.
    MenuRowHeight,
    /// Maximum menu entries per page, including an overflow continuation.
    MenuMaxRows,
    /// Start the optional panel with the session.
    PanelEnabled,
    /// Screen edge occupied by the panel.
    PanelPosition,
    /// Panel height in pixels.
    PanelHeight,
    /// Panel background color.
    PanelBackground,
    /// Panel text color.
    PanelForeground,
    /// Active workspace and task background.
    PanelActiveBackground,
    /// Urgent task background.
    PanelUrgentBackground,
    /// Outer panel padding.
    PanelPadding,
    /// Space between panel components.
    PanelSpacing,
    /// Maximum task-button width.
    PanelTaskMaxWidth,
    /// Workspaces represented in the task list.
    PanelTaskScope,
    /// Ordered panel components.
    PanelItems,
    /// Ordered desktop-entry launchers.
    PanelLaunchers,
    /// Local clock format.
    PanelClockFormat,
    /// Show workspace buttons in the panel.
    PanelShowWorkspaces,
    /// Show task buttons in the panel.
    PanelShowTasks,
    /// Show the local clock in the panel.
    PanelShowClock,
    /// Work-area edge resistance.
    EdgeResistance,
    /// Pointer drag threshold.
    DragThreshold,
    /// Double-click interval.
    DoubleClickMs,
    /// Decoration border width.
    BorderWidth,
    /// Decoration titlebar height.
    TitlebarHeight,
    /// X11 core font name.
    Font,
    /// Title alignment.
    TitleAlignment,
    /// Title text padding.
    TitlePadding,
    /// Focused border color.
    ActiveBorder,
    /// Unfocused border color.
    InactiveBorder,
    /// Urgent border color.
    UrgentBorder,
    /// Focused titlebar color.
    ActiveTitlebar,
    /// Unfocused titlebar color.
    InactiveTitlebar,
    /// Urgent titlebar color.
    UrgentTitlebar,
    /// Title text color.
    TitleText,
    /// Minimize button color.
    MinimizeButton,
    /// Maximize button color.
    MaximizeButton,
    /// Close button color.
    CloseButton,
    /// Button glyph color.
    ButtonGlyph,
}

impl SettingKey {
    const fn path(self) -> (&'static str, &'static str) {
        match self {
            Self::TerminalCommand => ("commands", "terminal"),
            Self::ScreenshotCommand => ("commands", "screenshot"),
            Self::WindowScreenshotCommand => ("commands", "window_screenshot"),
            Self::SessionCommand => ("commands", "session"),
            Self::AgentEnabled => ("agent", "enabled"),
            Self::AgentPolicy => ("agent", "policy"),
            Self::AgentSuppressionMs => ("agent", "suppression_ms"),
            Self::AgentKillChord => ("agent", "kill_chord"),
            Self::TerminalShortcut => ("shortcuts", "terminal"),
            Self::ScreenshotShortcut => ("shortcuts", "screenshot"),
            Self::WindowScreenshotShortcut => ("shortcuts", "window_screenshot"),
            Self::FocusNew => ("focus", "focus_new"),
            Self::FollowMouse => ("focus", "follow_mouse"),
            Self::PreventFocusStealing => ("focus", "prevent_focus_stealing"),
            Self::RaiseOnFocus => ("focus", "raise_on_focus"),
            Self::CenterFreeSpace => ("placement", "center_free_space"),
            Self::WorkspaceNames => ("workspaces", "names"),
            Self::WorkspaceColumns => ("workspaces", "columns"),
            Self::WorkspaceWrap => ("workspaces", "wrap"),
            Self::InitialWorkspace => ("workspaces", "initial"),
            Self::MarginTop => ("margins", "top"),
            Self::MarginRight => ("margins", "right"),
            Self::MarginBottom => ("margins", "bottom"),
            Self::MarginLeft => ("margins", "left"),
            Self::SwitcherEnabled => ("switcher", "enabled"),
            Self::SwitcherWidth => ("switcher", "width"),
            Self::SwitcherRowHeight => ("switcher", "row_height"),
            Self::SwitcherMaxRows => ("switcher", "max_rows"),
            Self::MenuWidth => ("menu", "width"),
            Self::MenuRowHeight => ("menu", "row_height"),
            Self::MenuMaxRows => ("menu", "max_rows"),
            Self::PanelEnabled => ("panel", "enabled"),
            Self::PanelPosition => ("panel", "position"),
            Self::PanelHeight => ("panel", "height"),
            Self::PanelBackground => ("panel", "background"),
            Self::PanelForeground => ("panel", "foreground"),
            Self::PanelActiveBackground => ("panel", "active_background"),
            Self::PanelUrgentBackground => ("panel", "urgent_background"),
            Self::PanelPadding => ("panel", "padding"),
            Self::PanelSpacing => ("panel", "spacing"),
            Self::PanelTaskMaxWidth => ("panel", "task_max_width"),
            Self::PanelTaskScope => ("panel", "task_scope"),
            Self::PanelItems => ("panel", "items"),
            Self::PanelLaunchers => ("panel", "launchers"),
            Self::PanelClockFormat => ("panel", "clock_format"),
            Self::PanelShowWorkspaces => ("panel", "show_workspaces"),
            Self::PanelShowTasks => ("panel", "show_tasks"),
            Self::PanelShowClock => ("panel", "show_clock"),
            Self::EdgeResistance => ("mouse", "edge_resistance"),
            Self::DragThreshold => ("mouse", "drag_threshold"),
            Self::DoubleClickMs => ("mouse", "double_click_ms"),
            Self::BorderWidth => ("theme", "border_width"),
            Self::TitlebarHeight => ("theme", "titlebar_height"),
            Self::Font => ("theme", "font"),
            Self::TitleAlignment => ("theme", "title_alignment"),
            Self::TitlePadding => ("theme", "title_padding"),
            Self::ActiveBorder => ("theme", "active_border"),
            Self::InactiveBorder => ("theme", "inactive_border"),
            Self::UrgentBorder => ("theme", "urgent_border"),
            Self::ActiveTitlebar => ("theme", "active_titlebar"),
            Self::InactiveTitlebar => ("theme", "inactive_titlebar"),
            Self::UrgentTitlebar => ("theme", "urgent_titlebar"),
            Self::TitleText => ("theme", "title_text"),
            Self::MinimizeButton => ("theme", "minimize_button"),
            Self::MaximizeButton => ("theme", "maximize_button"),
            Self::CloseButton => ("theme", "close_button"),
            Self::ButtonGlyph => ("theme", "button_glyph"),
        }
    }
}

/// Typed value accepted by one friendly setting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingValue {
    /// Boolean switch.
    Boolean(bool),
    /// Non-negative integer.
    Integer(u32),
    /// UTF-8 string.
    Text(String),
    /// Ordered UTF-8 string list.
    TextList(Vec<String>),
}

/// A format-preserving editable configuration document.
#[derive(Clone, Debug)]
pub struct SettingsDocument {
    document: DocumentMut,
}

/// Canonical format-preserving configuration document used by nobox tools.
pub type ConfigDocument = SettingsDocument;

impl SettingsDocument {
    /// Loads an existing file, or the commented defaults when it is absent.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, oversized, malformed, or invalid input.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        if path.exists() {
            let source = fs::read_to_string(path).map_err(|source| SettingsError::Read {
                path: path.to_path_buf(),
                source,
            })?;
            Self::parse(&source)
        } else {
            Self::parse(DEFAULT_CONFIG)
        }
    }

    /// Parses an editable source document through the canonical config model.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized, malformed, or invalid input.
    pub fn parse(source: &str) -> Result<Self, SettingsError> {
        check_source(source)?;
        let document = source.parse::<DocumentMut>()?;
        Ok(Self { document })
    }

    /// Returns the canonical typed view of the current document.
    ///
    /// # Errors
    ///
    /// Returns an error if advanced edits left the document invalid.
    pub fn config(&self) -> Result<Config, SettingsError> {
        Ok(Config::parse(&self.source())?)
    }

    /// Returns the complete format-preserved TOML source.
    #[must_use]
    pub fn source(&self) -> String {
        self.document.to_string()
    }

    /// Replaces the document after full canonical validation.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the document when input is invalid.
    pub fn replace_source(&mut self, source: &str) -> Result<(), SettingsError> {
        let replacement = Self::parse(source)?;
        *self = replacement;
        Ok(())
    }

    /// Applies one friendly control while retaining comments and unrelated data.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the document when the value has the
    /// wrong type or violates the canonical config model.
    pub fn set(&mut self, key: SettingKey, setting: SettingValue) -> Result<(), SettingsError> {
        validate_value_type(key, &setting)?;
        let mut candidate = self.document.clone();
        let (table, field) = key.path();
        candidate[table][field] = setting.into_item();
        check_source(&candidate.to_string())?;
        self.document = candidate;
        Ok(())
    }

    /// Stores one agent grant, keeping every comment and unrelated setting.
    ///
    /// The grant binds to the verified executable behind the socket. Nothing
    /// a companion declares about itself is written, because nothing it
    /// declares is an authorization input: a stored grant must not be
    /// reusable by whatever else can claim the same name.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the document when the executable is
    /// not absolute, no capability is named, or the result would be invalid.
    pub fn append_agent_grant(
        &mut self,
        label: &str,
        executable: &Path,
        uid: Option<u32>,
        capabilities: &[String],
    ) -> Result<(), SettingsError> {
        if !executable.is_absolute() {
            return Err(ConfigError::AgentGrantExecutable(0).into());
        }
        if capabilities.is_empty() {
            return Err(ConfigError::AgentGrantWithoutCapabilities(0).into());
        }
        let mut candidate = self.document.clone();
        let agent = candidate
            .entry("agent")
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        let Some(agent) = agent.as_table_mut() else {
            return Err(ConfigError::AgentGrantExecutable(0).into());
        };
        agent["enabled"] = toml_edit::value(true);
        let grants = agent
            .entry("grants")
            .or_insert(toml_edit::Item::ArrayOfTables(
                toml_edit::ArrayOfTables::new(),
            ));
        let Some(grants) = grants.as_array_of_tables_mut() else {
            return Err(ConfigError::AgentGrantExecutable(0).into());
        };
        let mut entry = toml_edit::Table::new();
        entry["label"] = toml_edit::value(label);
        entry["executable"] = toml_edit::value(executable.to_string_lossy().into_owned());
        if let Some(uid) = uid {
            entry["uid"] = toml_edit::value(i64::from(uid));
        }
        let mut atoms = toml_edit::Array::new();
        for capability in capabilities {
            atoms.push(capability.as_str());
        }
        entry["capabilities"] = toml_edit::value(atoms);
        grants.push(entry);
        check_source(&candidate.to_string())?;
        self.document = candidate;
        Ok(())
    }

    /// Removes one stored agent grant by position.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the document when no grant occupies
    /// that position, or when the result would be invalid.
    pub fn remove_agent_grant(&mut self, index: usize) -> Result<(), SettingsError> {
        let mut candidate = self.document.clone();
        let grants = candidate
            .get_mut("agent")
            .and_then(Item::as_table_mut)
            .and_then(|agent| agent.get_mut("grants"))
            .and_then(Item::as_array_of_tables_mut);
        let Some(grants) = grants.filter(|grants| index < grants.len()) else {
            return Err(ConfigError::AgentGrantExecutable(index + 1).into());
        };
        grants.remove(index);
        if grants.is_empty()
            && let Some(agent) = candidate.get_mut("agent").and_then(Item::as_table_mut)
        {
            agent.remove("grants");
        }
        check_source(&candidate.to_string())?;
        self.document = candidate;
        Ok(())
    }

    /// Changes which installed applications agents may launch without
    /// discarding either policy's configured list.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the document when the complete
    /// configuration would be invalid.
    pub fn set_agent_launch_policy(&mut self, policy: LaunchPolicy) -> Result<(), SettingsError> {
        let mut candidate = self.document.clone();
        ensure_agent_launch_table(&mut candidate);
        candidate["agent"]["launch"]["policy"] = toml_value(policy.as_str());
        check_source(&candidate.to_string())?;
        self.document = candidate;
        Ok(())
    }

    /// Enables or disables launches through user-writable desktop entries.
    ///
    /// Existing selections are retained while this is disabled so a
    /// temporarily unavailable or deliberately blocked entry is not erased.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the document when the complete
    /// configuration would be invalid.
    pub fn set_agent_launch_user_entries(&mut self, enabled: bool) -> Result<(), SettingsError> {
        let mut candidate = self.document.clone();
        ensure_agent_launch_table(&mut candidate);
        candidate["agent"]["launch"]["user_entries"] = toml_value(enabled);
        check_source(&candidate.to_string())?;
        self.document = candidate;
        Ok(())
    }

    /// Selects or deselects one desktop entry under the active launch policy.
    ///
    /// A selection is an allowed entry under [`LaunchPolicy::AllowListed`]
    /// and a blocked entry under [`LaunchPolicy::AllowInstalled`]. Unknown
    /// identifiers already in either list are preserved.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the document when launch policy is
    /// deny, the identifier is invalid, the active list is full, or the
    /// complete configuration would be invalid.
    pub fn set_agent_launch_selection(
        &mut self,
        desktop_id: &str,
        selected: bool,
    ) -> Result<(), SettingsError> {
        if !is_desktop_entry_id(desktop_id) {
            return Err(ConfigError::InvalidLaunchEntry(desktop_id.to_owned()).into());
        }
        let launch = self.config()?.agent.launch;
        let (field, mut entries) = match launch.policy {
            LaunchPolicy::Deny => return Err(SettingsError::LaunchSelectionWhileDenied),
            LaunchPolicy::AllowListed => ("allow", launch.allow),
            LaunchPolicy::AllowInstalled => ("deny", launch.deny),
        };
        if selected {
            if !entries.iter().any(|entry| entry == desktop_id) {
                if entries.len() >= MAX_LAUNCH_ENTRIES {
                    return Err(ConfigError::TooManyLaunchEntries(entries.len() + 1).into());
                }
                entries.push(desktop_id.to_owned());
            }
        } else {
            entries.retain(|entry| entry != desktop_id);
        }

        let mut candidate = self.document.clone();
        ensure_agent_launch_table(&mut candidate);
        candidate["agent"]["launch"][field] = SettingValue::TextList(entries).into_item();
        check_source(&candidate.to_string())?;
        self.document = candidate;
        Ok(())
    }

    /// Changes the workspace count while preserving existing names in order.
    ///
    /// New workspaces receive their one-based number as a name. Grid columns
    /// and the initial workspace are clamped when the count shrinks.
    ///
    /// # Errors
    ///
    /// Returns an error without changing the document when the count or the
    /// resulting complete configuration is invalid.
    pub fn set_workspace_count(&mut self, count: u32) -> Result<(), SettingsError> {
        let target =
            usize::try_from(count).map_err(|_| ConfigError::TooManyWorkspaces(usize::MAX))?;
        if target == 0 {
            return Err(ConfigError::NoWorkspaces.into());
        }
        if target > MAX_WORKSPACES {
            return Err(ConfigError::TooManyWorkspaces(target).into());
        }

        let workspace = self.config()?.workspaces;
        let mut names = workspace.names;
        names.truncate(target);
        while names.len() < target {
            names.push(names.len().saturating_add(1).to_string());
        }

        let mut candidate = self.document.clone();
        candidate["workspaces"]["names"] = SettingValue::TextList(names).into_item();
        candidate["workspaces"]["columns"] = toml_value(i64::from(workspace.columns.min(count)));
        candidate["workspaces"]["initial"] = toml_value(i64::from(workspace.initial.min(count)));
        check_source(&candidate.to_string())?;
        self.document = candidate;
        Ok(())
    }

    /// Atomically persists the current validated source with user-only mode.
    ///
    /// # Errors
    ///
    /// Returns an error when validation or durable replacement fails.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        save_source(path, &self.source())
    }
}

fn ensure_agent_launch_table(document: &mut DocumentMut) {
    let agent = document.entry("agent").or_insert(Item::Table(Table::new()));
    if let Some(agent) = agent.as_table_mut() {
        agent.entry("launch").or_insert(Item::Table(Table::new()));
    }
}

impl SettingValue {
    fn into_item(self) -> Item {
        match self {
            Self::Boolean(value) => toml_value(value),
            Self::Integer(value) => toml_value(i64::from(value)),
            Self::Text(value) => toml_value(value),
            Self::TextList(values) => {
                let mut array = Array::new();
                for entry in values {
                    array.push(entry);
                }
                Item::Value(Value::Array(array))
            }
        }
    }
}

fn validate_value_type(key: SettingKey, value: &SettingValue) -> Result<(), SettingsError> {
    let valid = match key {
        SettingKey::FocusNew
        | SettingKey::FollowMouse
        | SettingKey::PreventFocusStealing
        | SettingKey::RaiseOnFocus
        | SettingKey::CenterFreeSpace
        | SettingKey::WorkspaceWrap
        | SettingKey::SwitcherEnabled
        | SettingKey::PanelEnabled
        | SettingKey::PanelShowWorkspaces
        | SettingKey::PanelShowTasks
        | SettingKey::PanelShowClock
        | SettingKey::AgentEnabled => matches!(value, SettingValue::Boolean(_)),
        SettingKey::WorkspaceNames | SettingKey::PanelItems | SettingKey::PanelLaunchers => {
            matches!(value, SettingValue::TextList(_))
        }
        SettingKey::TerminalCommand
        | SettingKey::ScreenshotCommand
        | SettingKey::WindowScreenshotCommand
        | SettingKey::SessionCommand
        | SettingKey::TerminalShortcut
        | SettingKey::ScreenshotShortcut
        | SettingKey::WindowScreenshotShortcut
        | SettingKey::Font
        | SettingKey::TitleAlignment
        | SettingKey::AgentPolicy
        | SettingKey::AgentKillChord
        | SettingKey::PanelPosition
        | SettingKey::PanelTaskScope
        | SettingKey::PanelClockFormat
        | SettingKey::PanelBackground
        | SettingKey::PanelForeground
        | SettingKey::PanelActiveBackground
        | SettingKey::PanelUrgentBackground
        | SettingKey::ActiveBorder
        | SettingKey::InactiveBorder
        | SettingKey::UrgentBorder
        | SettingKey::ActiveTitlebar
        | SettingKey::InactiveTitlebar
        | SettingKey::UrgentTitlebar
        | SettingKey::TitleText
        | SettingKey::MinimizeButton
        | SettingKey::MaximizeButton
        | SettingKey::CloseButton
        | SettingKey::ButtonGlyph => matches!(value, SettingValue::Text(_)),
        SettingKey::WorkspaceColumns
        | SettingKey::InitialWorkspace
        | SettingKey::MarginTop
        | SettingKey::MarginRight
        | SettingKey::MarginBottom
        | SettingKey::MarginLeft
        | SettingKey::SwitcherWidth
        | SettingKey::SwitcherRowHeight
        | SettingKey::SwitcherMaxRows
        | SettingKey::MenuWidth
        | SettingKey::MenuRowHeight
        | SettingKey::MenuMaxRows
        | SettingKey::PanelHeight
        | SettingKey::PanelPadding
        | SettingKey::PanelSpacing
        | SettingKey::PanelTaskMaxWidth
        | SettingKey::EdgeResistance
        | SettingKey::DragThreshold
        | SettingKey::DoubleClickMs
        | SettingKey::BorderWidth
        | SettingKey::TitlebarHeight
        | SettingKey::TitlePadding
        | SettingKey::AgentSuppressionMs => matches!(value, SettingValue::Integer(_)),
    };
    if valid {
        Ok(())
    } else {
        Err(SettingsError::WrongValueType(key))
    }
}

fn check_source(source: &str) -> Result<(), SettingsError> {
    if source.len() > MAX_SETTINGS_SOURCE_BYTES {
        return Err(SettingsError::TooLarge(source.len()));
    }
    Config::parse(source)?;
    Ok(())
}

fn save_source(path: &Path, source: &str) -> Result<(), SettingsError> {
    check_source(source)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| SettingsError::NoParent(path.to_path_buf()))?;
    fs::create_dir_all(parent).map_err(|source| SettingsError::Write {
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = temporary_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(source.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        FileSync::sync_directory(parent)
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(SettingsError::Write {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

struct FileSync;

impl FileSync {
    fn sync_directory(path: &Path) -> std::io::Result<()> {
        fs::File::open(path)?.sync_all()
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.toml");
    path.with_file_name(format!(".{name}.nobox-settings-{}.tmp", std::process::id()))
}

/// Settings-document failure with actionable context.
#[derive(Debug, Error)]
pub enum SettingsError {
    /// The source exceeds the editor's explicit resource bound.
    #[error("configuration source is too large: {0} bytes")]
    TooLarge(usize),
    /// The source is not format-preserving TOML.
    #[error("configuration TOML is malformed")]
    Toml(#[from] toml_edit::TomlError),
    /// The canonical configuration rejected the source.
    #[error("configuration is invalid")]
    Config(#[from] ConfigError),
    /// A friendly control supplied an impossible value kind.
    #[error("wrong value type for {0:?}")]
    WrongValueType(SettingKey),
    /// Application membership has no meaning while launch policy is deny.
    #[error("applications cannot be selected while agent launch policy is deny")]
    LaunchSelectionWhileDenied,
    /// The target does not have a usable parent directory.
    #[error("configuration path has no parent: {0}")]
    NoParent(PathBuf),
    /// The source file could not be read.
    #[error("could not read settings from {path}")]
    Read {
        /// Attempted path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The validated source could not be durably replaced.
    #[error("could not write settings to {path}")]
    Write {
        /// Attempted path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Failure while reading, editing, validating, or saving a configuration document.
pub type ConfigDocumentError = SettingsError;

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU32, Ordering},
    };

    use super::*;

    static NEXT_DIRECTORY: AtomicU32 = AtomicU32::new(0);

    fn temporary_directory() -> PathBuf {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nobox-settings-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create isolated test directory");
        path
    }

    #[test]
    fn friendly_edits_preserve_comments_and_complex_bindings() {
        let mut document = SettingsDocument::parse(DEFAULT_CONFIG).expect("valid defaults");
        document
            .set(SettingKey::FollowMouse, SettingValue::Boolean(true))
            .expect("valid focus update");
        document
            .set(
                SettingKey::WorkspaceNames,
                SettingValue::TextList(vec!["code".to_owned(), "web".to_owned()]),
            )
            .expect("valid workspace update");
        document
            .set(SettingKey::InitialWorkspace, SettingValue::Integer(2))
            .expect("valid initial workspace update");
        document
            .set(SettingKey::MarginLeft, SettingValue::Integer(24))
            .expect("valid margin update");
        document
            .set(SettingKey::PanelShowClock, SettingValue::Boolean(false))
            .expect("valid panel update");
        document
            .set(
                SettingKey::PanelItems,
                SettingValue::TextList(
                    ["launchers", "tasks", "spacer", "clock"]
                        .map(str::to_owned)
                        .to_vec(),
                ),
            )
            .expect("valid panel order update");
        document
            .set(
                SettingKey::PanelLaunchers,
                SettingValue::TextList(vec!["org.example.Terminal.desktop".to_owned()]),
            )
            .expect("valid panel launcher update");
        document
            .set(
                SettingKey::PanelClockFormat,
                SettingValue::Text("%a %H:%M".to_owned()),
            )
            .expect("valid panel clock update");
        document
            .set(
                SettingKey::TerminalCommand,
                SettingValue::Text("kitty".to_owned()),
            )
            .expect("valid terminal update");
        document
            .set(
                SettingKey::SessionCommand,
                SettingValue::Text("ssdd".to_owned()),
            )
            .expect("valid session command update");
        document
            .set(
                SettingKey::TerminalShortcut,
                SettingValue::Text("W-F5".to_owned()),
            )
            .expect("valid shortcut update");
        let source = document.source();
        assert!(source.contains("# Focus clients as the pointer enters them."));
        assert!(source.contains("[[keyboard.bindings]]"));
        let config = document.config().expect("edited config remains valid");
        assert!(config.focus.follow_mouse);
        assert_eq!(config.workspaces.names, ["code", "web"]);
        assert_eq!(config.workspaces.initial, 2);
        assert_eq!(config.margins.left, 24);
        assert!(!config.panel.show_clock);
        assert_eq!(config.panel.items[0], crate::PanelItem::Launchers);
        assert_eq!(config.panel.launchers, ["org.example.Terminal.desktop"]);
        assert_eq!(config.panel.clock_format, "%a %H:%M");
        assert_eq!(config.commands.terminal, "kitty");
        assert_eq!(config.commands.session, "ssdd");
        assert_eq!(config.shortcuts.terminal.to_string(), "W-F5");
    }

    #[test]
    fn invalid_edit_rolls_back_exact_source() {
        let mut document = SettingsDocument::parse(DEFAULT_CONFIG).expect("valid defaults");
        let original = document.source();
        let error = document
            .set(SettingKey::BorderWidth, SettingValue::Integer(65))
            .expect_err("oversized border is rejected");
        assert!(matches!(error, SettingsError::Config(_)));
        assert_eq!(document.source(), original);
    }

    #[test]
    fn workspace_count_preserves_names_and_clamps_dependent_fields() {
        let mut document = SettingsDocument::parse(
            "# retained\n[workspaces]\nnames = ['code', 'web']\ncolumns = 2\ninitial = 2\n",
        )
        .expect("valid workspace settings");
        document
            .set_workspace_count(4)
            .expect("workspace count can grow");
        assert_eq!(
            document.config().unwrap().workspaces.names,
            ["code", "web", "3", "4"]
        );

        document
            .set_workspace_count(1)
            .expect("workspace count can shrink");
        let workspace = document.config().unwrap().workspaces;
        assert_eq!(workspace.names, ["code"]);
        assert_eq!(workspace.columns, 1);
        assert_eq!(workspace.initial, 1);
        assert!(document.source().starts_with("# retained\n"));
    }

    #[test]
    fn invalid_workspace_count_is_transactional() {
        let mut document = SettingsDocument::parse(DEFAULT_CONFIG).expect("valid defaults");
        let original = document.source();
        assert!(document.set_workspace_count(0).is_err());
        assert!(
            document
                .set_workspace_count(u32::try_from(MAX_WORKSPACES).unwrap_or(u32::MAX) + 1)
                .is_err()
        );
        assert_eq!(document.source(), original);
    }

    #[test]
    fn workspace_count_does_not_break_absolute_bindings() {
        let mut document = SettingsDocument::parse(
            "[workspaces]\nnames = ['one', 'two']\n\
             [[keyboard.bindings]]\nkey = 'W-2'\n\
             action = { type = 'switch_workspace', workspace = 2 }\n",
        )
        .expect("valid absolute workspace binding");
        let original = document.source();
        assert!(document.set_workspace_count(1).is_err());
        assert_eq!(document.source(), original);
    }

    #[test]
    fn wrong_friendly_value_kind_is_rejected() {
        let mut document = SettingsDocument::parse(DEFAULT_CONFIG).expect("valid defaults");
        let error = document
            .set(SettingKey::Font, SettingValue::Boolean(true))
            .expect_err("wrong type is rejected");
        assert!(matches!(
            error,
            SettingsError::WrongValueType(SettingKey::Font)
        ));
    }

    #[test]
    fn a_stored_grant_binds_to_the_executable_and_keeps_the_file_intact() {
        let mut document =
            SettingsDocument::parse("# keep me\n[focus]\nfollow_mouse = true\n").expect("parses");
        document
            .append_agent_grant(
                "my harness",
                Path::new("/usr/bin/nobox-agent"),
                Some(1000),
                &["observe".to_owned(), "manage.activate".to_owned()],
            )
            .expect("stored");
        let source = document.source();
        assert!(source.contains("# keep me"), "{source}");
        assert!(source.contains("follow_mouse = true"), "{source}");
        assert!(source.contains("[[agent.grants]]"), "{source}");
        assert!(source.contains("/usr/bin/nobox-agent"), "{source}");
        assert!(
            !source.contains("my harness\"\nharness"),
            "a declared name must never become a matching key"
        );

        let config = document.config().expect("valid");
        assert!(config.agent.enabled);
        assert_eq!(config.agent.grants.len(), 1);
        let held = config
            .agent
            .capabilities_for(Some(Path::new("/usr/bin/nobox-agent")), 1000);
        assert!(held.holds(nobox_agent_wire::Capability::ObserveStructure));
        assert!(held.holds(nobox_agent_wire::Capability::ManageActivate));
        assert!(
            config
                .agent
                .capabilities_for(Some(Path::new("/usr/bin/nobox-agent")), 1001)
                .is_empty(),
            "the stored user is part of the binding"
        );
    }

    #[test]
    fn a_stored_grant_can_be_taken_back() {
        let mut document = SettingsDocument::parse("# keep me\n").expect("parses");
        for name in ["first", "second"] {
            document
                .append_agent_grant(
                    name,
                    Path::new("/usr/bin/nobox-agent"),
                    None,
                    &["observe".to_owned()],
                )
                .expect("stored");
        }
        assert_eq!(document.config().expect("valid").agent.grants.len(), 2);
        document.remove_agent_grant(0).expect("removed");
        let config = document.config().expect("valid");
        assert_eq!(config.agent.grants.len(), 1);
        assert_eq!(config.agent.grants[0].label, "second");
        assert!(document.source().contains("# keep me"));
        assert!(
            document.remove_agent_grant(5).is_err(),
            "a position that holds no grant changes nothing"
        );
        document.remove_agent_grant(0).expect("removed");
        assert!(document.config().expect("valid").agent.grants.is_empty());
    }

    #[test]
    fn a_stored_grant_refuses_what_the_schema_would_refuse() {
        let mut document = SettingsDocument::parse("").expect("parses");
        assert!(
            document
                .append_agent_grant("x", Path::new("nobox-agent"), None, &["observe".to_owned()])
                .is_err()
        );
        assert!(
            document
                .append_agent_grant("x", Path::new("/usr/bin/nobox-agent"), None, &[])
                .is_err()
        );
        assert!(
            document
                .append_agent_grant(
                    "x",
                    Path::new("/usr/bin/nobox-agent"),
                    None,
                    &["observe.everything".to_owned()],
                )
                .is_err(),
            "an unknown capability is refused before it is written"
        );
        assert_eq!(document.source(), "", "a refused edit changes nothing");
    }

    #[test]
    fn launch_edits_are_typed_transactional_and_preserve_unknown_entries() {
        let mut document = SettingsDocument::parse(
            "# retained\n[agent.launch]\npolicy = 'allow_listed'\n\
             allow = ['missing.desktop']\ndeny = ['old.desktop']\nuser_entries = false\n",
        )
        .expect("valid launch policy");

        document
            .set_agent_launch_selection("org.example.Editor.desktop", true)
            .expect("select installed entry");
        document
            .set_agent_launch_policy(LaunchPolicy::AllowInstalled)
            .expect("change mode");
        document
            .set_agent_launch_selection("org.example.Editor.desktop", true)
            .expect("block installed entry");
        document
            .set_agent_launch_user_entries(true)
            .expect("enable user entries");

        let launch = document.config().expect("valid result").agent.launch;
        assert_eq!(launch.policy, LaunchPolicy::AllowInstalled);
        assert_eq!(
            launch.allow,
            ["missing.desktop", "org.example.Editor.desktop"]
        );
        assert_eq!(launch.deny, ["old.desktop", "org.example.Editor.desktop"]);
        assert!(launch.user_entries);
        assert!(document.source().starts_with("# retained\n"));
        assert!(document.source().contains("[agent.launch]"));

        document
            .set_agent_launch_selection("org.example.Editor.desktop", false)
            .expect("unblock installed entry");
        assert_eq!(
            document.config().expect("valid result").agent.launch.deny,
            ["old.desktop"]
        );
    }

    #[test]
    fn refused_launch_edits_leave_the_document_unchanged() {
        let mut document = SettingsDocument::parse(DEFAULT_CONFIG).expect("valid defaults");
        let original = document.source();
        assert!(matches!(
            document.set_agent_launch_selection("org.example.Editor.desktop", true),
            Err(SettingsError::LaunchSelectionWhileDenied)
        ));
        assert_eq!(document.source(), original);

        document
            .set_agent_launch_policy(LaunchPolicy::AllowListed)
            .expect("enable selection");
        let enabled = document.source();
        assert!(enabled.contains("[agent.launch]"));
        assert!(
            document
                .set_agent_launch_selection("../not-a-desktop-id", true)
                .is_err()
        );
        assert_eq!(document.source(), enabled);
    }

    #[test]
    fn launch_selection_bound_is_checked_before_editing() {
        let entries = (0..MAX_LAUNCH_ENTRIES)
            .map(|index| format!("entry-{index}.desktop"))
            .collect::<Vec<_>>()
            .join("', '");
        let source = format!("[agent.launch]\npolicy = 'allow_listed'\nallow = ['{entries}']\n");
        let mut document = SettingsDocument::parse(&source).expect("full list is valid");
        let original = document.source();
        assert!(
            document
                .set_agent_launch_selection("overflow.desktop", true)
                .is_err()
        );
        assert_eq!(document.source(), original);
    }

    #[test]
    fn atomic_save_creates_private_valid_file() {
        let directory = temporary_directory();
        let path = directory.join("nested/config.toml");
        let mut document = SettingsDocument::parse(DEFAULT_CONFIG).expect("valid defaults");
        document
            .set(SettingKey::TitlebarHeight, SettingValue::Integer(31))
            .expect("valid theme update");
        document.save(&path).expect("atomic save succeeds");
        let loaded = SettingsDocument::load(&path)
            .expect("saved file loads")
            .config()
            .expect("saved file validates");
        assert_eq!(loaded.theme.titlebar_height, 31);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!temporary_path(&path).exists());
        fs::remove_dir_all(directory).expect("remove isolated test directory");
    }

    #[test]
    fn advanced_replacement_is_transactional_and_bounded() {
        let mut document = SettingsDocument::parse(DEFAULT_CONFIG).expect("valid defaults");
        let original = document.source();
        assert!(
            document
                .replace_source("[focus]\nunknown = true\n")
                .is_err()
        );
        assert_eq!(document.source(), original);
        let oversized = "#".repeat(MAX_SETTINGS_SOURCE_BYTES + 1);
        assert!(matches!(
            SettingsDocument::parse(&oversized),
            Err(SettingsError::TooLarge(_))
        ));
    }
}
