use std::{
    env,
    ffi::OsStr,
    fmt,
    fs::{self, DirBuilder, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt as _,
        fs::{DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle},
};

use rustix::{net::sockopt::socket_peercred, process};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const RECORD_VERSION: u32 = 1;
const MAX_INSTANCE_RECORD_BYTES: u64 = 4_096;
const MAX_INSTANCE_ID_BYTES: usize = 32;
const MAX_UNIX_PATH_BYTES: usize = 107;
const CONTROL_QUEUE_BOUND: usize = 32;

/// Display-server backend owning a runtime instance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// The X11 window-manager backend.
    X11,
    /// The native Wayland compositor backend.
    Wayland,
}

/// Small factual capability set shared by diagnostics, settings, and status surfaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendCapabilities {
    /// Backend described by this set.
    pub backend: BackendKind,
    /// Whether a nested-X11 development session can run.
    pub nested_x11: bool,
    /// Whether direct device/TTY sessions can run.
    pub direct_session: bool,
    /// Whether managed client state can be restored across restart.
    pub session_restore: bool,
    /// Whether the optional standards-based panel is usable.
    pub panel: bool,
    /// Whether the Agent Seat is implemented for this backend.
    pub agent_seat: bool,
}

impl BackendCapabilities {
    /// Capabilities of the hardened X11 baseline.
    pub const X11: Self = Self {
        backend: BackendKind::X11,
        nested_x11: false,
        direct_session: true,
        session_restore: true,
        panel: true,
        agent_seat: true,
    };

    /// Capabilities of the managed nested Wayland compositor.
    pub const WAYLAND_NESTED: Self = Self {
        backend: BackendKind::Wayland,
        nested_x11: true,
        direct_session: false,
        session_restore: true,
        panel: false,
        agent_seat: false,
    };
}

impl BackendKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::X11 => "x11",
            Self::Wayland => "wayland",
        }
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for BackendKind {
    type Err = ControlError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "x11" => Ok(Self::X11),
            "wayland" => Ok(Self::Wayland),
            _ => Err(ControlError::InvalidBackend(value.to_owned())),
        }
    }
}

/// One typed process-level request accepted by every backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlRequest {
    /// Reload the effective configuration in place.
    Reload,
    /// Stop the current process cleanly.
    Shutdown,
    /// Capture session state for an external session coordinator.
    SaveSession,
}

impl ControlRequest {
    const fn encode(self) -> u8 {
        match self {
            Self::Reload => b'R',
            Self::Shutdown => b'Q',
            Self::SaveSession => b'S',
        }
    }

    const fn decode(value: u8) -> Option<Self> {
        match value {
            b'R' => Some(Self::Reload),
            b'Q' => Some(Self::Shutdown),
            b'S' => Some(Self::SaveSession),
            _ => None,
        }
    }
}

/// Opaque, filesystem-safe identity for one live backend process.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct InstanceId(String);

impl InstanceId {
    /// Parses a bounded lowercase hexadecimal runtime identity.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidInstanceId`] for any other representation.
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        if value.len() != MAX_INSTANCE_ID_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(ControlError::InvalidInstanceId(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }

    fn generate() -> Result<Self, ControlError> {
        let mut random = [0_u8; MAX_INSTANCE_ID_BYTES / 2];
        fs::File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut random))
            .map_err(ControlError::RandomIdentity)?;
        let mut encoded = String::with_capacity(MAX_INSTANCE_ID_BYTES);
        for byte in random {
            use fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").map_err(|_| ControlError::IdentityEncoding)?;
        }
        Ok(Self(encoded))
    }

    /// Returns the stable textual representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validated metadata for a live Nobox backend instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunningInstance {
    backend: BackendKind,
    id: InstanceId,
    pid: u32,
    socket_path: PathBuf,
}

impl RunningInstance {
    /// Backend which owns the instance.
    #[must_use]
    pub const fn backend(&self) -> BackendKind {
        self.backend
    }

    /// Opaque instance identity.
    #[must_use]
    pub const fn id(&self) -> &InstanceId {
        &self.id
    }

    /// Process recorded by the owning backend.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// Creates a sender after revalidating the private runtime files.
    ///
    /// # Errors
    ///
    /// Returns an error if the record changed or no longer passes ownership,
    /// permission, process, and socket validation.
    pub fn sender(&self) -> Result<ControlSender, ControlError> {
        let current = RunningInstance::load(self.backend, &self.id)?;
        if current.pid != self.pid || current.socket_path != self.socket_path {
            return Err(ControlError::StaleInstance(self.id.clone()));
        }
        Ok(ControlSender {
            socket_path: self.socket_path.clone(),
        })
    }

    /// Loads one exact backend instance from its private record.
    ///
    /// # Errors
    ///
    /// Returns an error for missing, malformed, insecure, mismatched, or stale
    /// runtime state.
    pub fn load(backend: BackendKind, id: &InstanceId) -> Result<Self, ControlError> {
        let directory = runtime_directory(false)?;
        let record_path = record_path(&directory, backend, id);
        validate_owned_regular_file(&record_path, 0o077)?;
        let source = read_bounded(&record_path)?;
        let record: InstanceRecord =
            toml::from_str(&source).map_err(|source| ControlError::InvalidRecord {
                path: record_path.clone(),
                source,
            })?;
        record.validate(backend, id)?;
        let socket_path = directory.join(&record.socket);
        validate_socket_path(&socket_path)?;
        validate_owned_socket(&socket_path)?;
        let pid = i32::try_from(record.pid)
            .ok()
            .and_then(process::Pid::from_raw)
            .ok_or_else(|| ControlError::StaleInstance(id.clone()))?;
        process::test_kill_process(pid).map_err(|_| ControlError::StaleInstance(id.clone()))?;
        Ok(Self {
            backend,
            id: id.clone(),
            pid: record.pid,
            socket_path,
        })
    }

    /// Finds the only live instance for a backend, rejecting ambiguity.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime directory is insecure, no instance is
    /// live, or more than one live instance exists.
    pub fn discover_unique(backend: BackendKind) -> Result<Self, ControlError> {
        let directory = runtime_directory(false)?;
        let prefix = format!("{}-", backend.as_str());
        let mut running = Vec::new();
        for entry in fs::read_dir(&directory).map_err(|source| ControlError::RuntimeDirectory {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| ControlError::RuntimeDirectory {
                path: directory.clone(),
                source,
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(raw_id) = name
                .strip_prefix(&prefix)
                .and_then(|name| name.strip_suffix(".toml"))
            else {
                continue;
            };
            let Ok(id) = InstanceId::parse(raw_id) else {
                continue;
            };
            if let Ok(instance) = Self::load(backend, &id) {
                running.push(instance);
            }
        }
        match running.len() {
            0 => Err(ControlError::NoRunningInstance(backend)),
            1 => Ok(running.remove(0)),
            count => Err(ControlError::AmbiguousInstances { backend, count }),
        }
    }
}

/// Client for one private runtime control socket.
#[derive(Clone, Debug)]
pub struct ControlSender {
    socket_path: PathBuf,
}

impl ControlSender {
    /// Writes one bounded request to the selected private control socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket is insecure, stale, unavailable, or
    /// cannot accept the request.
    pub fn send(&self, request: ControlRequest) -> Result<(), ControlError> {
        validate_socket_path(&self.socket_path)?;
        validate_owned_socket(&self.socket_path)?;
        let mut stream =
            UnixStream::connect(&self.socket_path).map_err(|source| ControlError::Connect {
                path: self.socket_path.clone(),
                source,
            })?;
        stream
            .write_all(&[request.encode()])
            .map_err(ControlError::Send)?;
        Ok(())
    }

    /// Requests an in-place reload.
    ///
    /// # Errors
    ///
    /// Returns any error from [`Self::send`].
    pub fn reload(&self) -> Result<(), ControlError> {
        self.send(ControlRequest::Reload)
    }

    /// Requests clean shutdown.
    ///
    /// # Errors
    ///
    /// Returns any error from [`Self::send`].
    pub fn shutdown(&self) -> Result<(), ControlError> {
        self.send(ControlRequest::Shutdown)
    }

    /// Requests a coherent session snapshot.
    ///
    /// # Errors
    ///
    /// Returns any error from [`Self::send`].
    pub fn save_session(&self) -> Result<(), ControlError> {
        self.send(ControlRequest::SaveSession)
    }
}

/// Owning server for one backend's private process-control endpoint.
pub struct ControlServer {
    instance: RunningInstance,
    receiver: Receiver<ControlRequest>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    record_path: PathBuf,
}

impl ControlServer {
    /// Creates a private endpoint and starts a bounded same-UID accept loop.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime directory is insecure, the socket or
    /// atomic record cannot be created, or the accept thread cannot start.
    pub fn bind(
        backend: BackendKind,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Self, ControlError> {
        let directory = runtime_directory(true)?;
        let id = InstanceId::generate()?;
        let socket_path = socket_path(&directory, backend, &id);
        validate_socket_path(&socket_path)?;
        let listener = UnixListener::bind(&socket_path).map_err(|source| ControlError::Bind {
            path: socket_path.clone(),
            source,
        })?;
        fs::set_permissions(
            &socket_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|source| ControlError::Permissions {
            path: socket_path.clone(),
            source,
        })?;
        let record_path = record_path(&directory, backend, &id);
        let record = InstanceRecord {
            version: RECORD_VERSION,
            backend,
            id: id.clone(),
            pid: std::process::id(),
            socket: socket_path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or(ControlError::InvalidSocketName)?
                .to_owned(),
        };
        write_record(&record_path, &record)?;
        let instance = RunningInstance {
            backend,
            id,
            pid: record.pid,
            socket_path: socket_path.clone(),
        };
        let (sender, receiver) = mpsc::sync_channel(CONTROL_QUEUE_BOUND);
        let stopping = Arc::new(AtomicBool::new(false));
        let thread_stopping = Arc::clone(&stopping);
        let thread = thread::Builder::new()
            .name(format!("nobox-{backend}-control"))
            .spawn(move || accept_loop(listener, sender, &thread_stopping, wake))
            .map_err(ControlError::Thread)?;
        Ok(Self {
            instance,
            receiver,
            stopping,
            thread: Some(thread),
            record_path,
        })
    }

    /// Metadata suitable for publication by the owning backend.
    #[must_use]
    pub const fn instance(&self) -> &RunningInstance {
        &self.instance
    }

    /// Creates an in-process client for signal and session bridges.
    #[must_use]
    pub fn sender(&self) -> ControlSender {
        ControlSender {
            socket_path: self.instance.socket_path.clone(),
        }
    }

    /// Takes the next queued request without blocking the backend event loop.
    pub fn try_recv(&self) -> Result<ControlRequest, TryRecvError> {
        self.receiver.try_recv()
    }

    /// Drains all requests currently queued at an event boundary.
    pub fn drain(&self) -> impl Iterator<Item = ControlRequest> + '_ {
        self.receiver.try_iter()
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.instance.socket_path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        remove_owned_socket(&self.instance.socket_path);
        remove_owned_regular_file(&self.record_path);
        if let Some(directory) = self.record_path.parent() {
            let _ = fs::remove_dir(directory);
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstanceRecord {
    version: u32,
    backend: BackendKind,
    id: InstanceId,
    pid: u32,
    socket: String,
}

impl InstanceRecord {
    fn validate(&self, backend: BackendKind, id: &InstanceId) -> Result<(), ControlError> {
        if self.version != RECORD_VERSION || self.backend != backend || &self.id != id {
            return Err(ControlError::MismatchedRecord(id.clone()));
        }
        if self.socket != socket_file_name(backend, id) {
            return Err(ControlError::InvalidSocketName);
        }
        Ok(())
    }
}

fn accept_loop(
    listener: UnixListener,
    sender: SyncSender<ControlRequest>,
    stopping: &AtomicBool,
    wake: impl Fn(),
) {
    loop {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        if stopping.load(Ordering::Acquire) {
            break;
        }
        let Ok(credentials) = socket_peercred(&stream) else {
            continue;
        };
        if credentials.uid != process::getuid() {
            continue;
        }
        let mut request = [0_u8; 2];
        let Ok(1) = stream.read(&mut request) else {
            continue;
        };
        let Some(request) = ControlRequest::decode(request[0]) else {
            continue;
        };
        match sender.try_send(request) {
            Ok(()) => wake(),
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {}
        }
    }
}

fn runtime_directory(create: bool) -> Result<PathBuf, ControlError> {
    let base = env::var_os("XDG_RUNTIME_DIR").ok_or(ControlError::MissingRuntimeDirectory)?;
    let base = PathBuf::from(base);
    validate_owned_directory(&base, 0o077)?;
    let directory = base.join("nobox");
    if create && !directory.exists() {
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(ControlError::RuntimeDirectory {
                    path: directory,
                    source,
                });
            }
        }
    }
    validate_owned_directory(&directory, 0o077)?;
    Ok(directory)
}

fn validate_owned_directory(path: &Path, forbidden_mode: u32) -> Result<(), ControlError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| ControlError::RuntimeDirectory {
        path: path.to_owned(),
        source,
    })?;
    if !private_path_attributes(
        metadata.file_type().is_dir(),
        metadata.file_type().is_symlink(),
        metadata.uid(),
        metadata.mode(),
        process::getuid().as_raw(),
        forbidden_mode,
    ) {
        return Err(ControlError::InsecurePath(path.to_owned()));
    }
    Ok(())
}

fn validate_owned_regular_file(path: &Path, forbidden_mode: u32) -> Result<(), ControlError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| ControlError::RecordRead {
        path: path.to_owned(),
        source,
    })?;
    if !private_path_attributes(
        metadata.file_type().is_file(),
        metadata.file_type().is_symlink(),
        metadata.uid(),
        metadata.mode(),
        process::getuid().as_raw(),
        forbidden_mode,
    ) {
        return Err(ControlError::InsecurePath(path.to_owned()));
    }
    Ok(())
}

fn validate_owned_socket(path: &Path) -> Result<(), ControlError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| ControlError::Connect {
        path: path.to_owned(),
        source,
    })?;
    if !private_path_attributes(
        metadata.file_type().is_socket(),
        metadata.file_type().is_symlink(),
        metadata.uid(),
        metadata.mode(),
        process::getuid().as_raw(),
        0o077,
    ) {
        return Err(ControlError::InsecurePath(path.to_owned()));
    }
    Ok(())
}

const fn private_path_attributes(
    correct_type: bool,
    symlink: bool,
    owner: u32,
    mode: u32,
    expected_owner: u32,
    forbidden_mode: u32,
) -> bool {
    correct_type && !symlink && owner == expected_owner && mode & forbidden_mode == 0
}

fn validate_socket_path(path: &Path) -> Result<(), ControlError> {
    if path.as_os_str().as_bytes().len() > MAX_UNIX_PATH_BYTES {
        return Err(ControlError::SocketPathTooLong(path.to_owned()));
    }
    Ok(())
}

fn socket_file_name(backend: BackendKind, id: &InstanceId) -> String {
    format!("{}-{id}.sock", backend.as_str())
}

fn socket_path(directory: &Path, backend: BackendKind, id: &InstanceId) -> PathBuf {
    directory.join(socket_file_name(backend, id))
}

fn record_path(directory: &Path, backend: BackendKind, id: &InstanceId) -> PathBuf {
    directory.join(format!("{}-{id}.toml", backend.as_str()))
}

fn write_record(path: &Path, record: &InstanceRecord) -> Result<(), ControlError> {
    let encoded = toml::to_string(record).map_err(ControlError::RecordEncode)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
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
        return Err(ControlError::RecordWrite {
            path: path.to_owned(),
            source,
        });
    }
    Ok(())
}

fn read_bounded(path: &Path) -> Result<String, ControlError> {
    let file = fs::File::open(path).map_err(|source| ControlError::RecordRead {
        path: path.to_owned(),
        source,
    })?;
    let mut source = String::new();
    file.take(MAX_INSTANCE_RECORD_BYTES + 1)
        .read_to_string(&mut source)
        .map_err(|source| ControlError::RecordRead {
            path: path.to_owned(),
            source,
        })?;
    if source.len() as u64 > MAX_INSTANCE_RECORD_BYTES {
        return Err(ControlError::RecordTooLarge(path.to_owned()));
    }
    Ok(source)
}

fn remove_owned_socket(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_socket() && metadata.uid() == process::getuid().as_raw()
    }) {
        let _ = fs::remove_file(path);
    }
}

fn remove_owned_regular_file(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_file() && metadata.uid() == process::getuid().as_raw()
    }) {
        let _ = fs::remove_file(path);
    }
}

/// Runtime-control boundary failure.
#[derive(Debug, Error)]
pub enum ControlError {
    /// XDG_RUNTIME_DIR is not available.
    #[error("XDG_RUNTIME_DIR is unset")]
    MissingRuntimeDirectory,
    /// A runtime directory could not be inspected or created.
    #[error("could not use runtime directory {path}")]
    RuntimeDirectory {
        /// Directory being inspected or created.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// A path is a symlink, has the wrong owner or type, or is accessible by another user.
    #[error("runtime path is not private and owned by the current user: {0}")]
    InsecurePath(PathBuf),
    /// A Unix socket path cannot fit in sockaddr_un.
    #[error("runtime socket path is too long: {0}")]
    SocketPathTooLong(PathBuf),
    /// An opaque instance id is malformed.
    #[error("invalid runtime instance id `{0}`")]
    InvalidInstanceId(String),
    /// A backend name is unknown.
    #[error("unknown runtime backend `{0}`")]
    InvalidBackend(String),
    /// Secure identity entropy could not be read.
    #[error("could not generate a runtime instance identity")]
    RandomIdentity(#[source] std::io::Error),
    /// A hexadecimal identity could not be encoded.
    #[error("could not encode a runtime instance identity")]
    IdentityEncoding,
    /// The control socket could not be bound.
    #[error("could not bind runtime control socket {path}")]
    Bind {
        /// Socket path being bound.
        path: PathBuf,
        /// Underlying socket failure.
        #[source]
        source: std::io::Error,
    },
    /// Private socket permissions could not be applied.
    #[error("could not secure runtime path {path}")]
    Permissions {
        /// Runtime path being secured.
        path: PathBuf,
        /// Underlying permission failure.
        #[source]
        source: std::io::Error,
    },
    /// The accept thread could not start.
    #[error("could not start runtime control thread")]
    Thread(#[source] std::io::Error),
    /// A runtime record could not be serialized.
    #[error("could not encode runtime instance record")]
    RecordEncode(#[source] toml::ser::Error),
    /// A runtime record could not be written.
    #[error("could not write runtime instance record {path}")]
    RecordWrite {
        /// Record path being written.
        path: PathBuf,
        /// Underlying write failure.
        #[source]
        source: std::io::Error,
    },
    /// A runtime record could not be read.
    #[error("could not read runtime instance record {path}")]
    RecordRead {
        /// Record path being read.
        path: PathBuf,
        /// Underlying read failure.
        #[source]
        source: std::io::Error,
    },
    /// A runtime record is oversized.
    #[error("runtime instance record is too large: {0}")]
    RecordTooLarge(PathBuf),
    /// A runtime record is malformed.
    #[error("invalid runtime instance record {path}")]
    InvalidRecord {
        /// Malformed record path.
        path: PathBuf,
        /// TOML decoding failure.
        #[source]
        source: toml::de::Error,
    },
    /// A record does not identify its requested backend instance.
    #[error("runtime record does not match instance {0}")]
    MismatchedRecord(InstanceId),
    /// A record contains a socket name which could escape its directory.
    #[error("runtime record contains an invalid socket name")]
    InvalidSocketName,
    /// A recorded process or socket is no longer live.
    #[error("runtime instance {0} is stale")]
    StaleInstance(InstanceId),
    /// No live instance exists for the requested backend.
    #[error("no running {0} instance was found")]
    NoRunningInstance(BackendKind),
    /// More than one instance exists and no exact identity was supplied.
    #[error("found {count} running {backend} instances; select one explicitly")]
    AmbiguousInstances {
        /// Backend with multiple live instances.
        backend: BackendKind,
        /// Number of live instances found.
        count: usize,
    },
    /// The private socket could not be opened.
    #[error("could not connect to runtime control socket {path}")]
    Connect {
        /// Socket path being opened.
        path: PathBuf,
        /// Underlying connection failure.
        #[source]
        source: std::io::Error,
    },
    /// A request could not be sent.
    #[error("could not send runtime control request")]
    Send(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, sync::mpsc, time::Duration};

    const _: () = {
        assert!(BackendCapabilities::WAYLAND_NESTED.nested_x11);
        assert!(BackendCapabilities::WAYLAND_NESTED.session_restore);
        assert!(!BackendCapabilities::WAYLAND_NESTED.direct_session);
        assert!(!BackendCapabilities::WAYLAND_NESTED.panel);
        assert!(!BackendCapabilities::WAYLAND_NESTED.agent_seat);
    };

    #[test]
    fn typed_request_wakes_and_round_trips() {
        let (wake_sender, wake_receiver) = mpsc::channel();
        let server = ControlServer::bind(BackendKind::Wayland, move || {
            let _ = wake_sender.send(());
        })
        .expect("bind server");
        server.sender().reload().expect("send reload");
        wake_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("prompt wake");
        assert_eq!(server.try_recv(), Ok(ControlRequest::Reload));
        drop(server);
    }

    #[test]
    fn private_paths_reject_wrong_owner_symlinks_and_open_modes() {
        let owner = process::getuid().as_raw();
        assert!(!private_path_attributes(
            true,
            false,
            owner.wrapping_add(1),
            0o700,
            owner,
            0o077
        ));
        assert!(!private_path_attributes(
            true, true, owner, 0o700, owner, 0o077
        ));
        assert!(!private_path_attributes(
            true, false, owner, 0o755, owner, 0o077
        ));
        assert!(private_path_attributes(
            true, false, owner, 0o700, owner, 0o077
        ));
    }

    #[test]
    fn record_symlinks_are_rejected() {
        let directory = runtime_directory(true).expect("runtime directory");
        let id = InstanceId::generate().expect("instance id");
        let target = directory.join(format!("test-target-{id}"));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&target)
            .expect("target file");
        let record = record_path(&directory, BackendKind::Wayland, &id);
        symlink(&target, &record).expect("record symlink");
        assert!(matches!(
            RunningInstance::load(BackendKind::Wayland, &id),
            Err(ControlError::InsecurePath(path)) if path == record
        ));
        fs::remove_file(record).expect("remove symlink");
        fs::remove_file(target).expect("remove target");
    }

    #[test]
    fn stale_pid_records_are_rejected() {
        let directory = runtime_directory(true).expect("runtime directory");
        let id = InstanceId::generate().expect("instance id");
        let socket = socket_path(&directory, BackendKind::Wayland, &id);
        let listener = UnixListener::bind(&socket).expect("stale socket");
        fs::set_permissions(&socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .expect("socket mode");
        let record_path = record_path(&directory, BackendKind::Wayland, &id);
        let record = InstanceRecord {
            version: RECORD_VERSION,
            backend: BackendKind::Wayland,
            id: id.clone(),
            pid: u32::MAX,
            socket: socket_file_name(BackendKind::Wayland, &id),
        };
        write_record(&record_path, &record).expect("stale record");
        assert!(matches!(
            RunningInstance::load(BackendKind::Wayland, &id),
            Err(ControlError::StaleInstance(stale)) if stale == id
        ));
        drop(listener);
        fs::remove_file(socket).expect("remove socket");
        fs::remove_file(record_path).expect("remove record");
    }

    #[test]
    fn ambiguous_instances_require_an_exact_identity() {
        let first = ControlServer::bind(BackendKind::Wayland, || {}).expect("first server");
        let second = ControlServer::bind(BackendKind::Wayland, || {}).expect("second server");
        assert!(matches!(
            RunningInstance::discover_unique(BackendKind::Wayland),
            Err(ControlError::AmbiguousInstances { count, .. }) if count >= 2
        ));
        drop((first, second));
    }

    #[test]
    fn overlong_socket_paths_are_rejected_before_bind() {
        let path = PathBuf::from(format!("/{}", "x".repeat(MAX_UNIX_PATH_BYTES + 1)));
        assert!(matches!(
            validate_socket_path(&path),
            Err(ControlError::SocketPathTooLong(rejected)) if rejected == path
        ));
    }
}
