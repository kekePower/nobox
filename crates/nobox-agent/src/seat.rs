//! The companion's client side of the Agent Seat Protocol.
//!
//! This half enforces nothing. Every request it sends is validated again by
//! the window manager against the session's grant, so a compromised companion
//! gains exactly the grant the manager already issued and nothing more.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use agent_seat_proto::{
    Call, ClientMessage, FrameLimits, Hello, Outcome, Request, RequestId, ServerMessage, Welcome,
    read_frame, write_frame,
};

/// A connected, greeted session.
pub struct Seat {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
    limits: FrameLimits,
    welcome: Welcome,
    next_request: u64,
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
        let mut seat = Self {
            reader: BufReader::new(stream),
            writer: BufWriter::new(write_half),
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
                // Events may interleave with responses once a session
                // subscribes; anything not answering this request is not this
                // call's business.
                ServerMessage::Response(_) | ServerMessage::Event(_) => {}
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
