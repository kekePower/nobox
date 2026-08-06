//! Protocol identities and geometry.
//!
//! Every identity here is a manager-assigned, display-server-neutral handle.
//! X11 window identifiers, atoms, and Wayland object identifiers never appear
//! on the wire, so a compositor backend can implement this protocol unchanged.

use serde::{Deserialize, Serialize};

/// Declares an opaque numeric newtype with transparent serialization.
macro_rules! opaque_id {
    ($(#[$meta:meta])* $name:ident($inner:ty), $doc_new:literal, $doc_raw:literal) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize,
        )]
        #[serde(transparent)]
        pub struct $name($inner);

        impl $name {
            #[doc = $doc_new]
            #[must_use]
            pub const fn new(raw: $inner) -> Self {
                Self(raw)
            }

            #[doc = $doc_raw]
            #[must_use]
            pub const fn raw(self) -> $inner {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "{}", self.0)
            }
        }
    };
}

opaque_id!(
    /// A managed top-level client, stable for that client's lifetime.
    ClientId(u64),
    "Wraps a manager-assigned client identifier.",
    "Returns the underlying identifier."
);

opaque_id!(
    /// A zero-based workspace index.
    WorkspaceId(u32),
    "Wraps a zero-based workspace index.",
    "Returns the zero-based workspace index."
);

opaque_id!(
    /// A connected display output.
    OutputId(u64),
    "Wraps a manager-assigned output identifier.",
    "Returns the underlying identifier."
);

opaque_id!(
    /// A session, issued by the manager at handshake.
    SessionId(u64),
    "Wraps a manager-issued session identifier.",
    "Returns the underlying identifier."
);

opaque_id!(
    /// An agent-chosen request identifier, echoed on the matching response.
    RequestId(u64),
    "Wraps an agent-chosen request identifier.",
    "Returns the underlying identifier."
);

opaque_id!(
    /// A manager-issued, session-local identifier for one successful input injection.
    ActionId(u64),
    "Wraps a manager-issued action identifier.",
    "Returns the session-local action identifier."
);

opaque_id!(
    /// The manager's monotonic sequence number. Every snapshot and every event
    /// is stamped with one, so an agent that snapshots at `N` and applies
    /// `N+1, N+2, …` holds a consistent world model.
    Sequence(u64),
    "Wraps a sequence number.",
    "Returns the sequence number."
);

opaque_id!(
    /// A per-client counter bumped on any descriptor-visible change, so
    /// freshness checks need no global-sequence equality.
    Generation(u64),
    "Wraps a client generation counter.",
    "Returns the generation counter."
);

impl Sequence {
    /// The sequence of a manager that has not yet published anything.
    pub const ZERO: Self = Self::new(0);

    /// Returns the next sequence number, saturating at the numeric bound.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl Generation {
    /// The generation of a freshly published descriptor.
    pub const FIRST: Self = Self::new(1);

    /// Returns the next generation, saturating at the numeric bound.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl ActionId {
    /// The first action issued in a session.
    pub const FIRST: Self = Self::new(1);

    /// Returns the next action identifier, saturating at the numeric bound.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A rectangle in root-window coordinates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    /// Horizontal position.
    pub x: i32,
    /// Vertical position.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Rect {
    /// Builds a rectangle.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns whether a content-relative point lies inside this rectangle's
    /// extent. Input coordinates are content-relative, so only the size
    /// participates.
    #[must_use]
    pub const fn contains_relative(self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as i64) < self.width as i64 && (y as i64) < self.height as i64
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionId, ClientId, Generation, Rect, Sequence};

    #[test]
    fn identities_encode_as_bare_numbers() {
        assert_eq!(
            serde_json::to_string(&ClientId::new(7)).expect("encodes"),
            "7"
        );
        let decoded: ClientId = serde_json::from_str("7").expect("decodes");
        assert_eq!(decoded, ClientId::new(7));
        assert_eq!(ActionId::FIRST.next(), ActionId::new(2));
    }

    #[test]
    fn counters_advance_and_saturate() {
        assert_eq!(Sequence::ZERO.next(), Sequence::new(1));
        assert_eq!(Generation::FIRST.next(), Generation::new(2));
        assert_eq!(Sequence::new(u64::MAX).next(), Sequence::new(u64::MAX));
    }

    #[test]
    fn relative_containment_uses_the_extent_only() {
        let rect = Rect::new(100, 40, 800, 600);
        assert!(rect.contains_relative(0, 0));
        assert!(rect.contains_relative(799, 599));
        assert!(!rect.contains_relative(800, 599));
        assert!(!rect.contains_relative(-1, 0));
    }

    #[test]
    fn unknown_rectangle_fields_are_rejected() {
        let decoded =
            serde_json::from_str::<Rect>("{\"x\":0,\"y\":0,\"width\":1,\"height\":1,\"depth\":24}");
        assert!(decoded.is_err());
    }
}
