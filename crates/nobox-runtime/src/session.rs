//! Bounded, protocol-neutral session persistence and run disposition.

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const SESSION_VERSION: u32 = 1;
const MAX_SESSION_CLIENTS: usize = 256;
const MAX_SESSION_FILE_BYTES: u64 = 1024 * 1024;
const MAX_IDENTITY_TEXT: usize = 1024;
const MAX_COMMAND_ARGUMENTS: usize = 64;

/// Requested process action after a backend event loop stops.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunDisposition {
    /// Exit the Nobox process.
    Exit,
    /// Restart Nobox, optionally by replacing it with a shell command.
    Restart {
        /// Replacement command; `None` restarts in-process.
        command: Option<String>,
    },
}

/// Versioned persistent state captured when a backend exits cleanly.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshot {
    version: u32,
    current_workspace: u32,
    clients: Vec<SessionClient>,
}

impl SessionSnapshot {
    /// Creates a current-version snapshot from neutral client state.
    #[must_use]
    pub fn new(current_workspace: u32, clients: Vec<SessionClient>) -> Self {
        Self {
            version: SESSION_VERSION,
            current_workspace,
            clients,
        }
    }

    /// Loads a saved session, treating a missing file as an empty snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, malformed, unsupported, or unbounded data.
    pub fn load(path: &Path) -> Result<Self, SessionError> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(SessionError::Read {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        let mut source = String::new();
        file.take(MAX_SESSION_FILE_BYTES + 1)
            .read_to_string(&mut source)
            .map_err(|source| SessionError::Read {
                path: path.to_owned(),
                source,
            })?;
        if source.len() as u64 > MAX_SESSION_FILE_BYTES {
            return Err(SessionError::FileLimit);
        }
        let snapshot: Self = toml::from_str(&source).map_err(|source| SessionError::Parse {
            path: path.to_owned(),
            source,
        })?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Atomically saves this snapshot with user-only permissions.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot is invalid or cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), SessionError> {
        self.validate()?;
        let Some(parent) = path.parent() else {
            return Err(SessionError::NoParent(path.to_owned()));
        };
        fs::create_dir_all(parent).map_err(|source| SessionError::Write {
            path: parent.to_owned(),
            source,
        })?;
        let encoded = toml::to_string_pretty(self).map_err(SessionError::Serialize)?;
        let temporary = temporary_path(path);
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(encoded.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temporary);
            return Err(SessionError::Write {
                path: path.to_owned(),
                source,
            });
        }
        Ok(())
    }

    /// Converts a validated snapshot into duplicate-safe, single-use candidates.
    #[must_use]
    pub fn into_restore(self) -> SessionRestore {
        if self.version != SESSION_VERSION {
            return SessionRestore::default();
        }
        let mut clients: Vec<Option<SessionClient>> = self.clients.into_iter().map(Some).collect();
        let mut duplicates = vec![false; clients.len()];
        for left in 0..clients.len() {
            for right in (left + 1)..clients.len() {
                if clients[left]
                    .as_ref()
                    .zip(clients[right].as_ref())
                    .is_some_and(|(left, right)| left.identity.matches(&right.identity))
                {
                    duplicates[left] = true;
                    duplicates[right] = true;
                }
            }
        }
        for (client, duplicate) in clients.iter_mut().zip(duplicates) {
            if duplicate {
                *client = None;
            }
        }
        SessionRestore {
            current_workspace: Some(self.current_workspace),
            clients,
        }
    }

    fn validate(&self) -> Result<(), SessionError> {
        if self.version == 0 && self.clients.is_empty() {
            return Ok(());
        }
        if self.version != SESSION_VERSION {
            return Err(SessionError::Version(self.version));
        }
        if self.clients.len() > MAX_SESSION_CLIENTS {
            return Err(SessionError::ClientLimit(self.clients.len()));
        }
        self.clients.iter().try_for_each(SessionClient::validate)
    }
}

/// Single-use, duplicate-safe candidates used while clients are managed.
#[derive(Debug, Default)]
pub struct SessionRestore {
    current_workspace: Option<u32>,
    clients: Vec<Option<SessionClient>>,
}

impl SessionRestore {
    /// Workspace restored when the backend claims its outputs.
    #[must_use]
    pub const fn current_workspace(&self) -> Option<u32> {
        self.current_workspace
    }

    /// Takes one exact neutral identity match at most once.
    pub fn take_match(&mut self, identity: &SessionIdentity) -> Option<SessionClient> {
        self.clients
            .iter_mut()
            .find(|candidate| {
                candidate
                    .as_ref()
                    .is_some_and(|candidate| candidate.identity.matches(identity))
            })
            .and_then(Option::take)
    }
}

/// Stable, protocol-neutral client identity used for session matching.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionIdentity {
    /// Session-manager identity, when one exists.
    pub session_id: Option<String>,
    /// Restart command identity, when one exists.
    pub command: Vec<String>,
    /// Application instance identity.
    pub instance: String,
    /// Application class identity.
    pub class: String,
    /// Application role identity.
    pub role: String,
    /// Application kind identity.
    pub kind: String,
}

impl SessionIdentity {
    fn matches(&self, other: &Self) -> bool {
        let stable = self
            .session_id
            .as_ref()
            .zip(other.session_id.as_ref())
            .is_some_and(|(left, right)| left == right)
            || !self.command.is_empty()
                && !other.command.is_empty()
                && self.command == other.command;
        stable
            && self.instance == other.instance
            && self.class == other.class
            && self.role == other.role
            && self.kind == other.kind
    }

    fn validate(&self) -> Result<(), SessionError> {
        if self.session_id.is_none() && self.command.is_empty() {
            return Err(SessionError::MissingIdentity);
        }
        if self.command.len() > MAX_COMMAND_ARGUMENTS {
            return Err(SessionError::CommandLimit(self.command.len()));
        }
        for value in self.session_id.iter().chain(self.command.iter()).chain([
            &self.instance,
            &self.class,
            &self.role,
            &self.kind,
        ]) {
            if value.len() > MAX_IDENTITY_TEXT || value.contains('\0') {
                return Err(SessionError::IdentityText);
            }
        }
        Ok(())
    }
}

/// Protocol-neutral stacking layer persisted for a client.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLayer {
    /// Below ordinary clients.
    Below,
    /// Ordinary client layer.
    Normal,
    /// Above ordinary clients.
    Above,
}

/// Persisted per-client decoration override.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionDecorationOverride {
    /// Follow ordinary policy.
    #[default]
    Default,
    /// Force server-side decorations.
    Decorated,
    /// Force decorations off.
    Undecorated,
}

/// Protocol-neutral state for one persisted client.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionClient {
    /// Stable matching identity.
    pub identity: SessionIdentity,
    /// Outer x coordinate.
    pub x: i32,
    /// Outer y coordinate.
    pub y: i32,
    /// Content width.
    pub width: u32,
    /// Content height.
    pub height: u32,
    /// Workspace, or all workspaces.
    pub workspace: Option<u32>,
    /// Whether the client is minimized.
    pub iconic: bool,
    /// Whether the client is shaded.
    pub shaded: bool,
    /// Whether task switchers omit the client.
    pub skip_taskbar: bool,
    /// Whether pagers omit the client.
    pub skip_pager: bool,
    /// Whether the client is fullscreen.
    pub fullscreen: bool,
    /// Horizontal maximization state.
    pub maximized_horizontal: bool,
    /// Vertical maximization state.
    pub maximized_vertical: bool,
    /// Stacking layer.
    pub layer: SessionLayer,
    /// Decoration override.
    #[serde(default)]
    pub decoration_override: SessionDecorationOverride,
    /// Whether the client held focus.
    pub focused: bool,
    /// Stable relative stacking position.
    pub stacking_index: u32,
}

impl SessionClient {
    fn validate(&self) -> Result<(), SessionError> {
        self.identity.validate()?;
        if self.width == 0 || self.height == 0 {
            return Err(SessionError::EmptyGeometry);
        }
        Ok(())
    }
}

/// Session persistence failure.
#[derive(Debug, Error)]
pub enum SessionError {
    /// Session state could not be read.
    #[error("could not read session state at {path}")]
    Read {
        /// Session path being read.
        path: PathBuf,
        /// Underlying read failure.
        #[source]
        source: std::io::Error,
    },
    /// Session state was not valid TOML.
    #[error("invalid session state at {path}")]
    Parse {
        /// Malformed session path.
        path: PathBuf,
        /// TOML decoding failure.
        #[source]
        source: toml::de::Error,
    },
    /// Session state could not be encoded.
    #[error("could not encode session state")]
    Serialize(#[source] toml::ser::Error),
    /// Session state could not be written.
    #[error("could not write session state at {path}")]
    Write {
        /// Session path being written.
        path: PathBuf,
        /// Underlying write failure.
        #[source]
        source: std::io::Error,
    },
    /// The output path has no parent directory.
    #[error("session path has no parent: {0}")]
    NoParent(PathBuf),
    /// The state schema version is unsupported.
    #[error("unsupported session state version {0}")]
    Version(u32),
    /// Too many clients were stored.
    #[error("session contains {0} clients; the limit is {MAX_SESSION_CLIENTS}")]
    ClientLimit(usize),
    /// Serialized state exceeds its resource bound.
    #[error("session state exceeds the {MAX_SESSION_FILE_BYTES}-byte limit")]
    FileLimit,
    /// A client has no stable identity.
    #[error("session client has neither session id nor restart command")]
    MissingIdentity,
    /// A restart command has too many arguments.
    #[error("session command has {0} arguments; the limit is {MAX_COMMAND_ARGUMENTS}")]
    CommandLimit(usize),
    /// Identity text is oversized or contains NUL.
    #[error("session identity text is invalid")]
    IdentityText,
    /// A saved client has empty geometry.
    #[error("session client geometry must be nonempty")]
    EmptyGeometry,
}

fn temporary_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session.toml");
    path.with_file_name(format!(".{name}.tmp-{}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(id: &str) -> SessionIdentity {
        SessionIdentity {
            session_id: Some(id.to_owned()),
            command: Vec::new(),
            instance: "editor".to_owned(),
            class: "Editor".to_owned(),
            role: "document".to_owned(),
            kind: "normal".to_owned(),
        }
    }

    fn client(id: &str, x: i32) -> SessionClient {
        SessionClient {
            identity: identity(id),
            x,
            y: 20,
            width: 640,
            height: 480,
            workspace: Some(0),
            iconic: false,
            shaded: false,
            skip_taskbar: false,
            skip_pager: false,
            fullscreen: false,
            maximized_horizontal: false,
            maximized_vertical: false,
            layer: SessionLayer::Normal,
            decoration_override: SessionDecorationOverride::Default,
            focused: false,
            stacking_index: 0,
        }
    }

    #[test]
    fn restore_candidates_are_consumed_once() {
        let mut restore = SessionSnapshot::new(2, vec![client("one", 10)]).into_restore();
        assert_eq!(restore.current_workspace(), Some(2));
        assert_eq!(
            restore.take_match(&identity("one")).map(|client| client.x),
            Some(10)
        );
        assert!(restore.take_match(&identity("one")).is_none());
    }

    #[test]
    fn duplicate_identity_candidates_are_discarded() {
        let mut restore =
            SessionSnapshot::new(0, vec![client("duplicate", 10), client("duplicate", 20)])
                .into_restore();
        assert!(restore.take_match(&identity("duplicate")).is_none());
    }
}
