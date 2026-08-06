//! The `[agent]` schema: whether an agent seat exists, and what it may do.
//!
//! Two rules shape this schema. Nothing is enabled by default, and a stored
//! grant binds to verified peer identity — the executable behind the socket —
//! never to a name a connecting process declares about itself. A declared
//! label is display text; it is deliberately not a matching key.

use std::path::{Path, PathBuf};

use agent_seat_proto::{Bundle, Capability, CapabilitySet};
use serde::{Deserialize, Deserializer};

use crate::{ApplicationMatcher, ConfigError, KeyChord, KeyboardModifier};

/// Longest usable UNIX socket path, from the platform's `sockaddr_un`.
pub const MAX_AGENT_SOCKET_PATH: usize = 107;

/// Most grants one configuration may declare.
pub const MAX_AGENT_GRANTS: usize = 64;

/// Most desktop-entry identifiers one launch list may hold.
pub const MAX_LAUNCH_ENTRIES: usize = 256;

/// Longest accepted desktop-entry identifier.
pub const MAX_DESKTOP_ENTRY_LEN: usize = 256;

/// Longest accepted human-suppression window.
pub const MAX_SUPPRESSION_MS: u32 = 60_000;

/// What happens when a companion connects without a stored grant.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentPolicy {
    /// Refuse every capability. The session still completes its handshake, so
    /// a harness learns it was denied rather than hanging.
    #[default]
    Deny,
    /// Ask the human with the manager's own consent dialog.
    Ask,
}

/// How visible a client is to agent sessions.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentVisibility {
    /// Ordinary: subject only to the session's grant and scope.
    #[default]
    Visible,
    /// Present with geometry but no title; capture and input are refused.
    Redacted,
    /// Absent from every response and event. Acting on its identity returns
    /// the same answer as a client that never existed.
    Hidden,
}

/// Which desktop entries an agent may start.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LaunchPolicy {
    /// Launch nothing.
    #[default]
    Deny,
    /// Launch only the entries named in `allow`.
    AllowListed,
    /// Launch any installed entry except those named in `deny`.
    AllowInstalled,
}

/// A capability written in configuration: either one atom or one consent
/// bundle standing for its atoms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrantedCapability {
    /// A consent bundle.
    Bundle(Bundle),
    /// A single atom.
    Atom(Capability),
}

impl GrantedCapability {
    /// Returns the atoms this entry grants.
    #[must_use]
    pub fn atoms(self) -> CapabilitySet {
        match self {
            Self::Bundle(bundle) => CapabilitySet::from_iter_atoms(bundle.atoms().iter().copied()),
            Self::Atom(atom) => CapabilitySet::EMPTY.with(atom),
        }
    }

    /// Returns the configured spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bundle(bundle) => bundle.as_str(),
            Self::Atom(atom) => atom.as_str(),
        }
    }
}

impl<'de> Deserialize<'de> for GrantedCapability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Bundle::from_wire(&name)
            .map(Self::Bundle)
            .or_else(|| Capability::from_wire(&name).map(Self::Atom))
            .ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "unknown agent capability {name:?}; use a bundle (observe, accessibility, \
                     capture, input, manage, launch) or an atom such as manage.activate"
                ))
            })
    }
}

/// One stored grant.
///
/// `executable` is the binding: the manager compares it against the executable
/// behind the connected socket, not against anything the peer says. On X11
/// this identification is informative rather than a hard boundary, since any
/// same-user process can bypass the manager entirely; it is specified and
/// enforced now because the Wayland backend makes it a real one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentGrant {
    /// Display text for consent and tracing. Never a matching key.
    #[serde(default)]
    pub label: String,
    /// Absolute path of the companion executable this grant belongs to.
    pub executable: PathBuf,
    /// User the companion must run as, when the grant should be narrower than
    /// the session's own user.
    #[serde(default)]
    pub uid: Option<u32>,
    /// Bundles and atoms this companion holds.
    pub capabilities: Vec<GrantedCapability>,
    /// Application scope. Out-of-scope clients are absent from responses and
    /// events, not merely inert.
    #[serde(default)]
    pub scope: Option<ApplicationMatcher>,
}

impl AgentGrant {
    /// Returns every atom this grant confers.
    #[must_use]
    pub fn capabilities(&self) -> CapabilitySet {
        self.capabilities
            .iter()
            .fold(CapabilitySet::EMPTY, |set, entry| set.union(entry.atoms()))
    }

    /// Returns whether this grant applies to a peer with `executable` and
    /// `uid`. A grant with no configured user applies to any user that can
    /// reach the socket, which the socket's own permissions already restrict
    /// to the session owner.
    #[must_use]
    pub fn matches_peer(&self, executable: Option<&Path>, uid: u32) -> bool {
        if self.uid.is_some_and(|expected| expected != uid) {
            return false;
        }
        executable.is_some_and(|path| path == self.executable)
    }
}

/// Launch policy for agent sessions.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentLaunchConfig {
    /// Which entries may be started.
    pub policy: LaunchPolicy,
    /// Entries allowed under [`LaunchPolicy::AllowListed`].
    pub allow: Vec<String>,
    /// Entries refused under [`LaunchPolicy::AllowInstalled`].
    pub deny: Vec<String>,
    /// Whether user-installed entries are launchable at all. A desktop entry
    /// runs code, and a user-writable entry is the easiest thing on the system
    /// for an agent to have arranged to exist.
    pub user_entries: bool,
}

impl AgentLaunchConfig {
    /// Returns whether `entry` may be launched.
    #[must_use]
    pub fn allows(&self, entry: &str, user_installed: bool) -> bool {
        if user_installed && !self.user_entries {
            return false;
        }
        match self.policy {
            LaunchPolicy::Deny => false,
            LaunchPolicy::AllowListed => self.allow.iter().any(|allowed| allowed == entry),
            LaunchPolicy::AllowInstalled => !self.deny.iter().any(|denied| denied == entry),
        }
    }
}

/// The `[agent]` section.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// Whether the manager listens for agent companions at all. No socket is
    /// created, and no seat is advertised, while this is false.
    pub enabled: bool,
    /// Socket path override. Empty means the manager's default location.
    pub socket: PathBuf,
    /// What to do with a companion that has no stored grant.
    pub policy: AgentPolicy,
    /// Stored grants, evaluated in order; the first match wins.
    pub grants: Vec<AgentGrant>,
    /// Launch policy.
    pub launch: AgentLaunchConfig,
    /// How long human input keeps agent input out, in milliseconds. The human
    /// wins structurally: politeness is not delegated to the agent.
    pub suppression_ms: u32,
    /// Chord that freezes every agent session at once. It is handled in the
    /// manager's own input path ahead of all agent traffic, so it works while
    /// an agent is flooding input.
    pub kill_chord: KeyChord,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            socket: PathBuf::new(),
            policy: AgentPolicy::Deny,
            grants: Vec::new(),
            launch: AgentLaunchConfig::default(),
            suppression_ms: 750,
            kill_chord: KeyChord::new([KeyboardModifier::Control, KeyboardModifier::Alt], "Escape"),
        }
    }
}

impl AgentConfig {
    /// Returns how long human input suppresses agent input.
    #[must_use]
    pub const fn suppression(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.suppression_ms as u64)
    }

    /// Returns the first grant matching a verified peer identity.
    #[must_use]
    pub fn grant_for(&self, executable: Option<&Path>, uid: u32) -> Option<&AgentGrant> {
        self.grants
            .iter()
            .find(|grant| grant.matches_peer(executable, uid))
    }

    /// Returns the capabilities a peer holds, which is nothing unless a stored
    /// grant names it.
    #[must_use]
    pub fn capabilities_for(&self, executable: Option<&Path>, uid: u32) -> CapabilitySet {
        self.grant_for(executable, uid)
            .map_or(CapabilitySet::EMPTY, AgentGrant::capabilities)
    }

    /// Checks bounds and consistency.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] describing the first problem found.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.socket.as_os_str().is_empty() {
            if !self.socket.is_absolute() {
                return Err(ConfigError::AgentSocketNotAbsolute(self.socket.clone()));
            }
            let length = self.socket.as_os_str().len();
            if length > MAX_AGENT_SOCKET_PATH {
                return Err(ConfigError::AgentSocketTooLong {
                    length,
                    limit: MAX_AGENT_SOCKET_PATH,
                });
            }
        }
        if self.suppression_ms > MAX_SUPPRESSION_MS {
            return Err(ConfigError::InvalidSuppressionWindow(self.suppression_ms));
        }
        if self.grants.len() > MAX_AGENT_GRANTS {
            return Err(ConfigError::TooManyAgentGrants(self.grants.len()));
        }
        for (index, grant) in self.grants.iter().enumerate() {
            let position = index + 1;
            if grant.executable.as_os_str().is_empty() || !grant.executable.is_absolute() {
                return Err(ConfigError::AgentGrantExecutable(position));
            }
            if grant.capabilities.is_empty() {
                return Err(ConfigError::AgentGrantWithoutCapabilities(position));
            }
            if let Some(scope) = &grant.scope {
                if scope.is_empty() {
                    return Err(ConfigError::EmptyAgentGrantScope(position));
                }
                for pattern in [
                    scope.name.as_deref(),
                    scope.class.as_deref(),
                    scope.group_name.as_deref(),
                    scope.group_class.as_deref(),
                    scope.role.as_deref(),
                    scope.title.as_deref(),
                ]
                .into_iter()
                .flatten()
                {
                    if pattern.is_empty() {
                        return Err(ConfigError::EmptyAgentGrantScope(position));
                    }
                }
            }
        }
        for list in [&self.launch.allow, &self.launch.deny] {
            if list.len() > MAX_LAUNCH_ENTRIES {
                return Err(ConfigError::TooManyLaunchEntries(list.len()));
            }
            for entry in list {
                if entry.is_empty()
                    || entry.len() > MAX_DESKTOP_ENTRY_LEN
                    || entry.contains('/')
                    || entry.contains('\0')
                {
                    return Err(ConfigError::InvalidLaunchEntry(entry.clone()));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use agent_seat_proto::{Bundle, Capability, CapabilitySet};

    use super::{
        AgentConfig, AgentGrant, AgentPolicy, AgentVisibility, GrantedCapability, LaunchPolicy,
    };
    use crate::{Config, ConfigError};

    fn parse(source: &str) -> Result<Config, ConfigError> {
        Config::parse(source)
    }

    #[test]
    fn the_agent_seat_is_absent_unless_configured() {
        let config = Config::default();
        assert!(!config.agent.enabled);
        assert_eq!(config.agent.policy, AgentPolicy::Deny);
        assert!(config.agent.grants.is_empty());
        assert_eq!(config.agent.launch.policy, LaunchPolicy::Deny);
        assert!(!config.agent.launch.user_entries);
        assert!(
            config
                .agent
                .capabilities_for(Some(Path::new("/usr/bin/anything")), 1000)
                .is_empty()
        );
    }

    #[test]
    fn the_human_wins_by_default_and_the_kill_chord_exists() {
        let config = AgentConfig::default();
        assert_eq!(config.suppression_ms, 750);
        assert_eq!(config.suppression(), std::time::Duration::from_millis(750));
        assert_eq!(config.kill_chord.symbol(), "Escape");
        assert_eq!(config.kill_chord.modifiers().len(), 2);
    }

    #[test]
    fn an_unbounded_suppression_window_is_refused() {
        let error = parse("[agent]\nsuppression_ms = 120000\n").expect_err("invalid");
        assert!(
            matches!(error, ConfigError::InvalidSuppressionWindow(120_000)),
            "{error}"
        );
        let config =
            parse("[agent]\nsuppression_ms = 0\nkill_chord = \"W-Escape\"\n").expect("valid");
        assert_eq!(config.agent.suppression_ms, 0);
        assert_eq!(config.agent.kill_chord.symbol(), "Escape");
    }

    #[test]
    fn a_grant_expands_bundles_and_atoms_together() {
        let config = parse(
            r#"
            [agent]
            enabled = true

            [[agent.grants]]
            label = "example harness"
            executable = "/usr/bin/nobox-agent"
            capabilities = ["observe", "manage.activate"]
            "#,
        )
        .expect("parses");
        config.validate().expect("valid");
        let held = config
            .agent
            .capabilities_for(Some(Path::new("/usr/bin/nobox-agent")), 1000);
        assert!(held.holds(Capability::ObserveStructure));
        assert!(held.holds(Capability::ObserveTitles));
        assert!(!held.holds(Capability::ObserveAccessibility));
        assert!(held.holds(Capability::ManageActivate));
        assert!(!held.holds(Capability::ManageGeometry));
        assert!(!held.holds(Capability::InputPointer));
    }

    #[test]
    fn accessibility_is_an_explicit_separate_grant() {
        let config = parse(
            r#"
            [[agent.grants]]
            executable = "/usr/bin/nobox-agent"
            capabilities = ["accessibility"]
            "#,
        )
        .expect("parses");
        let held = config
            .agent
            .capabilities_for(Some(Path::new("/usr/bin/nobox-agent")), 1000);
        assert!(held.holds(Capability::ObserveAccessibility));
        assert!(!held.holds(Capability::ObserveStructure));
        assert!(!held.holds(Capability::ObserveTitles));
    }

    #[test]
    fn a_grant_binds_to_the_executable_not_to_a_declared_name() {
        let config = parse(
            r#"
            [agent]
            enabled = true

            [[agent.grants]]
            label = "trusted"
            executable = "/usr/bin/nobox-agent"
            uid = 1000
            capabilities = ["observe"]
            "#,
        )
        .expect("parses");
        let grant = &config.agent.grants[0];
        assert!(grant.matches_peer(Some(Path::new("/usr/bin/nobox-agent")), 1000));
        assert!(!grant.matches_peer(Some(Path::new("/tmp/impostor")), 1000));
        assert!(!grant.matches_peer(Some(Path::new("/usr/bin/nobox-agent")), 1001));
        assert!(!grant.matches_peer(None, 1000));
    }

    #[test]
    fn a_grant_built_the_way_consent_builds_one_is_matched_next_time() {
        // "Allow and remember" appends a grant to the running configuration in
        // exactly this shape. If it were not matched by grant_for, remembering
        // would be indistinguishable from allowing once and the person would
        // be asked again on the very next connection.
        let mut config = parse("[agent]\nenabled = true\npolicy = \"ask\"\n").expect("parses");
        assert!(
            config
                .agent
                .grant_for(Some(Path::new("/usr/bin/nobox-agent")), 1000)
                .is_none()
        );

        let atoms = CapabilitySet::from_iter_atoms(Bundle::Observe.atoms().iter().copied());
        config.agent.grants.push(AgentGrant {
            label: "nobox-agent".to_owned(),
            executable: PathBuf::from("/usr/bin/nobox-agent"),
            uid: Some(1000),
            capabilities: atoms
                .atoms()
                .into_iter()
                .map(GrantedCapability::Atom)
                .collect(),
            scope: None,
        });

        let stored = config
            .agent
            .grant_for(Some(Path::new("/usr/bin/nobox-agent")), 1000)
            .expect("the remembered grant is found");
        assert_eq!(stored.capabilities(), atoms);
        // Still bound to the executable, not to anything the peer declares.
        assert!(
            config
                .agent
                .grant_for(Some(Path::new("/tmp/impostor")), 1000)
                .is_none()
        );
    }

    #[test]
    fn a_declared_label_is_not_a_matching_key() {
        let error = parse(
            r#"
            [[agent.grants]]
            harness = "trusted"
            capabilities = ["observe"]
            "#,
        )
        .expect_err("rejected");
        assert!(matches!(error, ConfigError::Toml(_)), "{error}");
    }

    #[test]
    fn unknown_capability_names_are_rejected() {
        let error = parse(
            r#"
            [[agent.grants]]
            executable = "/usr/bin/nobox-agent"
            capabilities = ["observe.everything"]
            "#,
        )
        .expect_err("rejected");
        assert!(matches!(error, ConfigError::Toml(_)), "{error}");
    }

    #[test]
    fn unknown_agent_keys_are_rejected() {
        let error = parse(
            r#"
            [agent]
            enabled = true
            allow_everything = true
            "#,
        )
        .expect_err("rejected");
        assert!(matches!(error, ConfigError::Toml(_)), "{error}");
    }

    #[test]
    fn grants_must_name_an_absolute_executable_and_a_capability() {
        let relative = parse(
            r#"
            [[agent.grants]]
            executable = "nobox-agent"
            capabilities = ["observe"]
            "#,
        )
        .expect_err("invalid");
        assert!(matches!(relative, ConfigError::AgentGrantExecutable(1)));

        let empty = parse(
            r#"
            [[agent.grants]]
            executable = "/usr/bin/nobox-agent"
            capabilities = []
            "#,
        )
        .expect_err("invalid");
        assert!(matches!(
            empty,
            ConfigError::AgentGrantWithoutCapabilities(1)
        ));
    }

    #[test]
    fn socket_overrides_are_bounded_by_the_platform() {
        let mut config = AgentConfig {
            socket: PathBuf::from("relative.sock"),
            ..AgentConfig::default()
        };
        assert!(matches!(
            config.validate().expect_err("invalid"),
            ConfigError::AgentSocketNotAbsolute(_)
        ));
        config.socket = PathBuf::from(format!("/tmp/{}", "s".repeat(120)));
        assert!(matches!(
            config.validate().expect_err("invalid"),
            ConfigError::AgentSocketTooLong { .. }
        ));
        config.socket = PathBuf::from("/run/user/1000/nobox/agent-seat-0.sock");
        config.validate().expect("valid");
    }

    #[test]
    fn an_empty_scope_matcher_is_refused() {
        let config = parse(
            r#"
            [[agent.grants]]
            executable = "/usr/bin/nobox-agent"
            capabilities = ["observe"]
            scope = {}
            "#,
        )
        .expect_err("invalid");
        assert!(matches!(config, ConfigError::EmptyAgentGrantScope(1)));
    }

    #[test]
    fn launch_policy_starts_closed_and_opens_only_as_written() {
        let config = parse(
            r#"
            [agent.launch]
            policy = "allow_listed"
            allow = ["org.example.Editor.desktop"]
            "#,
        )
        .expect("parses");
        config.validate().expect("valid");
        let launch = &config.agent.launch;
        assert!(launch.allows("org.example.Editor.desktop", false));
        assert!(!launch.allows("org.example.Shell.desktop", false));
        assert!(
            !launch.allows("org.example.Editor.desktop", true),
            "user-installed entries stay closed until enabled"
        );
    }

    #[test]
    fn installed_launch_policy_honors_the_deny_list() {
        let config = parse(
            r#"
            [agent.launch]
            policy = "allow_installed"
            deny = ["org.example.Wallet.desktop"]
            user_entries = true
            "#,
        )
        .expect("parses");
        config.validate().expect("valid");
        let launch = &config.agent.launch;
        assert!(launch.allows("org.example.Editor.desktop", false));
        assert!(launch.allows("org.example.Editor.desktop", true));
        assert!(!launch.allows("org.example.Wallet.desktop", false));
    }

    #[test]
    fn launch_entries_may_not_be_paths() {
        let config = parse(
            r#"
            [agent.launch]
            policy = "allow_listed"
            allow = ["../../etc/evil.desktop"]
            "#,
        )
        .expect_err("invalid");
        assert!(matches!(config, ConfigError::InvalidLaunchEntry(_)));
    }

    #[test]
    fn application_rules_carry_agent_visibility() {
        let config = parse(
            r#"
            [[applications]]
            match = { class = "Keepassxc" }
            agent_visibility = "hidden"

            [[applications]]
            match = { class = "Signal" }
            agent_visibility = "redacted"
            "#,
        )
        .expect("parses");
        config.validate().expect("valid");
        assert_eq!(
            config.applications[0].settings.agent_visibility,
            Some(AgentVisibility::Hidden)
        );
        assert_eq!(
            config.applications[1].settings.agent_visibility,
            Some(AgentVisibility::Redacted)
        );
        assert_eq!(
            config.applications[0]
                .settings
                .agent_visibility
                .unwrap_or_default(),
            AgentVisibility::Hidden
        );
        assert_eq!(AgentVisibility::default(), AgentVisibility::Visible);
    }
}
