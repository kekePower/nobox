//! Frame encoding: a big-endian length prefix followed by one JSON value.
//!
//! Every bound is checked before anything is allocated, so a hostile or
//! broken peer cannot make the manager reserve memory on its behalf. Bounds
//! are per message type rather than global: a handshake has no business being
//! the size of a screenshot.

use std::io::{Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{ErrorCode, ProtocolError};
use crate::message::{ClientMessage, Reply, ServerMessage};

/// Bytes in the length prefix.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// Per-message-type frame bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLimits {
    /// Largest handshake or session-control frame.
    pub handshake: usize,
    /// Largest request frame.
    pub request: usize,
    /// Largest ordinary response frame, including a full snapshot.
    pub response: usize,
    /// Largest response frame carrying pixels.
    pub capture: usize,
    /// Largest event frame.
    pub event: usize,
}

impl FrameLimits {
    /// Bounds sized for a desktop session: enough for a busy snapshot and a
    /// full-output capture, and nowhere near enough to be a memory lever.
    pub const DEFAULT: Self = Self {
        handshake: 8 * 1024,
        request: 64 * 1024,
        response: 4 * 1024 * 1024,
        capture: 32 * 1024 * 1024,
        event: 256 * 1024,
    };

    /// Returns the largest frame that may arrive from `direction`'s sender.
    #[must_use]
    pub const fn incoming_max(&self, direction: Direction) -> usize {
        match direction {
            Direction::ToManager => {
                if self.handshake > self.request {
                    self.handshake
                } else {
                    self.request
                }
            }
            Direction::ToAgent => {
                let mut max = self.handshake;
                if self.response > max {
                    max = self.response;
                }
                if self.capture > max {
                    max = self.capture;
                }
                if self.event > max {
                    max = self.event;
                }
                max
            }
        }
    }
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Which way a frame travels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    /// Companion to manager.
    ToManager,
    /// Manager to companion.
    ToAgent,
}

/// A message that knows its own size bound.
pub trait Bounded {
    /// Returns the bound that applies to this specific message.
    fn frame_limit(&self, limits: &FrameLimits) -> usize;

    /// Returns the direction this message travels.
    fn direction() -> Direction;
}

impl Bounded for ClientMessage {
    fn frame_limit(&self, limits: &FrameLimits) -> usize {
        match self {
            Self::Hello(_) => limits.handshake,
            Self::Request(_) => limits.request,
        }
    }

    fn direction() -> Direction {
        Direction::ToManager
    }
}

impl Bounded for ServerMessage {
    fn frame_limit(&self, limits: &FrameLimits) -> usize {
        match self {
            Self::Welcome(_) | Self::Goodbye(_) => limits.handshake,
            Self::Event(_) => limits.event,
            Self::Response(response) => {
                if matches!(
                    &response.outcome,
                    crate::message::Outcome::Ok {
                        reply: Reply::Capture { .. }
                    }
                ) {
                    limits.capture
                } else {
                    limits.response
                }
            }
        }
    }

    fn direction() -> Direction {
        Direction::ToAgent
    }
}

/// Why a frame could not be read or written.
#[derive(Debug)]
pub enum CodecError {
    /// The peer closed the connection cleanly at a frame boundary.
    Closed,
    /// The transport failed.
    Io(std::io::Error),
    /// The frame exceeded the bound for its direction or its type.
    TooLarge {
        /// Declared or encoded length.
        length: usize,
        /// Bound that was exceeded.
        limit: usize,
    },
    /// The frame was not a JSON value this protocol version accepts.
    Malformed(serde_json::Error),
}

impl CodecError {
    /// Returns the protocol error to report before dropping the session. Every
    /// codec failure is fatal: a peer that cannot frame correctly cannot be
    /// trusted to recover mid-stream.
    #[must_use]
    pub fn as_protocol_error(&self) -> ProtocolError {
        match self {
            Self::Closed => ProtocolError::new(ErrorCode::Malformed, "connection closed"),
            Self::Io(error) => ProtocolError::new(ErrorCode::Internal, error.to_string()),
            Self::TooLarge { length, limit } => ProtocolError::new(
                ErrorCode::TooLarge,
                format!("frame of {length} bytes exceeds the {limit} byte bound"),
            ),
            Self::Malformed(error) => ProtocolError::new(ErrorCode::Malformed, error.to_string()),
        }
    }

    /// Returns whether this is an ordinary end of stream rather than a fault.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => formatter.write_str("connection closed"),
            Self::Io(error) => write!(formatter, "transport failed: {error}"),
            Self::TooLarge { length, limit } => {
                write!(formatter, "frame of {length} bytes exceeds {limit} bytes")
            }
            Self::Malformed(error) => write!(formatter, "malformed frame: {error}"),
        }
    }
}

impl std::error::Error for CodecError {}

impl From<std::io::Error> for CodecError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for CodecError {
    fn from(error: serde_json::Error) -> Self {
        Self::Malformed(error)
    }
}

/// Reads one framed message.
///
/// The declared length is checked against the direction's bound before any
/// buffer is allocated, and against the concrete message type's bound after
/// parsing.
///
/// # Errors
///
/// Returns [`CodecError::Closed`] at a clean end of stream, and a fatal error
/// for any oversized, truncated, or malformed frame.
pub fn read_frame<T>(reader: &mut impl Read, limits: &FrameLimits) -> Result<T, CodecError>
where
    T: DeserializeOwned + Bounded,
{
    let mut prefix = [0_u8; LENGTH_PREFIX_BYTES];
    let mut filled = 0;
    while filled < LENGTH_PREFIX_BYTES {
        let read = reader.read(&mut prefix[filled..])?;
        if read == 0 {
            if filled == 0 {
                return Err(CodecError::Closed);
            }
            return Err(CodecError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "truncated frame length",
            )));
        }
        filled += read;
    }
    let length = u32::from_be_bytes(prefix) as usize;
    let incoming = limits.incoming_max(T::direction());
    if length > incoming {
        return Err(CodecError::TooLarge {
            length,
            limit: incoming,
        });
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    let message: T = serde_json::from_slice(&body)?;
    let limit = message.frame_limit(limits);
    if length > limit {
        return Err(CodecError::TooLarge { length, limit });
    }
    Ok(message)
}

/// Writes one framed message.
///
/// # Errors
///
/// Returns an error when the encoded message exceeds its own bound, or when
/// the transport fails.
pub fn write_frame<T>(
    writer: &mut impl Write,
    message: &T,
    limits: &FrameLimits,
) -> Result<(), CodecError>
where
    T: Serialize + Bounded,
{
    let body = serde_json::to_vec(message)?;
    let limit = message.frame_limit(limits);
    if body.len() > limit {
        return Err(CodecError::TooLarge {
            length: body.len(),
            limit,
        });
    }
    let length = u32::try_from(body.len()).map_err(|_| CodecError::TooLarge {
        length: body.len(),
        limit,
    })?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{CodecError, Direction, FrameLimits, read_frame, write_frame};
    use crate::base64::Base64Bytes;
    use crate::error::ProtocolError;
    use crate::ids::{Rect, RequestId, Sequence};
    use crate::message::{
        Call, CaptureImage, ClientMessage, Hello, ImageFormat, Outcome, Reply, Request, Response,
        ServerMessage,
    };

    fn snapshot_request() -> ClientMessage {
        ClientMessage::Request(Request {
            id: RequestId::new(1),
            call: Call::DesktopSnapshot {},
        })
    }

    #[test]
    fn frames_round_trip_through_a_stream() {
        let limits = FrameLimits::DEFAULT;
        let mut buffer = Vec::new();
        let hello = ClientMessage::Hello(Hello::new("harness", "test"));
        write_frame(&mut buffer, &hello, &limits).expect("writes");
        write_frame(&mut buffer, &snapshot_request(), &limits).expect("writes");

        let mut cursor = Cursor::new(buffer);
        let first: ClientMessage = read_frame(&mut cursor, &limits).expect("reads");
        let second: ClientMessage = read_frame(&mut cursor, &limits).expect("reads");
        assert_eq!(first, hello);
        assert_eq!(second, snapshot_request());
        let end = read_frame::<ClientMessage>(&mut cursor, &limits).expect_err("ends");
        assert!(end.is_closed());
    }

    #[test]
    fn an_oversized_prefix_is_refused_before_allocating() {
        let limits = FrameLimits::DEFAULT;
        let mut frame = (u32::MAX).to_be_bytes().to_vec();
        frame.extend_from_slice(b"{}");
        let mut cursor = Cursor::new(frame);
        let error = read_frame::<ClientMessage>(&mut cursor, &limits).expect_err("refuses");
        match error {
            CodecError::TooLarge { length, limit } => {
                assert_eq!(length, u32::MAX as usize);
                assert_eq!(limit, limits.incoming_max(Direction::ToManager));
            }
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn a_request_may_not_borrow_the_capture_bound() {
        let limits = FrameLimits {
            request: 32,
            ..FrameLimits::DEFAULT
        };
        let mut buffer = Vec::new();
        let error = write_frame(&mut buffer, &snapshot_request(), &limits).expect_err("refuses");
        assert!(matches!(error, CodecError::TooLarge { limit: 32, .. }));
        assert!(buffer.is_empty(), "nothing is written past the bound");
    }

    #[test]
    fn captures_use_the_large_bound_and_other_responses_do_not() {
        let limits = FrameLimits::DEFAULT;
        let capture = ServerMessage::Response(Response {
            id: RequestId::new(1),
            sequence: Sequence::new(4),
            outcome: Outcome::Ok {
                reply: Reply::Capture {
                    image: CaptureImage {
                        format: ImageFormat::Png,
                        width: 2,
                        height: 2,
                        source: Rect::new(0, 0, 2, 2),
                        content: Some(Rect::new(0, 0, 2, 2)),
                        grid: None,
                        sequence: Sequence::new(4),
                        data: Base64Bytes::new(vec![0; 16]),
                    },
                },
            },
        });
        let denied = ServerMessage::Response(Response {
            id: RequestId::new(2),
            sequence: Sequence::new(4),
            outcome: Outcome::Error {
                error: ProtocolError::denied("no grant"),
            },
        });
        use super::Bounded;
        assert_eq!(capture.frame_limit(&limits), limits.capture);
        assert_eq!(denied.frame_limit(&limits), limits.response);

        let mut buffer = Vec::new();
        write_frame(&mut buffer, &capture, &limits).expect("writes");
        let mut cursor = Cursor::new(buffer);
        let decoded: ServerMessage = read_frame(&mut cursor, &limits).expect("reads");
        assert_eq!(decoded, capture);
    }

    #[test]
    fn a_truncated_frame_fails_without_blocking_forever() {
        let limits = FrameLimits::DEFAULT;
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &snapshot_request(), &limits).expect("writes");
        buffer.truncate(buffer.len() - 3);
        let mut cursor = Cursor::new(buffer);
        let error = read_frame::<ClientMessage>(&mut cursor, &limits).expect_err("fails");
        assert!(!error.is_closed());
        assert!(matches!(error, CodecError::Io(_)));
    }

    #[test]
    fn a_half_written_length_prefix_is_not_a_clean_close() {
        let limits = FrameLimits::DEFAULT;
        let mut cursor = Cursor::new(vec![0_u8, 0_u8]);
        let error = read_frame::<ClientMessage>(&mut cursor, &limits).expect_err("fails");
        assert!(!error.is_closed());
    }

    #[test]
    fn garbage_and_unknown_fields_are_rejected_as_malformed() {
        let limits = FrameLimits::DEFAULT;
        for body in [
            b"not json".to_vec(),
            br#"{"request":{"id":1,"call":{"tool":"client.get","client":1,"pid":9}}}"#.to_vec(),
            br#"{"unknown_frame":{}}"#.to_vec(),
        ] {
            let mut frame = u32::try_from(body.len())
                .expect("fits")
                .to_be_bytes()
                .to_vec();
            frame.extend_from_slice(&body);
            let mut cursor = Cursor::new(frame);
            let error = read_frame::<ClientMessage>(&mut cursor, &limits).expect_err("rejects");
            assert!(matches!(error, CodecError::Malformed(_)), "{error}");
            assert!(error.as_protocol_error().is_fatal());
        }
    }

    #[test]
    fn direction_bounds_cover_every_message_type() {
        let limits = FrameLimits::DEFAULT;
        assert_eq!(
            limits.incoming_max(Direction::ToManager),
            limits.request.max(limits.handshake)
        );
        assert_eq!(limits.incoming_max(Direction::ToAgent), limits.capture);
    }
}
