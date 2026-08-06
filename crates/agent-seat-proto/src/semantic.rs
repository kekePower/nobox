//! Bounded, protocol-neutral semantic projections of one client.
//!
//! These types expose the useful shape of an accessibility tree without
//! exposing an accessibility backend, D-Bus address, X11 identifier, or
//! toolkit-specific object path. Handles are session-local and valid only for
//! the tree generation that issued them.

use serde::{Deserialize, Serialize};

use crate::{ClientId, Generation, Rect};

/// Most semantic nodes one response may contain.
pub const MAX_SEMANTIC_NODES: u16 = 128;
/// Most nodes a helper may inspect for one bounded projection or search.
pub const MAX_SEMANTIC_SCAN_NODES: u16 = 4_096;
/// Greatest subtree depth one request may traverse.
pub const MAX_SEMANTIC_DEPTH: u8 = 16;
/// Most roles or states one search predicate may name.
pub const MAX_SEMANTIC_FILTER_ITEMS: usize = 16;
/// Longest search text accepted on the wire.
pub const MAX_SEMANTIC_QUERY_LEN: usize = 256;
/// Longest exposed accessible name or description.
pub const MAX_SEMANTIC_NAME_LEN: usize = 512;
/// Longest exposed non-secret value.
pub const MAX_SEMANTIC_VALUE_LEN: usize = 512;
/// Most relationships exposed for one node.
pub const MAX_SEMANTIC_RELATIONS: usize = 16;
/// Most targets exposed for one relationship.
pub const MAX_SEMANTIC_RELATION_TARGETS: usize = 16;

macro_rules! semantic_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize,
            Serialize,
        )]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Wraps a manager-issued opaque value.
            #[must_use]
            pub const fn new(raw: u64) -> Self {
                Self(raw)
            }

            /// Returns the opaque numeric representation.
            #[must_use]
            pub const fn raw(self) -> u64 {
                self.0
            }
        }
    };
}

semantic_id!(
    /// A monotonic version of one client's semantic tree.
    TreeGeneration
);
semantic_id!(
    /// A node identity whose meaning is scoped to a tree generation.
    SemanticNodeId
);
semantic_id!(
    /// An opaque cursor for the next deterministic page.
    SemanticContinuation
);

impl TreeGeneration {
    /// First successfully observed tree generation.
    pub const FIRST: Self = Self::new(1);

    /// Returns the next generation, saturating at the numeric bound.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// A node handle that cannot accidentally be used against another tree.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticNodeHandle {
    /// Tree version that issued the node identity.
    pub tree: TreeGeneration,
    /// Opaque identity inside that version.
    pub node: SemanticNodeId,
}

/// Portable semantic roles. Backend-only roles collapse to `unknown`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRole {
    /// Top-level application container.
    Application,
    /// Top-level window.
    Window,
    /// Dialog window.
    Dialog,
    /// Document content.
    Document,
    /// Heading.
    Heading,
    /// Paragraph.
    Paragraph,
    /// Navigable link.
    Link,
    /// Push button.
    Button,
    /// Check box.
    CheckBox,
    /// Radio button.
    RadioButton,
    /// Combo box.
    ComboBox,
    /// Static text.
    Text,
    /// Editable text entry.
    Entry,
    /// List container.
    List,
    /// List item.
    ListItem,
    /// Table or grid.
    Table,
    /// Table or grid cell.
    Cell,
    /// Image.
    Image,
    /// Video content.
    Video,
    /// Audio content.
    Audio,
    /// Menu container.
    Menu,
    /// Menu item.
    MenuItem,
    /// Tab.
    Tab,
    /// Tab list.
    TabList,
    /// Toolbar.
    Toolbar,
    /// Status indicator.
    Status,
    /// Slider.
    Slider,
    /// Numeric spin button.
    SpinButton,
    /// Progress indicator.
    Progress,
    /// Scroll bar.
    ScrollBar,
    /// Visual separator.
    Separator,
    /// Tooltip.
    Tooltip,
    /// Generic group.
    Group,
    /// Generic section.
    Section,
    /// Form container.
    Form,
    /// Navigational landmark.
    Landmark,
    /// Backend role with no portable mapping.
    Unknown,
}

impl SemanticRole {
    /// Every role accepted by search filters.
    pub const ALL: [Self; 37] = [
        Self::Application,
        Self::Window,
        Self::Dialog,
        Self::Document,
        Self::Heading,
        Self::Paragraph,
        Self::Link,
        Self::Button,
        Self::CheckBox,
        Self::RadioButton,
        Self::ComboBox,
        Self::Text,
        Self::Entry,
        Self::List,
        Self::ListItem,
        Self::Table,
        Self::Cell,
        Self::Image,
        Self::Video,
        Self::Audio,
        Self::Menu,
        Self::MenuItem,
        Self::Tab,
        Self::TabList,
        Self::Toolbar,
        Self::Status,
        Self::Slider,
        Self::SpinButton,
        Self::Progress,
        Self::ScrollBar,
        Self::Separator,
        Self::Tooltip,
        Self::Group,
        Self::Section,
        Self::Form,
        Self::Landmark,
        Self::Unknown,
    ];
}

/// Stable, useful state facts. Raw toolkit state bitsets never cross the wire.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticState {
    /// Currently active.
    Active,
    /// Work is in progress.
    Busy,
    /// Checked.
    Checked,
    /// Collapsed.
    Collapsed,
    /// Not enabled.
    Disabled,
    /// Editable.
    Editable,
    /// Expanded.
    Expanded,
    /// Can receive focus.
    Focusable,
    /// Currently focused.
    Focused,
    /// Value is invalid.
    Invalid,
    /// Modal.
    Modal,
    /// Accepts multiple text lines.
    Multiline,
    /// Outside the visible viewport.
    Offscreen,
    /// Pressed.
    Pressed,
    /// Contains protected or secret text.
    Protected,
    /// Read-only.
    ReadOnly,
    /// Required.
    Required,
    /// Selected.
    Selected,
    /// Can be selected.
    Selectable,
    /// Reported visible by the backend.
    Visible,
}

impl SemanticState {
    /// Every state accepted by search filters.
    pub const ALL: [Self; 20] = [
        Self::Active,
        Self::Busy,
        Self::Checked,
        Self::Collapsed,
        Self::Disabled,
        Self::Editable,
        Self::Expanded,
        Self::Focusable,
        Self::Focused,
        Self::Invalid,
        Self::Modal,
        Self::Multiline,
        Self::Offscreen,
        Self::Pressed,
        Self::Protected,
        Self::ReadOnly,
        Self::Required,
        Self::Selected,
        Self::Selectable,
        Self::Visible,
    ];
}

/// A normalized semantic relationship.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRelationKind {
    /// Target labels this node.
    LabelledBy,
    /// Target describes this node.
    DescribedBy,
    /// This node controls the target.
    ControllerFor,
    /// Target controls this node.
    ControlledBy,
    /// This node belongs to the target group.
    MemberOf,
    /// Content flows to the target.
    FlowsTo,
    /// Content flows from the target.
    FlowsFrom,
    /// This node reports an error for the target.
    ErrorFor,
    /// Target contains this node's error message.
    ErrorMessage,
}

/// One bounded relationship edge list.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRelation {
    /// Relationship kind.
    pub kind: SemanticRelationKind,
    /// Bounded targets in stable order.
    pub targets: Vec<SemanticNodeHandle>,
}

/// One node in a bounded semantic projection.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticNode {
    /// Opaque generation-scoped identity.
    pub handle: SemanticNodeHandle,
    /// Parent when it is part of this tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SemanticNodeHandle>,
    /// Distance below the requested subtree root.
    pub depth: u8,
    /// Portable role.
    pub role: SemanticRole,
    /// Bounded accessible name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Bounded accessible description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A non-secret value. Always absent for protected nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Stable state facts in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<SemanticState>,
    /// Bounds relative to the client's content origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
    /// Backend-reported number of direct children.
    pub child_count: u32,
    /// Bounded normalized relationships.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<SemanticRelation>,
}

/// A deterministic page of a client's semantic tree.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticTreePage {
    /// Target client.
    pub client: ClientId,
    /// Window-manager descriptor generation used for correlation.
    pub generation: Generation,
    /// Tree version used by every handle in this page.
    pub tree_generation: TreeGeneration,
    /// Requested subtree root.
    pub root: SemanticNodeHandle,
    /// Nodes in stable breadth-first order.
    pub nodes: Vec<SemanticNode>,
    /// Cursor for another page, absent when traversal is complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<SemanticContinuation>,
}

/// Constrained search over role, accessible name, and state.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SemanticQuery {
    /// Case-insensitive substring of the accessible name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Empty means any role.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<SemanticRole>,
    /// Every named state must be present.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<SemanticState>,
}

/// A deterministic page of constrained search matches.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticSearchPage {
    /// Target client.
    pub client: ClientId,
    /// Window-manager descriptor generation used for correlation.
    pub generation: Generation,
    /// Tree version used by every handle in this page.
    pub tree_generation: TreeGeneration,
    /// Matching nodes in stable breadth-first tree order.
    pub matches: Vec<SemanticNode>,
    /// Cursor for another page, absent when search is complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<SemanticContinuation>,
}

#[cfg(test)]
mod tests {
    use super::{SemanticNodeHandle, SemanticNodeId, SemanticQuery, TreeGeneration};

    #[test]
    fn handles_carry_the_tree_generation() {
        let handle = SemanticNodeHandle {
            tree: TreeGeneration::new(4),
            node: SemanticNodeId::new(9),
        };
        let encoded = serde_json::to_string(&handle).expect("encodes");
        assert_eq!(encoded, r#"{"tree":4,"node":9}"#);
        assert_eq!(
            serde_json::from_str::<SemanticNodeHandle>(&encoded).expect("decodes"),
            handle
        );
    }

    #[test]
    fn query_is_strict_and_compact() {
        assert_eq!(
            serde_json::to_string(&SemanticQuery::default()).expect("encodes"),
            "{}"
        );
        assert!(serde_json::from_str::<SemanticQuery>(r#"{"anything":true}"#).is_err());
    }
}
