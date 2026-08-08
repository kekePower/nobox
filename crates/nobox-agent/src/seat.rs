//! The companion's client side of the Agent Seat Protocol.
//!
//! This half enforces nothing. Every request it sends is validated again by
//! the window manager against the session's grant, so a compromised companion
//! gains exactly the grant the manager already issued and nothing more.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, BufWriter};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nobox_agent_wire::{
    Bundle, Call, ClientMessage, EventEnvelope, FrameLimits, Hello, Outcome, Request, RequestId,
    Sequence, ServerMessage, Welcome, read_frame, write_frame,
};
use x11rb::NONE;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

/// Events held for a poll that has not been made yet.
///
/// The manager already bounds its own queue and asks for a re-snapshot when it
/// overflows, so this only has to survive the gap between two polls.
const MAX_BUFFERED_EVENTS: usize = 4096;

/// Longest a poll may wait before answering with nothing.
pub const MAX_POLL_WAIT: Duration = Duration::from_secs(30);

const MAX_ADVERTISEMENT_BYTES: usize = 256;
const MAX_ADVERTISEMENT_LONGS: u32 = 65;
const MAX_SOCKET_PATH_BYTES: usize = 107;

/// A connected, greeted session.
pub struct Seat {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
    /// A third handle, used only to bound how long a poll waits.
    timeouts: UnixStream,
    limits: FrameLimits,
    welcome: Welcome,
    next_request: u64,
    events: VecDeque<EventEnvelope>,
}

impl Seat {
    /// Connects to a manager's socket and completes the handshake, asking for
    /// `requested`.
    ///
    /// Asking is not receiving: the manager answers with the grant it issued,
    /// which is what [`Seat::welcome`] reports. A hello that asks for nothing
    /// leaves a manager configured to consult a human with nothing to show,
    /// so it decides alone.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message when the socket is unreachable, the
    /// manager speaks a different protocol version, or the handshake is
    /// refused.
    pub fn connect(
        socket: &Path,
        harness: &str,
        purpose: &str,
        requested: &[Bundle],
    ) -> Result<Self, String> {
        let stream = UnixStream::connect(socket).map_err(|error| {
            format!(
                "cannot reach the agent seat at {}: {error}",
                socket.display()
            )
        })?;
        let write_half = stream
            .try_clone()
            .map_err(|error| format!("cannot split the agent seat socket: {error}"))?;
        let timeouts = stream
            .try_clone()
            .map_err(|error| format!("cannot split the agent seat socket: {error}"))?;
        let mut seat = Self {
            reader: BufReader::new(stream),
            writer: BufWriter::new(write_half),
            timeouts,
            limits: FrameLimits::DEFAULT,
            welcome: Welcome {
                protocol: String::new(),
                version: 0,
                manager: String::new(),
                session: nobox_agent_wire::SessionId::new(0),
                nonce: String::new(),
                granted: nobox_agent_wire::CapabilitySet::EMPTY,
                scoped: false,
                sequence: nobox_agent_wire::Sequence::ZERO,
                features: Vec::new(),
            },
            next_request: 1,
            events: VecDeque::new(),
        };
        let hello = Hello::new(harness, purpose).requesting(requested.iter().copied());
        seat.send(&ClientMessage::Hello(hello))?;
        match seat.receive()? {
            ServerMessage::Welcome(welcome) => {
                seat.welcome = welcome;
                Ok(seat)
            }
            ServerMessage::Goodbye(goodbye) => Err(format!(
                "the manager refused the handshake ({:?}): {}",
                goodbye.reason, goodbye.message
            )),
            other => Err(format!("expected a welcome, got {other:?}")),
        }
    }

    /// Returns what the manager granted this session.
    pub const fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Performs one tool call.
    ///
    /// # Errors
    ///
    /// Returns a message when the transport fails. A refusal by the manager is
    /// a successful round trip and comes back as an [`Outcome::Error`].
    pub fn call(&mut self, call: Call) -> Result<Outcome, String> {
        let id = RequestId::new(self.next_request);
        self.next_request += 1;
        self.send(&ClientMessage::Request(Request { id, call }))?;
        loop {
            match self.receive()? {
                ServerMessage::Response(response) if response.id == id => {
                    return Ok(response.outcome);
                }
                // Events interleave with responses once a session subscribes.
                // They are the agent's, not this call's, so they are buffered
                // for the next poll rather than discarded.
                ServerMessage::Event(envelope) => self.buffer(envelope),
                ServerMessage::Response(_) => {}
                ServerMessage::Welcome(_) => return Err("unexpected second welcome".to_owned()),
                ServerMessage::Goodbye(goodbye) => {
                    return Err(format!(
                        "the manager ended the session ({:?}): {}",
                        goodbye.reason, goodbye.message
                    ));
                }
            }
        }
    }

    /// Returns events after `after`, waiting up to `wait` for the first one.
    ///
    /// The sequence number is the explicit cross-request identifier a stateless
    /// protocol needs: an agent holds it, passes it back, and never depends on
    /// this process having been the one that saw the earlier events.
    ///
    /// # Errors
    ///
    /// Returns a message when the transport fails.
    pub fn poll_events(
        &mut self,
        after: Sequence,
        wait: Duration,
    ) -> Result<Vec<EventEnvelope>, String> {
        let wait = wait.min(MAX_POLL_WAIT);
        let deadline = Instant::now() + wait;
        loop {
            let ready: Vec<EventEnvelope> = self
                .events
                .iter()
                .filter(|envelope| envelope.sequence.raw() > after.raw())
                .cloned()
                .collect();
            if !ready.is_empty() {
                self.events
                    .retain(|envelope| envelope.sequence.raw() <= after.raw());
                return Ok(ready);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(Vec::new());
            }
            if !self.wait_for_frame(remaining)? {
                return Ok(Vec::new());
            }
        }
    }

    /// Waits for one frame to begin arriving, then reads it whole.
    ///
    /// The timeout applies only while nothing has arrived, so a slow frame is
    /// never truncated part-way and the stream cannot desynchronize.
    fn wait_for_frame(&mut self, remaining: Duration) -> Result<bool, String> {
        self.timeouts
            .set_read_timeout(Some(remaining))
            .map_err(|error| format!("cannot bound a poll: {error}"))?;
        let arrived = match self.reader.fill_buf() {
            Ok([]) => Err("the manager closed the session".to_owned()),
            Ok(_) => Ok(true),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(format!("cannot read from the agent seat: {error}")),
        };
        let _ = self.timeouts.set_read_timeout(None);
        if !arrived? {
            return Ok(false);
        }
        match self.receive()? {
            ServerMessage::Event(envelope) => self.buffer(envelope),
            ServerMessage::Goodbye(goodbye) => {
                return Err(format!(
                    "the manager ended the session ({:?}): {}",
                    goodbye.reason, goodbye.message
                ));
            }
            ServerMessage::Response(_) | ServerMessage::Welcome(_) => {}
        }
        Ok(true)
    }

    fn buffer(&mut self, envelope: EventEnvelope) {
        if self.events.len() >= MAX_BUFFERED_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(envelope);
    }

    fn send(&mut self, message: &ClientMessage) -> Result<(), String> {
        write_frame(&mut self.writer, message, &self.limits)
            .map_err(|error| format!("cannot write to the agent seat: {error}"))
    }

    fn receive(&mut self) -> Result<ServerMessage, String> {
        read_frame(&mut self.reader, &self.limits)
            .map_err(|error| format!("cannot read from the agent seat: {error}"))
    }
}

/// Resolves the socket to connect to.
///
/// An explicit path wins, then `AGENT_SEAT_SOCKET`, then the live X11
/// selection-bound root advertisement. A selected source never falls through
/// on error.
pub fn resolve_socket(explicit: Option<&Path>) -> Result<Option<PathBuf>, String> {
    if let Some(path) = explicit {
        return validate_socket_path(path, "--socket").map(Some);
    }
    if let Some(path) = std::env::var_os("AGENT_SEAT_SOCKET") {
        return validate_socket_path(Path::new(&path), "AGENT_SEAT_SOCKET").map(Some);
    }
    discover_x11_socket()
}

fn validate_socket_path(path: &Path, source: &str) -> Result<PathBuf, String> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() {
        return Err(format!("{source} is empty"));
    }
    if !path.is_absolute() {
        return Err(format!("{source} is not an absolute socket path"));
    }
    if bytes.contains(&0) {
        return Err(format!("{source} contains a NUL byte"));
    }
    if bytes.len() > MAX_SOCKET_PATH_BYTES {
        return Err(format!(
            "{source} is too long for a local socket ({} bytes, maximum {MAX_SOCKET_PATH_BYTES})",
            bytes.len()
        ));
    }
    Ok(path.to_path_buf())
}

fn discover_x11_socket() -> Result<Option<PathBuf>, String> {
    if std::env::var_os("DISPLAY").is_none_or(|display| display.is_empty()) {
        return Ok(None);
    }
    let (connection, screen_index) = x11rb::connect(None)
        .map_err(|error| format!("cannot inspect the X11 Agent Seat advertisement: {error}"))?;
    let screen = connection
        .setup()
        .roots
        .get(screen_index)
        .ok_or_else(|| format!("X11 screen {screen_index} does not exist"))?;
    let selection_name = format!("_AGENT_SEAT_S{screen_index}");
    let selection = connection
        .intern_atom(true, selection_name.as_bytes())
        .map_err(|error| format!("cannot look up {selection_name}: {error}"))?
        .reply()
        .map_err(|error| format!("cannot look up {selection_name}: {error}"))?
        .atom;
    if selection == NONE {
        return Ok(None);
    }
    let owner = connection
        .get_selection_owner(selection)
        .map_err(|error| format!("cannot inspect {selection_name} ownership: {error}"))?
        .reply()
        .map_err(|error| format!("cannot inspect {selection_name} ownership: {error}"))?
        .owner;
    if owner == NONE {
        return Ok(None);
    }
    let property = connection
        .intern_atom(true, nobox_agent_wire::ADVERTISEMENT_PROPERTY.as_bytes())
        .map_err(|error| format!("cannot look up the Agent Seat property: {error}"))?
        .reply()
        .map_err(|error| format!("cannot look up the Agent Seat property: {error}"))?
        .atom;
    let utf8 = connection
        .intern_atom(true, b"UTF8_STRING")
        .map_err(|error| format!("cannot look up UTF8_STRING: {error}"))?
        .reply()
        .map_err(|error| format!("cannot look up UTF8_STRING: {error}"))?
        .atom;
    if property == NONE || utf8 == NONE {
        return Ok(None);
    }
    let Some(root_value) = read_advertisement(&connection, screen.root, property, utf8)? else {
        return Ok(None);
    };
    let Some(owner_value) = read_advertisement(&connection, owner, property, utf8)? else {
        return Ok(None);
    };
    if root_value != owner_value {
        return Ok(None);
    }
    let current_owner = connection
        .get_selection_owner(selection)
        .map_err(|error| format!("cannot recheck {selection_name} ownership: {error}"))?
        .reply()
        .map_err(|error| format!("cannot recheck {selection_name} ownership: {error}"))?
        .owner;
    if current_owner != owner {
        return Ok(None);
    }
    let encoded = std::str::from_utf8(&root_value)
        .map_err(|_| "the Agent Seat advertisement is not UTF-8".to_owned())?;
    let advertisement = nobox_agent_wire::Advertisement::parse(encoded)
        .filter(|advertisement| advertisement.encode().as_bytes() == root_value)
        .ok_or_else(|| "the Agent Seat advertisement is malformed".to_owned())?;
    if !advertisement.is_compatible() {
        return Err(format!(
            "the advertised Agent Seat protocol {} revision {} is incompatible with {} revision {}",
            advertisement.protocol,
            advertisement.version,
            nobox_agent_wire::PROTOCOL_NAME,
            nobox_agent_wire::PROTOCOL_VERSION
        ));
    }
    validate_socket_path(
        Path::new(&advertisement.socket),
        "the Agent Seat advertisement",
    )
    .map(Some)
}

fn read_advertisement<C: Connection>(
    connection: &C,
    window: u32,
    property: u32,
    utf8: u32,
) -> Result<Option<Vec<u8>>, String> {
    let reply = connection
        .get_property(
            false,
            window,
            property,
            AtomEnum::ANY,
            0,
            MAX_ADVERTISEMENT_LONGS,
        )
        .map_err(|error| format!("cannot read the Agent Seat advertisement: {error}"))?
        .reply()
        .map_err(|error| format!("cannot read the Agent Seat advertisement: {error}"))?;
    if reply.type_ == NONE {
        return Ok(None);
    }
    if reply.type_ != utf8
        || reply.format != 8
        || reply.bytes_after != 0
        || reply.value.len() > MAX_ADVERTISEMENT_BYTES
    {
        return Err(
            "the Agent Seat advertisement has an invalid X11 type, format, or size".to_owned(),
        );
    }
    Ok(Some(reply.value))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{MAX_SOCKET_PATH_BYTES, resolve_socket, validate_socket_path};

    #[test]
    fn an_explicit_path_wins() {
        assert_eq!(
            resolve_socket(Some(Path::new("/tmp/seat.sock"))),
            Ok(Some(std::path::PathBuf::from("/tmp/seat.sock")))
        );
    }

    #[test]
    fn explicit_socket_paths_are_absolute_nonempty_and_bounded() {
        assert!(validate_socket_path(Path::new(""), "test").is_err());
        assert!(validate_socket_path(Path::new("relative.sock"), "test").is_err());
        let long = format!("/{}", "x".repeat(MAX_SOCKET_PATH_BYTES));
        assert!(validate_socket_path(Path::new(&long), "test").is_err());
        assert_eq!(
            validate_socket_path(Path::new("/tmp/seat.sock"), "test"),
            Ok(PathBuf::from("/tmp/seat.sock"))
        );
    }
}
