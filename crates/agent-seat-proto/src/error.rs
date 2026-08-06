//! Structured protocol errors.
//!
//! Errors are part of the contract, not diagnostics: a denied, stale, or
//! interrupted request must say exactly what happened and exactly which steps
//! committed. Hidden clients deliberately share [`ErrorCode::NoSuchClient`]
//! with genuinely nonexistent ones so errors cannot be used as an oracle.

use serde::{Deserialize, Serialize};

use crate::ids::{ActionId, Generation};
use crate::message::Step;

/// What shape or constraint would make an argument usable.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    /// Broad JSON shape or correction operation.
    pub kind: ExpectedKind,
    /// Inclusive numeric lower bound, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i64>,
    /// Inclusive numeric upper bound, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<u64>,
    /// Inclusive string-length lower bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    /// Inclusive string-length upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
    /// Inclusive array-length upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    /// Exact permitted values for [`ExpectedKind::Enum`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// Object fields of which at least one must be present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_any: Vec<String>,
}

impl Expected {
    /// Expects a JSON value of one broad kind.
    #[must_use]
    pub const fn kind(kind: ExpectedKind) -> Self {
        Self {
            kind,
            minimum: None,
            maximum: None,
            min_length: None,
            max_length: None,
            max_items: None,
            values: Vec::new(),
            required_any: Vec::new(),
        }
    }

    /// Expects an integer inside an optional inclusive range.
    #[must_use]
    pub fn integer(minimum: Option<i64>, maximum: Option<u64>) -> Self {
        Self {
            minimum,
            maximum,
            ..Self::kind(ExpectedKind::Integer)
        }
    }

    /// Expects a string inside an inclusive length range.
    #[must_use]
    pub fn string(min_length: Option<usize>, max_length: Option<usize>) -> Self {
        Self {
            min_length: min_length.and_then(|value| u64::try_from(value).ok()),
            max_length: max_length.and_then(|value| u64::try_from(value).ok()),
            ..Self::kind(ExpectedKind::String)
        }
    }

    /// Expects an array no longer than `max_items`, when bounded.
    #[must_use]
    pub fn array(max_items: Option<usize>) -> Self {
        Self {
            max_items: max_items.and_then(|value| u64::try_from(value).ok()),
            ..Self::kind(ExpectedKind::Array)
        }
    }

    /// Expects one value from a closed set.
    #[must_use]
    pub fn one_of<I, S>(values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            values: values.into_iter().map(Into::into).collect(),
            ..Self::kind(ExpectedKind::Enum)
        }
    }

    /// Expects an object containing at least one named field.
    #[must_use]
    pub fn object_with_any<I, S>(fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            required_any: fields.into_iter().map(Into::into).collect(),
            ..Self::kind(ExpectedKind::Object)
        }
    }
}

/// Broad expected shape or correction operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedKind {
    /// The field is not accepted and must be removed.
    Absent,
    /// A JSON boolean.
    Boolean,
    /// A JSON integer.
    Integer,
    /// A JSON string.
    String,
    /// A JSON array.
    Array,
    /// A JSON object.
    Object,
    /// One string from the accompanying `values` set.
    Enum,
}

/// The broad JSON kind that was actually supplied.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceivedKind {
    /// The required field was absent.
    Missing,
    /// JSON null.
    Null,
    /// A JSON boolean.
    Boolean,
    /// An integral JSON number.
    Integer,
    /// A non-integral JSON number.
    Number,
    /// A JSON string.
    String,
    /// A JSON array.
    Array,
    /// A JSON object.
    Object,
}

/// When repeating a failed operation can be useful.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Retryability {
    /// Repeating cannot make this operation supported.
    Never,
    /// Correct the request fields before retrying.
    AfterCorrection,
    /// Refresh the relevant desktop state before retrying.
    AfterObservation,
    /// Wait until the person stops interacting before retrying.
    AfterHumanIdle,
    /// Wait for the user to resume the frozen session.
    AfterSessionResume,
    /// The user must change a grant or policy first.
    AfterPolicyChange,
    /// A transient internal failure may be retried once unchanged.
    Immediate,
}

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

    /// Returns the retry condition models should use without parsing prose.
    #[must_use]
    pub const fn retryability(self) -> Retryability {
        match self {
            Self::Malformed
            | Self::UnsupportedVersion
            | Self::HandshakeOrder
            | Self::InvalidIdentity
            | Self::InvalidArgument
            | Self::TooLarge => Retryability::AfterCorrection,
            Self::Denied | Self::SessionRevoked | Self::LaunchDenied => {
                Retryability::AfterPolicyChange
            }
            Self::NoSuchClient | Self::NoSuchTarget | Self::StaleState => {
                Retryability::AfterObservation
            }
            Self::Interrupted => Retryability::AfterHumanIdle,
            Self::Unsupported => Retryability::Never,
            Self::SessionFrozen => Retryability::AfterSessionResume,
            Self::Internal => Retryability::Immediate,
        }
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
    /// JSON Pointer locating the unusable argument, relative to the call's
    /// argument object. Absent when correcting arguments is not the remedy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Machine-readable shape or constraint required at `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<Box<Expected>>,
    /// Broad kind actually received at `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received: Option<ReceivedKind>,
    /// Exact condition under which retrying can be useful.
    pub retryable: Retryability,
    /// The client's current generation, when the code is
    /// [`ErrorCode::StaleState`], so the agent can re-observe precisely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_generation: Option<Generation>,
    /// Session-local action identity when input was injected before failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionId>,
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
            retryable: code.retryability(),
            code,
            message: message.into(),
            path: None,
            expected: None,
            received: None,
            current_generation: None,
            action: None,
            committed: Vec::new(),
        }
    }

    /// Builds an argument failure that can be corrected without parsing its
    /// diagnostic message.
    #[must_use]
    pub fn invalid_argument(
        path: impl Into<String>,
        expected: Expected,
        received: ReceivedKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            path: Some(path.into()),
            expected: Some(Box::new(expected)),
            received: Some(received),
            ..Self::new(ErrorCode::InvalidArgument, message)
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

    /// Adds the identity of input that was already injected.
    #[must_use]
    pub fn with_action(mut self, action: ActionId) -> Self {
        self.action = Some(action);
        self
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
    use super::{ErrorCode, Expected, ExpectedKind, ProtocolError, ReceivedKind, Retryability};
    use crate::ids::{ActionId, Generation};
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
            "{\"code\":\"no_such_client\",\"message\":\"no such client\",\"retryable\":\"after_observation\"}"
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
        let error = ProtocolError::interrupted(vec![Step::Activate, Step::Raise])
            .with_action(ActionId::new(3));
        let encoded = serde_json::to_string(&error).expect("encodes");
        assert!(
            encoded.contains("\"committed\":[\"activate\",\"raise\"]"),
            "{encoded}"
        );
        assert!(encoded.contains("\"action\":3"), "{encoded}");
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

    #[test]
    fn invalid_arguments_carry_a_complete_machine_correction() {
        let error = ProtocolError::invalid_argument(
            "/grid/spacing",
            Expected::integer(Some(50), Some(512)),
            ReceivedKind::String,
            "grid spacing is not usable",
        );
        let value = serde_json::to_value(&error).expect("encodes");
        assert_eq!(value["code"], "invalid_argument");
        assert_eq!(value["path"], "/grid/spacing");
        assert_eq!(value["expected"]["kind"], "integer");
        assert_eq!(value["expected"]["minimum"], 50);
        assert_eq!(value["expected"]["maximum"], 512);
        assert_eq!(value["received"], "string");
        assert_eq!(value["retryable"], "after_correction");
        assert_eq!(
            serde_json::from_value::<ProtocolError>(value).expect("decodes"),
            error
        );
    }

    #[test]
    fn retry_advice_is_code_driven_not_inferred_from_messages() {
        assert_eq!(
            ErrorCode::Interrupted.retryability(),
            Retryability::AfterHumanIdle
        );
        assert_eq!(
            ErrorCode::SessionFrozen.retryability(),
            Retryability::AfterSessionResume
        );
        assert_eq!(
            ErrorCode::Denied.retryability(),
            Retryability::AfterPolicyChange
        );
        assert_eq!(ErrorCode::Unsupported.retryability(), Retryability::Never);
        assert_eq!(
            Expected::kind(ExpectedKind::Boolean).kind,
            ExpectedKind::Boolean
        );
    }
}
