//! Bounded UNIX-socket transport for the Nobox Agent Seat Protocol.
//!
//! Threads here only move bounded bytes. Every decision — what a session is
//! granted, what it may see, what it may do — happens on the manager's event
//! loop against manager policy, never in a companion and never on an I/O
//! thread. A dead, slow, hostile, or flooding companion loses its own session
//! and nothing else: the manager never blocks on this socket, never allocates
//! on a peer's say-so, and never lets a session failure reach window
//! management.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufReader, BufWriter, Read};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::{self, JoinHandle};

use nobox_agent_wire::{
    Advertisement, ClientMessage, DisconnectReason, FrameLimits, Goodbye, ProtocolError,
    ServerMessage, SessionId, read_frame, write_frame,
};
use tracing::{debug, info, warn};

/// Frames one session may have queued toward its companion before the manager
/// gives up on it.
const WRITER_QUEUE: usize = 64;

/// Frames every session together may have queued toward the event loop. A
/// flooding companion fills this and then waits on its own reader thread; the
/// manager is never the one that blocks.
const INBOX_QUEUE: usize = 256;

/// Parent hops recorded for a peer.
const MAX_PARENT_CHAIN: usize = 8;

/// Sessions accepted at once.
const MAX_SESSIONS: usize = 8;

/// What the manager could verify about the process behind a socket.
///
/// On X11 this is informative rather than a boundary: any process running as
/// the session user can bypass the manager entirely. It is collected and
/// enforced now because stored grants bind to it, and because the Wayland
/// backend makes the same check a real one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerIdentity {
    /// Peer user.
    pub uid: u32,
    /// Peer group.
    pub gid: u32,
    /// Peer process.
    pub pid: i32,
    /// Executable behind the peer process, when readable.
    pub executable: Option<PathBuf>,
    /// Bounded parent-process chain, nearest first.
    pub parents: Vec<i32>,
}

impl PeerIdentity {
    fn collect(stream: &UnixStream) -> Result<Self, std::io::Error> {
        let credentials = rustix::net::sockopt::socket_peercred(stream)?;
        let pid = credentials.pid.as_raw_nonzero().get();
        Ok(Self {
            uid: credentials.uid.as_raw(),
            gid: credentials.gid.as_raw(),
            pid,
            executable: executable_of(pid),
            parents: parent_chain(pid),
        })
    }
}

fn executable_of(pid: i32) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }
    fs::read_link(format!("/proc/{pid}/exe")).ok()
}

fn parent_chain(pid: i32) -> Vec<i32> {
    let mut chain = Vec::new();
    let mut current = pid;
    while chain.len() < MAX_PARENT_CHAIN && current > 1 {
        let Ok(stat) = fs::read_to_string(format!("/proc/{current}/stat")) else {
            break;
        };
        // The command field can contain spaces and parentheses, so the fields
        // after it are only unambiguous from the last closing parenthesis.
        let Some((_, tail)) = stat.rsplit_once(')') else {
            break;
        };
        let mut fields = tail.split_whitespace();
        let _state = fields.next();
        let Some(parent) = fields.next().and_then(|field| field.parse::<i32>().ok()) else {
            break;
        };
        if parent <= 1 {
            break;
        }
        chain.push(parent);
        current = parent;
    }
    chain
}

/// Reads a per-connection nonce from the system's entropy source.
pub fn nonce() -> String {
    let mut bytes = [0_u8; 16];
    match fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)) {
        Ok(()) => bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        Err(error) => {
            warn!(%error, "could not read a session nonce");
            String::new()
        }
    }
}

/// What an I/O thread hands to the event loop.
pub enum Inbound {
    /// A companion connected and its peer identity was collected.
    Connected {
        /// Session the manager assigned at accept time.
        session: SessionId,
        /// What could be verified about the process behind the socket.
        peer: Box<PeerIdentity>,
        /// Channel the event loop answers on.
        writer: SyncSender<ServerMessage>,
    },
    /// A well-formed frame arrived.
    Frame {
        /// Session it arrived on.
        session: SessionId,
        /// The decoded frame.
        message: Box<ClientMessage>,
    },
    /// The session's stream could not be framed and must end.
    Faulted {
        /// Session that faulted.
        session: SessionId,
        /// Why, for the goodbye and the log.
        error: ProtocolError,
    },
    /// The companion went away.
    Disconnected {
        /// Session that ended.
        session: SessionId,
    },
}

/// What the listener and a session's I/O threads share.
#[derive(Clone)]
struct SeatContext {
    inbox: SyncSender<Inbound>,
    wake_manager: Arc<dyn Fn() + Send + Sync>,
    wakeup_pending: Arc<AtomicBool>,
    limits: FrameLimits,
}

impl SeatContext {
    /// Wakes the event loop at most once per drain cycle, so a flooding
    /// companion cannot turn its own traffic into backend wakeups.
    fn wake(&self) {
        if self.wakeup_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        (self.wake_manager)();
    }

    /// Hands something to the event loop and wakes it, reporting whether the
    /// listener is still running.
    fn deliver(&self, inbound: Inbound) -> bool {
        if self.inbox.send(inbound).is_err() {
            return false;
        }
        self.wake();
        true
    }
}

/// One session's transport: who is behind it, and how to reach them.
///
/// Everything a session is *allowed* to do lives in `nobox-core`'s agent
/// state instead, so this module never decides anything.
struct SessionTransport {
    peer: Box<PeerIdentity>,
    writer: SyncSender<ServerMessage>,
    greeted: bool,
    /// Declared harness name, kept for display and tracing only.
    harness: String,
}

struct PreparedListener {
    listener: UnixListener,
    context: SeatContext,
}

/// The listener, its connected companions, and the socket they live on.
pub struct AgentSeat {
    socket_path: PathBuf,
    advertisement: Advertisement,
    inbox: Receiver<Inbound>,
    sessions: BTreeMap<SessionId, SessionTransport>,
    wakeup_pending: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    prepared: Option<PreparedListener>,
    acceptor: Option<JoinHandle<()>>,
}

impl AgentSeat {
    /// Binds the private socket without accepting sessions yet.
    ///
    /// The backend activates the listener only after publishing its own
    /// authenticated discovery mechanism.
    pub fn prepare(
        configured_socket: Option<&Path>,
        session_name: Option<&str>,
        wake_manager: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Self> {
        let socket_path = socket_path(configured_socket, session_name)?;
        let Some(advertisement_path) = socket_path.to_str().map(ToOwned::to_owned) else {
            warn!(path = ?socket_path, "agent seat socket path is not valid UTF-8");
            return None;
        };
        let listener = match bind(&socket_path) {
            Ok(listener) => listener,
            Err(error) => {
                warn!(path = %socket_path.display(), %error, "agent seat not started");
                return None;
            }
        };
        let (sender, inbox) = sync_channel(INBOX_QUEUE);
        let wakeup_pending = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let context = SeatContext {
            inbox: sender,
            wake_manager,
            wakeup_pending: Arc::clone(&wakeup_pending),
            limits: FrameLimits::DEFAULT,
        };
        Some(Self {
            advertisement: Advertisement::new(advertisement_path),
            socket_path,
            inbox,
            sessions: BTreeMap::new(),
            wakeup_pending,
            stopping,
            prepared: Some(PreparedListener { listener, context }),
            acceptor: None,
        })
    }

    /// Begins accepting sessions after backend discovery is publicly established.
    pub fn activate(&mut self) -> Result<(), std::io::Error> {
        if self.acceptor.is_some() {
            return Ok(());
        }
        let prepared = self
            .prepared
            .take()
            .ok_or_else(|| std::io::Error::other("agent seat listener is not prepared"))?;
        let stopping = Arc::clone(&self.stopping);
        let thread = thread::Builder::new()
            .name("nobox-agent-seat".to_owned())
            .stack_size(128 * 1024)
            .spawn(move || accept_loop(&prepared.listener, &prepared.context, &stopping))?;
        self.acceptor = Some(thread);
        info!(
            path = %self.socket_path.display(),
            protocol = nobox_agent_wire::PROTOCOL_NAME,
            version = nobox_agent_wire::PROTOCOL_VERSION,
            "agent seat listening"
        );
        Ok(())
    }

    /// Returns what the manager should publish on the root window.
    pub fn advertisement(&self) -> &Advertisement {
        &self.advertisement
    }

    /// Takes everything the I/O threads have queued.
    ///
    /// The wakeup flag is cleared first, so a frame that arrives during this
    /// call wakes the loop again rather than waiting for an unrelated event.
    pub fn take_inbound(&mut self) -> Vec<Inbound> {
        self.wakeup_pending.store(false, Ordering::SeqCst);
        let mut inbound = Vec::new();
        while let Ok(item) = self.inbox.try_recv() {
            inbound.push(item);
        }
        inbound
    }

    /// Accepts a newly connected companion, or refuses it when too many
    /// sessions are already open.
    pub fn accept(
        &mut self,
        session: SessionId,
        peer: Box<PeerIdentity>,
        writer: SyncSender<ServerMessage>,
    ) -> bool {
        if self.sessions.len() >= MAX_SESSIONS {
            warn!(session = %session, "refusing an agent session above the concurrent limit");
            let _ = writer.try_send(ServerMessage::Goodbye(Goodbye {
                reason: DisconnectReason::ProtocolViolation,
                message: "too many agent sessions".to_owned(),
            }));
            return false;
        }
        self.sessions.insert(
            session,
            SessionTransport {
                peer,
                writer,
                greeted: false,
                harness: String::new(),
            },
        );
        true
    }

    /// Returns the verified identity behind a session.
    pub fn peer(&self, session: SessionId) -> Option<&PeerIdentity> {
        self.sessions.get(&session).map(|state| &*state.peer)
    }

    /// Returns whether a session has completed its handshake.
    pub fn greeted(&self, session: SessionId) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|state| state.greeted)
    }

    /// Records that a session completed its handshake, keeping its declared
    /// harness name for display.
    pub fn mark_greeted(&mut self, session: SessionId, harness: String) {
        if let Some(state) = self.sessions.get_mut(&session) {
            state.greeted = true;
            state.harness = harness;
        }
    }

    /// Returns a session's declared harness name, which is display text and
    /// never an authorization input.
    pub fn harness(&self, session: SessionId) -> &str {
        self.sessions
            .get(&session)
            .map_or("", |state| state.harness.as_str())
    }

    /// Returns whether a session is still connected.
    pub fn holds(&self, session: SessionId) -> bool {
        self.sessions.contains_key(&session)
    }

    /// Drops a session's transport without sending anything.
    pub fn forget(&mut self, session: SessionId) -> bool {
        self.sessions.remove(&session).is_some()
    }

    /// Ends a session with a reason the companion can act on.
    pub fn close(&mut self, session: SessionId, reason: DisconnectReason, message: &str) {
        let Some(state) = self.sessions.remove(&session) else {
            return;
        };
        // Best effort: the writer thread drains what is queued, flushes, and
        // shuts the socket down once this sender drops.
        let _ = state.writer.try_send(ServerMessage::Goodbye(Goodbye {
            reason,
            message: message.to_owned(),
        }));
    }

    /// Queues a frame without ever blocking the event loop.
    ///
    /// Returns whether the session survived: a companion that has stopped
    /// reading is disconnected rather than allowed to apply backpressure to
    /// window management.
    pub fn send(&mut self, session: SessionId, message: ServerMessage) -> bool {
        let Some(state) = self.sessions.get(&session) else {
            return false;
        };
        match state.writer.try_send(message) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                warn!(session = %session, "disconnecting an agent session that stopped reading");
                self.close(
                    session,
                    DisconnectReason::SlowConsumer,
                    "the session stopped reading its socket",
                );
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                self.sessions.remove(&session);
                false
            }
        }
    }

    /// Offers a frame, reporting whether it was accepted.
    ///
    /// Unlike a response, an event that does not fit is not fatal: the manager
    /// keeps it queued and tries again, and only a backlog past its own bound
    /// costs the session a resync.
    pub fn offer(&mut self, session: SessionId, message: ServerMessage) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|state| state.writer.try_send(message).is_ok())
    }

    /// Ends every session and stops listening.
    pub fn stop(&mut self) {
        for session in self.sessions.keys().copied().collect::<Vec<_>>() {
            self.close(
                session,
                DisconnectReason::ManagerShutdown,
                "the window manager is shutting down",
            );
        }
        self.stopping.store(true, Ordering::SeqCst);
        self.prepared.take();
        if self.acceptor.is_some() {
            // Unblock the accept call so the listener thread observes the flag.
            let _ = UnixStream::connect(&self.socket_path);
        }
        if let Some(thread) = self.acceptor.take() {
            let _ = thread.join();
        }
        if let Err(error) = fs::remove_file(&self.socket_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %self.socket_path.display(), %error, "could not remove the agent socket");
            }
        }
    }
}

impl Drop for AgentSeat {
    fn drop(&mut self) {
        if self.prepared.is_some() || self.acceptor.is_some() {
            self.stop();
        }
    }
}

/// Chooses where the seat listens.
fn socket_path(configured_socket: Option<&Path>, session_name: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = configured_socket.filter(|path| !path.as_os_str().is_empty()) {
        return Some(path.to_path_buf());
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let runtime = PathBuf::from(runtime);
    if !runtime.is_absolute() {
        warn!("XDG_RUNTIME_DIR is not absolute; the agent seat has nowhere private to listen");
        return None;
    }
    let session_name = session_name.unwrap_or_default();
    Some(runtime.join("nobox").join(format!(
        "agent-seat-{}.sock",
        sanitize_session_name(session_name)
    )))
}

/// Reduces a backend session name to something safe inside a path.
fn sanitize_session_name(session_name: &str) -> String {
    let trimmed = session_name.trim_start_matches(':');
    let sanitized: String = trimmed
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "0".to_owned()
    } else {
        sanitized
    }
}

/// Creates the listening socket with a private directory and private socket.
fn bind(path: &Path) -> Result<UnixListener, std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "another manager is already listening on this socket",
            ));
        }
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn accept_loop(listener: &UnixListener, context: &SeatContext, stopping: &Arc<AtomicBool>) {
    let next_session = AtomicU64::new(1);
    for stream in listener.incoming() {
        if stopping.load(Ordering::SeqCst) {
            break;
        }
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                warn!(%error, "agent seat stopped accepting connections");
                break;
            }
        };
        let peer = match PeerIdentity::collect(&stream) {
            Ok(peer) => peer,
            Err(error) => {
                warn!(%error, "refusing an agent connection with unreadable credentials");
                continue;
            }
        };
        let session = SessionId::new(next_session.fetch_add(1, Ordering::Relaxed));
        let (writer_sender, writer_receiver) = sync_channel(WRITER_QUEUE);
        if !context.deliver(Inbound::Connected {
            session,
            peer: Box::new(peer),
            writer: writer_sender,
        }) {
            break;
        }
        let reader_stream = match stream.try_clone() {
            Ok(clone) => clone,
            Err(error) => {
                warn!(%error, "could not split an agent connection");
                context.deliver(Inbound::Disconnected { session });
                continue;
            }
        };
        spawn_session(session, stream, reader_stream, writer_receiver, context);
    }
    debug!("agent seat listener stopped");
}

fn spawn_session(
    session: SessionId,
    writer_stream: UnixStream,
    reader_stream: UnixStream,
    outgoing: Receiver<ServerMessage>,
    context: &SeatContext,
) {
    let limits = context.limits;
    let writer = thread::Builder::new()
        .name(format!("nobox-agent-w{session}"))
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut sink = BufWriter::new(&writer_stream);
            while let Ok(message) = outgoing.recv() {
                let goodbye = matches!(message, ServerMessage::Goodbye(_));
                if let Err(error) = write_frame(&mut sink, &message, &limits) {
                    debug!(session = %session, %error, "agent session write failed");
                    break;
                }
                if goodbye {
                    break;
                }
            }
            drop(sink);
            let _ = writer_stream.shutdown(Shutdown::Both);
        });
    if let Err(error) = writer {
        warn!(session = %session, %error, "could not start an agent session writer");
        context.deliver(Inbound::Disconnected { session });
        return;
    }
    let context = context.clone();
    let reader = thread::Builder::new()
        .name(format!("nobox-agent-r{session}"))
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut source = BufReader::new(&reader_stream);
            loop {
                match read_frame::<ClientMessage>(&mut source, &limits) {
                    Ok(message) => {
                        // A full inbox blocks this reader, which is exactly the
                        // backpressure a flooding companion should feel.
                        if !context.deliver(Inbound::Frame {
                            session,
                            message: Box::new(message),
                        }) {
                            break;
                        }
                    }
                    Err(error) => {
                        let inbound = if error.is_closed() {
                            Inbound::Disconnected { session }
                        } else {
                            Inbound::Faulted {
                                session,
                                error: error.as_protocol_error(),
                            }
                        };
                        context.deliver(inbound);
                        break;
                    }
                }
            }
            let _ = reader_stream.shutdown(Shutdown::Both);
        });
    if let Err(error) = reader {
        warn!(session = %session, %error, "could not start an agent session reader");
    }
}

#[cfg(test)]
mod tests {
    use super::{parent_chain, sanitize_session_name, socket_path};
    use std::path::{Path, PathBuf};

    #[test]
    fn session_names_become_safe_path_components() {
        assert_eq!(sanitize_session_name(":0"), "0");
        assert_eq!(sanitize_session_name(":1.0"), "1.0");
        assert_eq!(sanitize_session_name(""), "0");
        assert_eq!(
            sanitize_session_name("../../etc/passwd"),
            ".._.._etc_passwd"
        );
        assert_eq!(sanitize_session_name("host:0.0"), "host_0.0");
    }

    #[test]
    fn a_configured_socket_path_is_used_verbatim() {
        assert_eq!(
            socket_path(Some(Path::new("/run/user/1000/custom.sock")), Some(":0")),
            Some(PathBuf::from("/run/user/1000/custom.sock"))
        );
    }

    #[test]
    fn the_parent_chain_is_bounded_and_terminates() {
        let chain = parent_chain(std::process::id() as i32);
        assert!(chain.len() <= super::MAX_PARENT_CHAIN);
        assert!(chain.iter().all(|parent| *parent > 1));
        assert!(parent_chain(0).is_empty());
        assert!(parent_chain(-1).is_empty());
    }
}
