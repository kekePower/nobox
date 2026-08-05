//! The companion's client side of the Agent Seat Protocol.
//!
//! This half enforces nothing. Every request it sends is validated again by
//! the window manager against the session's grant, so a compromised companion
//! gains exactly the grant the manager already issued and nothing more.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agent_seat_proto::{
    Call, ClientMessage, EventEnvelope, FrameLimits, Hello, Outcome, Request, RequestId, Sequence,
    ServerMessage, Welcome, read_frame, write_frame,
};

/// Events held for a poll that has not been made yet.
///
/// The manager already bounds its own queue and asks for a re-snapshot when it
/// overflows, so this only has to survive the gap between two polls.
const MAX_BUFFERED_EVENTS: usize = 4096;

/// Longest a poll may wait before answering with nothing.
pub const MAX_POLL_WAIT: Duration = Duration::from_secs(30);

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
    /// Connects to a manager's socket and completes the handshake.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message when the socket is unreachable, the
    /// manager speaks a different protocol version, or the handshake is
    /// refused.
    pub fn connect(socket: &Path, harness: &str, purpose: &str) -> Result<Self, String> {
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
                session: agent_seat_proto::SessionId::new(0),
                nonce: String::new(),
                granted: agent_seat_proto::CapabilitySet::EMPTY,
                scoped: false,
                sequence: agent_seat_proto::Sequence::ZERO,
                features: Vec::new(),
            },
            next_request: 1,
            events: VecDeque::new(),
        };
        let hello = Hello::new(harness, purpose);
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
/// An explicit path wins, then `AGENT_SEAT_SOCKET`, then the conventional
/// per-display location. A window manager also advertises its socket in the
/// `_AGENT_SEAT` root property, which a host can read with `xprop` when the
/// conventional path does not apply.
#[must_use]
pub fn resolve_socket(explicit: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("AGENT_SEAT_SOCKET") {
        return Some(PathBuf::from(path));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let display = display.trim_start_matches(':');
    let display: String = display
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let display = if display.is_empty() {
        "0".to_owned()
    } else {
        display
    };
    Some(
        PathBuf::from(runtime)
            .join("nobox")
            .join(format!("agent-seat-{display}.sock")),
    )
}

#[cfg(test)]
mod tests {
    use super::resolve_socket;

    #[test]
    fn an_explicit_path_wins() {
        assert_eq!(
            resolve_socket(Some("/tmp/seat.sock")),
            Some(std::path::PathBuf::from("/tmp/seat.sock"))
        );
    }
}
