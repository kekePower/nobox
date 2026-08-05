//! Capability atoms and the consent bundles presented over them.

use serde::{Deserialize, Serialize};

/// One fine-grained permission. Capabilities are independent: none implies
/// another, and every request is checked against the atoms a session holds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub enum Capability {
    /// Structured desktop state: identities, geometry, stacking, workspaces.
    #[serde(rename = "observe.structure")]
    ObserveStructure,
    /// Window titles in descriptors and events.
    #[serde(rename = "observe.titles")]
    ObserveTitles,
    /// Pixels of a client that is currently visible.
    #[serde(rename = "capture.client_visible")]
    CaptureClientVisible,
    /// Pixels of a client that is obscured or off-screen.
    #[serde(rename = "capture.client_obscured")]
    CaptureClientObscured,
    /// Pixels of a whole output.
    #[serde(rename = "capture.output")]
    CaptureOutput,
    /// Window-addressed pointer injection.
    #[serde(rename = "input.pointer")]
    InputPointer,
    /// Window-addressed keyboard injection.
    #[serde(rename = "input.keyboard")]
    InputKeyboard,
    /// Activation through the manager's focus contract.
    #[serde(rename = "manage.activate")]
    ManageActivate,
    /// Move and resize.
    #[serde(rename = "manage.geometry")]
    ManageGeometry,
    /// State changes such as maximize, fullscreen, or minimize.
    #[serde(rename = "manage.state")]
    ManageState,
    /// Negotiated close through the client's own protocol.
    #[serde(rename = "manage.close")]
    ManageClose,
    /// Workspace switching and per-client workspace assignment.
    #[serde(rename = "manage.workspace")]
    ManageWorkspace,
    /// Launching applications from the desktop-entry catalog.
    #[serde(rename = "launch.desktop")]
    LaunchDesktop,
}

impl Capability {
    /// Every capability atom, in declaration order.
    pub const ALL: [Self; 13] = [
        Self::ObserveStructure,
        Self::ObserveTitles,
        Self::CaptureClientVisible,
        Self::CaptureClientObscured,
        Self::CaptureOutput,
        Self::InputPointer,
        Self::InputKeyboard,
        Self::ManageActivate,
        Self::ManageGeometry,
        Self::ManageState,
        Self::ManageClose,
        Self::ManageWorkspace,
        Self::LaunchDesktop,
    ];

    /// Returns the wire name of this atom.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ObserveStructure => "observe.structure",
            Self::ObserveTitles => "observe.titles",
            Self::CaptureClientVisible => "capture.client_visible",
            Self::CaptureClientObscured => "capture.client_obscured",
            Self::CaptureOutput => "capture.output",
            Self::InputPointer => "input.pointer",
            Self::InputKeyboard => "input.keyboard",
            Self::ManageActivate => "manage.activate",
            Self::ManageGeometry => "manage.geometry",
            Self::ManageState => "manage.state",
            Self::ManageClose => "manage.close",
            Self::ManageWorkspace => "manage.workspace",
            Self::LaunchDesktop => "launch.desktop",
        }
    }

    /// Parses a wire name, returning `None` for anything unknown.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|atom| atom.as_str() == name)
    }

    /// Returns the bundle this atom is presented under during consent.
    #[must_use]
    pub const fn bundle(self) -> Bundle {
        match self {
            Self::ObserveStructure | Self::ObserveTitles => Bundle::Observe,
            Self::CaptureClientVisible | Self::CaptureClientObscured | Self::CaptureOutput => {
                Bundle::Capture
            }
            Self::InputPointer | Self::InputKeyboard => Bundle::Input,
            Self::ManageActivate
            | Self::ManageGeometry
            | Self::ManageState
            | Self::ManageClose
            | Self::ManageWorkspace => Bundle::Manage,
            Self::LaunchDesktop => Bundle::Launch,
        }
    }
}

/// A consent-presentation grouping over capability atoms. Bundles exist for
/// human-readable consent only; grants always record atoms, so narrowing a
/// bundle later needs no protocol change.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bundle {
    /// Structured desktop state and events.
    Observe,
    /// Pixel access.
    Capture,
    /// Synthesized, window-addressed input.
    Input,
    /// Activation, geometry, state, workspace, and close.
    Manage,
    /// Starting approved installed applications.
    Launch,
}

impl Bundle {
    /// Every bundle, in escalating sensitivity order.
    pub const ALL: [Self; 5] = [
        Self::Observe,
        Self::Capture,
        Self::Input,
        Self::Manage,
        Self::Launch,
    ];

    /// Returns the wire name of this bundle.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Capture => "capture",
            Self::Input => "input",
            Self::Manage => "manage",
            Self::Launch => "launch",
        }
    }

    /// Parses a wire name, returning `None` for anything unknown.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|bundle| bundle.as_str() == name)
    }

    /// Returns the atoms this bundle expands to.
    #[must_use]
    pub const fn atoms(self) -> &'static [Capability] {
        match self {
            Self::Observe => &[Capability::ObserveStructure, Capability::ObserveTitles],
            Self::Capture => &[
                Capability::CaptureClientVisible,
                Capability::CaptureClientObscured,
                Capability::CaptureOutput,
            ],
            Self::Input => &[Capability::InputPointer, Capability::InputKeyboard],
            Self::Manage => &[
                Capability::ManageActivate,
                Capability::ManageGeometry,
                Capability::ManageState,
                Capability::ManageClose,
                Capability::ManageWorkspace,
            ],
            Self::Launch => &[Capability::LaunchDesktop],
        }
    }
}

/// An unordered set of capability atoms, cheap to copy and to intersect.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct CapabilitySet {
    bits: u16,
}

impl CapabilitySet {
    /// An empty set: the deny-by-default grant.
    pub const EMPTY: Self = Self { bits: 0 };

    const fn bit(capability: Capability) -> u16 {
        1 << (capability as u16)
    }

    /// Builds a set from an iterator of atoms.
    #[must_use]
    pub fn from_iter_atoms(atoms: impl IntoIterator<Item = Capability>) -> Self {
        let mut set = Self::EMPTY;
        for atom in atoms {
            set = set.with(atom);
        }
        set
    }

    /// Returns this set plus `capability`.
    #[must_use]
    pub const fn with(self, capability: Capability) -> Self {
        Self {
            bits: self.bits | Self::bit(capability),
        }
    }

    /// Returns this set minus `capability`.
    #[must_use]
    pub const fn without(self, capability: Capability) -> Self {
        Self {
            bits: self.bits & !Self::bit(capability),
        }
    }

    /// Returns whether `capability` is held.
    #[must_use]
    pub const fn holds(self, capability: Capability) -> bool {
        self.bits & Self::bit(capability) != 0
    }

    /// Returns whether no capability is held.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// Returns the union of two sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    /// Returns the intersection of two sets.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }

    /// Returns the held atoms in declaration order.
    #[must_use]
    pub fn atoms(self) -> Vec<Capability> {
        Capability::ALL
            .into_iter()
            .filter(|atom| self.holds(*atom))
            .collect()
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        Self::from_iter_atoms(iter)
    }
}

impl Serialize for CapabilitySet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.atoms().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CapabilitySet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_iter_atoms(Vec::<Capability>::deserialize(
            deserializer,
        )?))
    }
}

#[cfg(test)]
mod tests {
    use super::{Bundle, Capability, CapabilitySet};

    #[test]
    fn every_atom_belongs_to_exactly_one_bundle() {
        for atom in Capability::ALL {
            let owners = Bundle::ALL
                .into_iter()
                .filter(|bundle| bundle.atoms().contains(&atom))
                .count();
            assert_eq!(owners, 1, "{} is in {owners} bundles", atom.as_str());
            assert!(atom.bundle().atoms().contains(&atom));
        }
    }

    #[test]
    fn capability_names_round_trip_through_json() {
        for atom in Capability::ALL {
            let encoded = serde_json::to_string(&atom).expect("atom encodes");
            assert_eq!(encoded, format!("\"{}\"", atom.as_str()));
            let decoded: Capability = serde_json::from_str(&encoded).expect("atom decodes");
            assert_eq!(decoded, atom);
        }
    }

    #[test]
    fn unknown_capability_names_are_rejected() {
        let decoded = serde_json::from_str::<Capability>("\"observe.everything\"");
        assert!(decoded.is_err());
    }

    #[test]
    fn sets_hold_only_what_was_added() {
        let set = CapabilitySet::from_iter_atoms([
            Capability::ObserveStructure,
            Capability::ManageActivate,
        ]);
        assert!(set.holds(Capability::ObserveStructure));
        assert!(set.holds(Capability::ManageActivate));
        assert!(!set.holds(Capability::ObserveTitles));
        assert!(!set.holds(Capability::ManageGeometry));
        assert_eq!(
            set.atoms(),
            vec![Capability::ObserveStructure, Capability::ManageActivate]
        );
        assert!(
            set.without(Capability::ObserveStructure)
                .holds(Capability::ManageActivate)
        );
        assert!(CapabilitySet::EMPTY.is_empty());
    }

    #[test]
    fn bundles_expand_to_disjoint_atom_groups() {
        let observe = CapabilitySet::from_iter_atoms(Bundle::Observe.atoms().iter().copied());
        let manage = CapabilitySet::from_iter_atoms(Bundle::Manage.atoms().iter().copied());
        assert!(observe.intersection(manage).is_empty());
        assert!(!observe.union(manage).is_empty());
    }

    #[test]
    fn capability_sets_round_trip_as_atom_lists() {
        let set = CapabilitySet::from_iter_atoms([
            Capability::InputPointer,
            Capability::ObserveStructure,
        ]);
        let encoded = serde_json::to_string(&set).expect("set encodes");
        assert_eq!(encoded, "[\"observe.structure\",\"input.pointer\"]");
        let decoded: CapabilitySet = serde_json::from_str(&encoded).expect("set decodes");
        assert_eq!(decoded, set);
    }
}
