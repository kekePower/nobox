//! Bounded root correlation shared by the disposable helper and its tests.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Internal helper protocol version.
pub const HELPER_VERSION: u8 = 1;
/// Largest accepted helper request.
pub const MAX_INPUT_BYTES: usize = 16 * 1024;
/// Most server-verified target processes.
pub const MAX_TARGET_PIDS: usize = 64;
/// Content and frame rectangles, at most.
pub const MAX_TARGET_RECTS: usize = 2;
/// Most application roots inspected.
pub const MAX_APPLICATIONS: usize = 64;
/// Most direct accessible top-levels inspected.
pub const MAX_TOPLEVELS: usize = 64;

/// One manager-supplied screen-coordinate rectangle.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetRect {
    /// Screen x coordinate.
    pub x: i32,
    /// Screen y coordinate.
    pub y: i32,
    /// Positive width.
    pub width: u16,
    /// Positive height.
    pub height: u16,
}

/// Evidence for one already-authorized X11 client.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRequest {
    /// Internal helper protocol version.
    pub v: u8,
    /// X-Resource server-verified local process family.
    pub pids: Vec<u32>,
    /// Content and frame rectangles in screen coordinates.
    pub rects: Vec<TargetRect>,
    /// Whether the verified process family owns exactly one managed top-level.
    pub single_client: bool,
}

impl DiscoveryRequest {
    /// Checks strict semantic bounds after serde has checked the shape.
    ///
    /// # Errors
    ///
    /// Returns `InvalidRequest` for a wrong version, empty or oversized list,
    /// duplicate/zero PID, duplicate rectangle, or empty rectangle.
    pub fn validate(&self) -> Result<(), InvalidRequest> {
        if self.v != HELPER_VERSION {
            return Err(InvalidRequest);
        }
        if self.pids.is_empty() || self.pids.len() > MAX_TARGET_PIDS {
            return Err(InvalidRequest);
        }
        if self.pids.contains(&0)
            || self.pids.iter().copied().collect::<BTreeSet<_>>().len() != self.pids.len()
        {
            return Err(InvalidRequest);
        }
        if self.rects.is_empty() || self.rects.len() > MAX_TARGET_RECTS {
            return Err(InvalidRequest);
        }
        if self
            .rects
            .iter()
            .any(|rect| rect.width == 0 || rect.height == 0)
            || self.rects.iter().copied().collect::<BTreeSet<_>>().len() != self.rects.len()
        {
            return Err(InvalidRequest);
        }
        Ok(())
    }
}

/// Only the direct roles permitted to represent an application top-level.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TopLevelRole {
    /// Dialog.
    Dialog,
    /// Qt's plain top-level QWidget role.
    Filler,
    /// Ordinary decorated frame.
    Frame,
    /// Undecorated top-level window.
    Window,
}

/// Minimal candidate evidence. No accessible content enters this type.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    /// Application and top-level D-Bus connection PIDs.
    pub pids: Vec<u32>,
    /// Reported screen extents.
    pub rect: TargetRect,
    /// Direct-child top-level role.
    pub role: TopLevelRole,
    /// AT-SPI showing state.
    pub showing: bool,
    /// AT-SPI visible state.
    pub visible: bool,
    /// AT-SPI defunct state.
    pub defunct: bool,
}

impl Candidate {
    fn is_valid(&self) -> bool {
        !self.pids.is_empty()
            && self.pids.len() <= 2
            && self.pids.iter().all(|pid| *pid > 0)
            && self.pids.iter().copied().collect::<BTreeSet<_>>().len() == self.pids.len()
            && self.rect.width > 0
            && self.rect.height > 0
    }
}

/// Privacy-equivalent discovery outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryStatus {
    /// Exactly one root was proven.
    Matched,
    /// More than one eligible root remains.
    Ambiguous,
    /// No safe result, including missing and failed discovery.
    Unavailable,
    /// The manager-to-helper request was invalid.
    Invalid,
}

/// The helper's entire bounded output.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryResponse {
    /// Internal helper protocol version.
    pub v: u8,
    /// Privacy-equivalent result.
    pub status: DiscoveryStatus,
}

/// Marker returned for every invalid internal request or fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidRequest;

/// Correlates already-bounded candidates without using names or traversal order.
#[must_use]
pub fn correlate(
    target: &DiscoveryRequest,
    candidates: &[Candidate],
    complete: bool,
) -> DiscoveryStatus {
    if target.validate().is_err()
        || candidates.len() > MAX_TOPLEVELS
        || candidates.iter().any(|candidate| !candidate.is_valid())
    {
        return DiscoveryStatus::Invalid;
    }
    if !complete {
        return DiscoveryStatus::Unavailable;
    }

    let target_pids = target.pids.iter().copied().collect::<BTreeSet<_>>();
    let eligible = candidates
        .iter()
        .filter(|candidate| {
            candidate.pids.iter().all(|pid| target_pids.contains(pid))
                && candidate.showing
                && candidate.visible
                && !candidate.defunct
        })
        .collect::<Vec<_>>();
    let exact = eligible
        .iter()
        .filter(|candidate| target.rects.contains(&candidate.rect))
        .count();
    if exact == 1 {
        return DiscoveryStatus::Matched;
    }
    if exact > 1 {
        return DiscoveryStatus::Ambiguous;
    }
    if target.single_client && eligible.len() == 1 {
        let candidate = eligible[0];
        if target
            .rects
            .iter()
            .any(|rect| rect.width == candidate.rect.width && rect.height == candidate.rect.height)
        {
            return DiscoveryStatus::Matched;
        }
    }
    if eligible.len() > 1 {
        DiscoveryStatus::Ambiguous
    } else {
        DiscoveryStatus::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Candidate, DiscoveryRequest, DiscoveryStatus, HELPER_VERSION, TargetRect, TopLevelRole,
        correlate,
    };

    const RECT: TargetRect = TargetRect {
        x: 40,
        y: 70,
        width: 900,
        height: 600,
    };

    fn target() -> DiscoveryRequest {
        DiscoveryRequest {
            v: HELPER_VERSION,
            pids: vec![100, 101],
            rects: vec![RECT],
            single_client: false,
        }
    }

    fn candidate() -> Candidate {
        Candidate {
            pids: vec![100],
            rect: RECT,
            role: TopLevelRole::Frame,
            showing: true,
            visible: true,
            defunct: false,
        }
    }

    #[test]
    fn exact_and_process_family_matches_are_unique() {
        assert_eq!(
            correlate(&target(), &[candidate()], true),
            DiscoveryStatus::Matched
        );
        let family = Candidate {
            pids: vec![100, 101],
            ..candidate()
        };
        assert_eq!(
            correlate(&target(), &[family], true),
            DiscoveryStatus::Matched
        );
    }

    #[test]
    fn positionless_mapping_requires_a_complete_bijection() {
        let mut target = target();
        target.single_client = true;
        let stale = Candidate {
            rect: TargetRect { x: 0, y: 0, ..RECT },
            ..candidate()
        };
        assert_eq!(
            correlate(&target, std::slice::from_ref(&stale), true),
            DiscoveryStatus::Matched
        );
        let second = Candidate {
            role: TopLevelRole::Window,
            ..stale.clone()
        };
        assert_eq!(
            correlate(&target, &[stale, second], true),
            DiscoveryStatus::Ambiguous
        );
    }

    #[test]
    fn missing_unrelated_hidden_and_partial_are_equivalent() {
        let unavailable = DiscoveryStatus::Unavailable;
        assert_eq!(correlate(&target(), &[], true), unavailable);
        assert_eq!(
            correlate(
                &target(),
                &[Candidate {
                    pids: vec![999],
                    ..candidate()
                }],
                true
            ),
            unavailable
        );
        assert_eq!(
            correlate(
                &target(),
                &[Candidate {
                    visible: false,
                    ..candidate()
                }],
                true
            ),
            unavailable
        );
        assert_eq!(correlate(&target(), &[candidate()], false), unavailable);
    }

    #[test]
    fn malformed_evidence_is_invalid_not_best_effort() {
        let mut target = target();
        target.pids.push(100);
        assert_eq!(
            correlate(&target, &[candidate()], true),
            DiscoveryStatus::Invalid
        );
        assert!(
            serde_json::from_str::<DiscoveryRequest>(
                r#"{"v":1,"pids":[100],"rects":[],"single_client":true,"title":"secret"}"#
            )
            .is_err()
        );
    }
}
