//! Bounded persistence for X11 window-session state.

use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const SESSION_VERSION: u32 = 1;
const MAX_SESSION_CLIENTS: usize = 256;
const MAX_SESSION_FILE_BYTES: u64 = 1024 * 1024;
const MAX_IDENTITY_TEXT: usize = 1024;
const MAX_COMMAND_ARGUMENTS: usize = 64;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
/// Versioned persistent state captured when the X11 manager exits cleanly.
pub struct SessionSnapshot {
    version: u32,
    current_workspace: u32,
    clients: Vec<SessionClient>,
}

impl SessionSnapshot {
    pub(crate) fn new(current_workspace: u32, clients: Vec<SessionClient>) -> Self {
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
        if u64::try_from(source.len()).unwrap_or(u64::MAX) > MAX_SESSION_FILE_BYTES {
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

    /// Converts a validated snapshot into single-use restore candidates.
    #[must_use]
    pub fn into_restore(self) -> SessionRestore {
        if self.version != SESSION_VERSION {
            return SessionRestore::default();
        }
        let current_workspace = Some(self.current_workspace);
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
            current_workspace,
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
        for client in &self.clients {
            client.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
/// Single-use, duplicate-safe candidates used while X11 clients are managed.
pub struct SessionRestore {
    current_workspace: Option<u32>,
    clients: Vec<Option<SessionClient>>,
}

impl SessionRestore {
    pub(crate) const fn current_workspace(&self) -> Option<u32> {
        self.current_workspace
    }

    pub(crate) fn take_match(&mut self, identity: &SessionIdentity) -> Option<SessionClient> {
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionIdentity {
    pub(crate) session_id: Option<String>,
    pub(crate) command: Vec<String>,
    pub(crate) instance: String,
    pub(crate) class: String,
    pub(crate) role: String,
    pub(crate) kind: String,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionLayer {
    Below,
    Normal,
    Above,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionClient {
    pub(crate) identity: SessionIdentity,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) workspace: Option<u32>,
    pub(crate) iconic: bool,
    pub(crate) shaded: bool,
    pub(crate) skip_taskbar: bool,
    pub(crate) skip_pager: bool,
    pub(crate) fullscreen: bool,
    pub(crate) maximized_horizontal: bool,
    pub(crate) maximized_vertical: bool,
    pub(crate) layer: SessionLayer,
    pub(crate) focused: bool,
    pub(crate) stacking_index: u32,
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
        /// Attempted path.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// Session state was not valid TOML.
    #[error("invalid session state at {path}")]
    Parse {
        /// Attempted path.
        path: PathBuf,
        /// TOML parsing failure.
        #[source]
        source: toml::de::Error,
    },
    /// Session state could not be encoded.
    #[error("could not encode session state")]
    Serialize(#[source] toml::ser::Error),
    /// Session state could not be written.
    #[error("could not write session state at {path}")]
    Write {
        /// Attempted path.
        path: PathBuf,
        /// Underlying I/O failure.
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
    /// The serialized state exceeds its resource bound.
    #[error("session state exceeds the {MAX_SESSION_FILE_BYTES}-byte limit")]
    FileLimit,
    /// A client has no stable identity.
    #[error("session client has neither SM_CLIENT_ID nor WM_COMMAND")]
    MissingIdentity,
    /// A legacy command has too many arguments.
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
            focused: false,
            stacking_index: 0,
        }
    }

    #[test]
    fn restore_candidates_are_consumed_once() {
        let mut restore = SessionSnapshot::new(2, vec![client("one", 10)]).into_restore();
        assert_eq!(restore.current_workspace(), Some(2));
        assert_eq!(restore.take_match(&identity("one")).unwrap().x, 10);
        assert!(restore.take_match(&identity("one")).is_none());
    }

    #[test]
    fn duplicate_identity_candidates_are_all_discarded() {
        let mut restore =
            SessionSnapshot::new(0, vec![client("duplicate", 10), client("duplicate", 20)])
                .into_restore();
        assert!(restore.take_match(&identity("duplicate")).is_none());
    }

    #[test]
    fn strict_snapshot_round_trips() {
        let snapshot = SessionSnapshot::new(1, vec![client("round-trip", -20)]);
        let encoded = toml::to_string(&snapshot).unwrap();
        let decoded: SessionSnapshot = toml::from_str(&encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.current_workspace, 1);
        assert_eq!(decoded.clients[0].x, -20);
    }

    #[test]
    fn version_zero_cannot_contain_restore_data() {
        let snapshot = SessionSnapshot {
            version: 0,
            current_workspace: 2,
            clients: vec![client("unexpected", 10)],
        };
        assert!(matches!(snapshot.validate(), Err(SessionError::Version(0))));
        assert!(snapshot.into_restore().current_workspace().is_none());
    }
}
