//! The Agent Seat Protocol wire format.
//!
//! An agent seat is a second seat on a desktop session: controlled by the
//! window manager, attributable, and subordinate to the human seat. This crate
//! is the contract between a manager that offers such a seat and a companion
//! process that translates it for an agent harness. It contains types and
//! framing only — no policy, no I/O loop, and no dependency on any particular
//! window manager or display server.
//!
//! Identities on this wire are manager-assigned handles. X11 window
//! identifiers, atoms, and Wayland object identifiers never appear, so the
//! same protocol serves an X11 window manager and a Wayland compositor.
//!
//! Discovery follows the traditional X11 route: a manager advertises
//! [`ADVERTISEMENT_PROPERTY`] on the root window, naming the protocol version
//! and the socket path, so a companion needs no side channel to find the seat.
//!
//! ```
//! use agent_seat_proto::{ClientMessage, FrameLimits, Hello, read_frame, write_frame};
//!
//! let limits = FrameLimits::DEFAULT;
//! let hello = ClientMessage::Hello(Hello::new("example-harness", "documentation"));
//! let mut wire = Vec::new();
//! write_frame(&mut wire, &hello, &limits).expect("frame is written");
//!
//! let decoded: ClientMessage = read_frame(&mut wire.as_slice(), &limits).expect("frame is read");
//! assert_eq!(decoded, hello);
//! ```

pub mod base64;
pub mod capability;
pub mod codec;
pub mod error;
pub mod ids;
pub mod message;

pub use base64::Base64Bytes;
pub use capability::{Bundle, Capability, CapabilitySet};
pub use codec::{Bounded, CodecError, Direction, FrameLimits, read_frame, write_frame};
pub use error::{ErrorCode, ProtocolError};
pub use ids::{ClientId, Generation, OutputId, Rect, RequestId, Sequence, SessionId, WorkspaceId};
pub use message::{
    ApplicationIdentity, ApplicationKind, Call, CaptureArea, CaptureImage, ClientDescriptor,
    ClientMessage, ClientState, DesktopSnapshot, DisconnectReason, Event, EventEnvelope, EventKind,
    Expects, Feature, GeometryRequest, Goodbye, Hello, HumanActivityKind, ImageFormat, KeyAction,
    Modifier, Outcome, OutputDescriptor, PointerAction, PointerButton, Reply, Request, Response,
    ServerMessage, SessionChange, StateChange, Step, Welcome, WorkspaceDescriptor,
};

/// The protocol's name on the wire and in its advertisement.
pub const PROTOCOL_NAME: &str = "agent-seat";

/// The protocol version this crate implements.
///
/// Pre-1.0 versions may break without compatibility shims. Both peers refuse a
/// version they do not implement rather than guessing at the difference.
pub const PROTOCOL_VERSION: u32 = 1;

/// Root-window property naming an available agent seat.
pub const ADVERTISEMENT_PROPERTY: &str = "_AGENT_SEAT";

/// Field separator inside [`ADVERTISEMENT_PROPERTY`]. A nul keeps socket paths
/// containing spaces unambiguous.
const ADVERTISEMENT_SEPARATOR: char = '\u{0}';

/// What a manager publishes to say an agent seat exists and where it lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Advertisement {
    /// Protocol name, normally [`PROTOCOL_NAME`].
    pub protocol: String,
    /// Protocol version the manager implements.
    pub version: u32,
    /// Absolute path of the manager's listening socket.
    pub socket: String,
}

impl Advertisement {
    /// Builds an advertisement for this build of the protocol.
    #[must_use]
    pub fn new(socket: impl Into<String>) -> Self {
        Self {
            protocol: PROTOCOL_NAME.to_owned(),
            version: PROTOCOL_VERSION,
            socket: socket.into(),
        }
    }

    /// Encodes the property value.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{}{ADVERTISEMENT_SEPARATOR}{}{ADVERTISEMENT_SEPARATOR}{}",
            self.protocol, self.version, self.socket
        )
    }

    /// Parses a property value, returning `None` for anything malformed.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let mut fields = value.split(ADVERTISEMENT_SEPARATOR);
        let protocol = fields.next()?;
        let version = fields.next()?.parse().ok()?;
        let socket = fields.next()?;
        if protocol.is_empty() || socket.is_empty() || fields.next().is_some() {
            return None;
        }
        Some(Self {
            protocol: protocol.to_owned(),
            version,
            socket: socket.to_owned(),
        })
    }

    /// Returns whether this crate can speak to the advertised seat.
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        self.protocol == PROTOCOL_NAME && self.version == PROTOCOL_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::{ADVERTISEMENT_PROPERTY, Advertisement, PROTOCOL_NAME, PROTOCOL_VERSION};

    #[test]
    fn advertisements_round_trip_including_awkward_paths() {
        let advertisement = Advertisement::new("/run/user/1000/no box/agent-seat-0.sock");
        let encoded = advertisement.encode();
        assert_eq!(Advertisement::parse(&encoded), Some(advertisement.clone()));
        assert!(advertisement.is_compatible());
        assert_eq!(advertisement.protocol, PROTOCOL_NAME);
        assert_eq!(advertisement.version, PROTOCOL_VERSION);
    }

    #[test]
    fn malformed_advertisements_are_rejected() {
        assert_eq!(Advertisement::parse(""), None);
        assert_eq!(Advertisement::parse("agent-seat"), None);
        assert_eq!(Advertisement::parse("agent-seat\u{0}1"), None);
        assert_eq!(Advertisement::parse("agent-seat\u{0}x\u{0}/tmp/s"), None);
        assert_eq!(Advertisement::parse("agent-seat\u{0}1\u{0}"), None);
        assert_eq!(
            Advertisement::parse("agent-seat\u{0}1\u{0}/tmp/s\u{0}extra"),
            None
        );
    }

    #[test]
    fn foreign_protocols_and_versions_are_not_compatible() {
        let mut advertisement = Advertisement::new("/tmp/s");
        advertisement.version = PROTOCOL_VERSION + 1;
        assert!(!advertisement.is_compatible());
        advertisement.version = PROTOCOL_VERSION;
        advertisement.protocol = "other".to_owned();
        assert!(!advertisement.is_compatible());
    }

    #[test]
    fn the_advertised_property_name_is_protocol_neutral() {
        assert!(!ADVERTISEMENT_PROPERTY.to_lowercase().contains("nobox"));
        assert!(!PROTOCOL_NAME.contains("nobox"));
    }
}
