//! The agent seat: a bounded UNIX-socket listener speaking the Agent Seat
//! Protocol.
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

use agent_seat_proto::{
    Advertisement, CapabilitySet, ClientMessage, DisconnectReason, ErrorCode, FrameLimits, Goodbye,
    Hello, Outcome, ProtocolError, Request, Response, ServerMessage, SessionId, Welcome,
    read_frame, write_frame,
};
use nobox_config::AgentConfig;
use tracing::{debug, info, warn};

use crate::ControlSender;

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
pub(crate) struct PeerIdentity {
    /// Peer user.
    pub(crate) uid: u32,
    /// Peer group.
    pub(crate) gid: u32,
    /// Peer process.
    pub(crate) pid: i32,
    /// Executable behind the peer process, when readable.
    pub(crate) executable: Option<PathBuf>,
    /// Bounded parent-process chain, nearest first.
    pub(crate) parents: Vec<i32>,
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

    /// Returns the executable path used for grant binding.
    pub(crate) fn executable(&self) -> Option<&Path> {
        self.executable.as_deref()
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
fn nonce() -> String {
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
enum Inbound {
    /// A companion connected and its peer identity was collected.
    Connected {
        session: SessionId,
        peer: Box<PeerIdentity>,
        writer: SyncSender<ServerMessage>,
    },
    /// A well-formed frame arrived.
    Frame {
        session: SessionId,
        message: Box<ClientMessage>,
    },
    /// The session's stream could not be framed and must end.
    Faulted {
        session: SessionId,
        error: ProtocolError,
    },
    /// The companion went away.
    Disconnected { session: SessionId },
}

/// What the listener and a session's I/O threads share.
#[derive(Clone)]
struct SeatContext {
    inbox: SyncSender<Inbound>,
    control: Arc<ControlSender>,
    wakeup_pending: Arc<AtomicBool>,
    limits: FrameLimits,
}

impl SeatContext {
    /// Wakes the event loop at most once per drain cycle, so a flooding
    /// companion cannot turn its own traffic into X11 traffic.
    fn wake(&self) {
        if self.wakeup_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Err(error) = self.control.send_data(crate::CONTROL_AGENT_TRAFFIC, 0) {
            warn!(%error, "could not wake the event loop for agent traffic");
        }
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

/// One live session as the manager sees it.
struct Session {
    peer: Box<PeerIdentity>,
    writer: SyncSender<ServerMessage>,
    greeted: bool,
    harness: String,
    granted: CapabilitySet,
    scoped: bool,
}

/// The listener, its sessions, and the socket they live on.
pub(crate) struct AgentSeat {
    socket_path: PathBuf,
    advertisement: Advertisement,
    inbox: Receiver<Inbound>,
    sessions: BTreeMap<SessionId, Session>,
    wakeup_pending: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    listener: Option<JoinHandle<()>>,
}

impl AgentSeat {
    /// Starts listening, or returns `None` when no seat should exist.
    ///
    /// A seat that cannot be established is reported and skipped. Window
    /// management does not depend on it, so nothing here may fail the manager.
    pub(crate) fn start(
        config: &AgentConfig,
        display: Option<&str>,
        control: ControlSender,
    ) -> Option<Self> {
        if !config.enabled {
            return None;
        }
        let socket_path = socket_path(config, display)?;
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
            control: Arc::new(control),
            wakeup_pending: Arc::clone(&wakeup_pending),
            limits: FrameLimits::DEFAULT,
        };
        let thread = thread::Builder::new()
            .name("nobox-agent-seat".to_owned())
            .stack_size(128 * 1024)
            .spawn({
                let stopping = Arc::clone(&stopping);
                move || accept_loop(&listener, &context, &stopping)
            });
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                warn!(%error, "could not start the agent seat listener");
                let _ = fs::remove_file(&socket_path);
                return None;
            }
        };
        info!(
            path = %socket_path.display(),
            protocol = agent_seat_proto::PROTOCOL_NAME,
            version = agent_seat_proto::PROTOCOL_VERSION,
            "agent seat listening"
        );
        if config.policy == nobox_config::AgentPolicy::Ask {
            warn!(
                "agent policy \"ask\" has no consent dialog yet; companions without a stored \
                 grant are denied"
            );
        }
        Some(Self {
            advertisement: Advertisement::new(socket_path.to_string_lossy().into_owned()),
            socket_path,
            inbox,
            sessions: BTreeMap::new(),
            wakeup_pending,
            stopping,
            listener: Some(thread),
        })
    }

    /// Returns what the manager should publish on the root window.
    pub(crate) fn advertisement(&self) -> &Advertisement {
        &self.advertisement
    }

    /// Handles everything the I/O threads have queued.
    ///
    /// Called at an event-loop boundary, so a session observes the manager
    /// between coherent states and never during one.
    pub(crate) fn drain(&mut self, config: &AgentConfig) {
        // Cleared before draining: a frame that arrives during the drain sets
        // the flag again and wakes the loop once more, so no frame can wait
        // for an unrelated event.
        self.wakeup_pending.store(false, Ordering::SeqCst);
        while let Ok(inbound) = self.inbox.try_recv() {
            match inbound {
                Inbound::Connected {
                    session,
                    peer,
                    writer,
                } => self.connect(session, peer, writer),
                Inbound::Frame { session, message } => self.handle(config, session, *message),
                Inbound::Faulted { session, error } => {
                    if self.sessions.contains_key(&session) {
                        debug!(session = %session, %error, "agent session faulted");
                        self.close(session, DisconnectReason::ProtocolViolation, &error.message);
                    }
                }
                Inbound::Disconnected { session } => {
                    if self.sessions.remove(&session).is_some() {
                        info!(session = %session, "agent session disconnected");
                    }
                }
            }
        }
    }

    fn connect(
        &mut self,
        session: SessionId,
        peer: Box<PeerIdentity>,
        writer: SyncSender<ServerMessage>,
    ) {
        if self.sessions.len() >= MAX_SESSIONS {
            warn!(session = %session, "refusing an agent session above the concurrent limit");
            let _ = writer.try_send(ServerMessage::Goodbye(Goodbye {
                reason: DisconnectReason::ProtocolViolation,
                message: "too many agent sessions".to_owned(),
            }));
            return;
        }
        info!(
            session = %session,
            uid = peer.uid,
            pid = peer.pid,
            executable = ?peer.executable,
            "agent session connected"
        );
        self.sessions.insert(
            session,
            Session {
                peer,
                writer,
                greeted: false,
                harness: String::new(),
                granted: CapabilitySet::EMPTY,
                scoped: false,
            },
        );
    }

    fn handle(&mut self, config: &AgentConfig, session: SessionId, message: ClientMessage) {
        match message {
            ClientMessage::Hello(hello) => self.greet(config, session, hello),
            ClientMessage::Request(request) => self.request(session, request),
        }
    }

    fn greet(&mut self, config: &AgentConfig, session: SessionId, hello: Hello) {
        let Some(state) = self.sessions.get_mut(&session) else {
            return;
        };
        if state.greeted {
            self.reject(
                session,
                ProtocolError::new(ErrorCode::HandshakeOrder, "the session already greeted"),
            );
            return;
        }
        if let Err(error) = hello.validate() {
            self.reject(session, error);
            return;
        }
        let grant = config.grant_for(state.peer.executable(), state.peer.uid);
        let granted = grant.map_or(CapabilitySet::EMPTY, nobox_config::AgentGrant::capabilities);
        let scoped = grant.is_some_and(|grant| grant.scope.is_some());
        state.greeted = true;
        state.harness = hello.harness.clone();
        state.granted = granted;
        state.scoped = scoped;
        info!(
            session = %session,
            harness = %hello.harness,
            purpose = %hello.purpose,
            requested = ?hello.requested,
            granted = ?granted.atoms(),
            scoped,
            "agent session greeted"
        );
        let welcome = ServerMessage::Welcome(Welcome {
            protocol: agent_seat_proto::PROTOCOL_NAME.to_owned(),
            version: agent_seat_proto::PROTOCOL_VERSION,
            manager: format!("nobox {}", env!("CARGO_PKG_VERSION")),
            session,
            nonce: nonce(),
            granted,
            scoped,
            sequence: agent_seat_proto::Sequence::ZERO,
            // Nothing is performable yet: the tool surface arrives with the
            // milestones that implement it, and an agent must not be told
            // otherwise.
            features: Vec::new(),
        });
        self.send(session, welcome);
    }

    fn request(&mut self, session: SessionId, request: Request) {
        let Some(state) = self.sessions.get(&session) else {
            return;
        };
        if !state.greeted {
            self.reject(
                session,
                ProtocolError::new(ErrorCode::HandshakeOrder, "greet before making requests"),
            );
            return;
        }
        let required = request.call.required();
        let outcome = if let Err(error) = request.call.validate() {
            Outcome::Error { error }
        } else if required.intersection(state.granted) != required {
            debug!(
                session = %session,
                tool = request.call.tool(),
                "denying an agent request outside the session grant"
            );
            Outcome::Error {
                error: ProtocolError::denied("this session was not granted that capability"),
            }
        } else {
            // The grant allows it and the manager cannot yet perform it. That
            // is a different answer from a denial, and saying so honestly is
            // what lets a harness distinguish policy from capability.
            Outcome::Error {
                error: ProtocolError::new(
                    ErrorCode::Unsupported,
                    format!("{} is not implemented yet", request.call.tool()),
                ),
            }
        };
        self.send(
            session,
            ServerMessage::Response(Response {
                id: request.id,
                sequence: agent_seat_proto::Sequence::ZERO,
                outcome,
            }),
        );
    }

    /// Answers a fatal protocol failure and ends the session.
    fn reject(&mut self, session: SessionId, error: ProtocolError) {
        warn!(session = %session, %error, "ending an agent session");
        self.close(session, DisconnectReason::ProtocolViolation, &error.message);
    }

    fn close(&mut self, session: SessionId, reason: DisconnectReason, message: &str) {
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
    fn send(&mut self, session: SessionId, message: ServerMessage) {
        let Some(state) = self.sessions.get(&session) else {
            return;
        };
        match state.writer.try_send(message) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                warn!(session = %session, "disconnecting an agent session that stopped reading");
                self.close(
                    session,
                    DisconnectReason::SlowConsumer,
                    "the session stopped reading its socket",
                );
            }
            Err(TrySendError::Disconnected(_)) => {
                self.sessions.remove(&session);
            }
        }
    }

    /// Ends every session and stops listening.
    pub(crate) fn stop(&mut self) {
        for session in self.sessions.keys().copied().collect::<Vec<_>>() {
            self.close(
                session,
                DisconnectReason::ManagerShutdown,
                "the window manager is shutting down",
            );
        }
        self.stopping.store(true, Ordering::SeqCst);
        // Unblock the accept call so the listener thread observes the flag.
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(thread) = self.listener.take() {
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
        if self.listener.is_some() {
            self.stop();
        }
    }
}

/// Chooses where the seat listens.
fn socket_path(config: &AgentConfig, display: Option<&str>) -> Option<PathBuf> {
    if !config.socket.as_os_str().is_empty() {
        return Some(config.socket.clone());
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let runtime = PathBuf::from(runtime);
    if !runtime.is_absolute() {
        warn!("XDG_RUNTIME_DIR is not absolute; the agent seat has nowhere private to listen");
        return None;
    }
    let display = display
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("DISPLAY").ok())
        .unwrap_or_default();
    Some(
        runtime
            .join("nobox")
            .join(format!("agent-seat-{}.sock", sanitize_display(&display))),
    )
}

/// Reduces a display name to something safe inside a path.
fn sanitize_display(display: &str) -> String {
    let trimmed = display.trim_start_matches(':');
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
    use super::{parent_chain, sanitize_display, socket_path};
    use nobox_config::AgentConfig;
    use std::path::PathBuf;

    #[test]
    fn display_names_become_safe_path_components() {
        assert_eq!(sanitize_display(":0"), "0");
        assert_eq!(sanitize_display(":1.0"), "1.0");
        assert_eq!(sanitize_display(""), "0");
        assert_eq!(sanitize_display("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(sanitize_display("host:0.0"), "host_0.0");
    }

    #[test]
    fn a_configured_socket_path_is_used_verbatim() {
        let config = AgentConfig {
            enabled: true,
            socket: PathBuf::from("/run/user/1000/custom.sock"),
            ..AgentConfig::default()
        };
        assert_eq!(
            socket_path(&config, Some(":0")),
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
