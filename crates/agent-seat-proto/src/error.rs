//! Structured protocol errors.
//!
//! Errors are part of the contract, not diagnostics: a denied, stale, or
//! interrupted request must say exactly what happened and exactly which steps
//! committed. Hidden clients deliberately share [`ErrorCode::NoSuchClient`]
//! with genuinely nonexistent ones so errors cannot be used as an oracle.

use serde::{Deserialize, Serialize};

use crate::ids::Generation;
use crate::message::Step;

/// The machine-readable reason a request failed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The frame was not valid for this protocol version.
    Malformed,
    /// The peer offered a protocol name or version the manager does not speak.
    UnsupportedVersion,
    /// A hello was expected first, or a second hello arrived.
    HandshakeOrder,
    /// The declared identity failed the manager's bounds.
    InvalidIdentity,
    /// The request names a capability this session was not granted. Returned
    /// identically whether the grant is absent, narrowed, or scoped away.
    Denied,
    /// The named client does not exist, is out of the session's scope, or is
    /// hidden by an application rule. The three are indistinguishable.
    NoSuchClient,
    /// The named workspace or output does not exist.
    NoSuchTarget,
    /// A freshness precondition no longer holds.
    StaleState,
    /// Human input preempted the request.
    Interrupted,
    /// The manager or backend cannot perform this operation at all, such as
    /// obscured capture without composite redirection.
    Unsupported,
    /// The request was well-formed but its arguments were not usable.
    InvalidArgument,
    /// The session is frozen by the kill chord and accepts no further work.
    SessionFrozen,
    /// The session's grant was revoked.
    SessionRevoked,
    /// The launch was refused by launch policy.
    LaunchDenied,
    /// The request exceeded a protocol bound.
    TooLarge,
    /// The manager failed internally; the agent may retry.
    Internal,
}

impl ErrorCode {
    /// Returns the wire name of this code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::UnsupportedVersion => "unsupported_version",
            Self::HandshakeOrder => "handshake_order",
            Self::InvalidIdentity => "invalid_identity",
            Self::Denied => "denied",
            Self::NoSuchClient => "no_such_client",
            Self::NoSuchTarget => "no_such_target",
            Self::StaleState => "stale_state",
            Self::Interrupted => "interrupted",
            Self::Unsupported => "unsupported",
            Self::InvalidArgument => "invalid_argument",
            Self::SessionFrozen => "session_frozen",
            Self::SessionRevoked => "session_revoked",
            Self::LaunchDenied => "launch_denied",
            Self::TooLarge => "too_large",
            Self::Internal => "internal",
        }
    }

    /// Returns whether the session must be closed after reporting this code.
    #[must_use]
    pub const fn is_fatal(self) -> bool {
        matches!(
            self,
            Self::Malformed
                | Self::UnsupportedVersion
                | Self::HandshakeOrder
                | Self::InvalidIdentity
                | Self::TooLarge
        )
    }
}

/// A failed request, with the structured detail its code implies.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolError {
    /// Machine-readable reason.
    pub code: ErrorCode,
    /// Human-readable detail. Never load-bearing for agent logic, and never
    /// discloses anything the code itself withholds.
    pub message: String,
    /// The client's current generation, when the code is
    /// [`ErrorCode::StaleState`], so the agent can re-observe precisely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_generation: Option<Generation>,
    /// Steps that were already committed when the request failed. Always
    /// present for [`ErrorCode::Interrupted`]; never omitted to imply that
    /// nothing happened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub committed: Vec<Step>,
}

impl ProtocolError {
    /// Builds an error carrying no structured detail.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            current_generation: None,
            committed: Vec::new(),
        }
    }

    /// Builds the deny-by-default refusal for a missing capability.
    #[must_use]
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Denied, message)
    }

    /// Builds the shared refusal for absent, out-of-scope, and hidden clients.
    #[must_use]
    pub fn no_such_client() -> Self {
        Self::new(ErrorCode::NoSuchClient, "no such client")
    }

    /// Builds a freshness rejection naming the client's current generation.
    #[must_use]
    pub fn stale_state(current: Generation) -> Self {
        Self {
            current_generation: Some(current),
            ..Self::new(ErrorCode::StaleState, "precondition no longer holds")
        }
    }

    /// Builds a preemption result naming the steps that did commit.
    #[must_use]
    pub fn interrupted(committed: Vec<Step>) -> Self {
        Self {
            committed,
            ..Self::new(ErrorCode::Interrupted, "preempted by human input")
        }
    }

    /// Returns whether the session must be closed after this error.
    #[must_use]
    pub const fn is_fatal(&self) -> bool {
        self.code.is_fatal()
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::{ErrorCode, ProtocolError};
    use crate::ids::Generation;
    use crate::message::Step;

    #[test]
    fn absent_and_hidden_clients_share_one_encoding() {
        let absent = ProtocolError::no_such_client();
        let hidden = ProtocolError::no_such_client();
        assert_eq!(
            serde_json::to_string(&absent).expect("encodes"),
            serde_json::to_string(&hidden).expect("encodes")
        );
        assert_eq!(
            serde_json::to_string(&absent).expect("encodes"),
            "{\"code\":\"no_such_client\",\"message\":\"no such client\"}"
        );
    }

    #[test]
    fn stale_state_carries_the_current_generation() {
        let error = ProtocolError::stale_state(Generation::new(9));
        assert_eq!(error.current_generation, Some(Generation::new(9)));
        let encoded = serde_json::to_string(&error).expect("encodes");
        assert!(encoded.contains("\"current_generation\":9"), "{encoded}");
    }

    #[test]
    fn interruption_always_encodes_the_committed_steps() {
        let error = ProtocolError::interrupted(vec![Step::Activate, Step::Raise]);
        let encoded = serde_json::to_string(&error).expect("encodes");
        assert!(
            encoded.contains("\"committed\":[\"activate\",\"raise\"]"),
            "{encoded}"
        );
        let decoded: ProtocolError = serde_json::from_str(&encoded).expect("decodes");
        assert_eq!(decoded, error);
    }

    #[test]
    fn unknown_error_fields_are_rejected() {
        let decoded = serde_json::from_str::<ProtocolError>(
            "{\"code\":\"denied\",\"message\":\"no\",\"retry_after\":5}",
        );
        assert!(decoded.is_err());
    }

    #[test]
    fn handshake_failures_are_fatal_and_request_failures_are_not() {
        assert!(ErrorCode::UnsupportedVersion.is_fatal());
        assert!(ErrorCode::Malformed.is_fatal());
        assert!(!ErrorCode::Denied.is_fatal());
        assert!(!ErrorCode::StaleState.is_fatal());
    }
}
