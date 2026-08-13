//! Protocol-neutral window-management state.

pub mod agent;

use std::collections::{BTreeMap, BTreeSet};

/// An opaque identifier assigned by a display-server backend.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ClientId(u64);

impl ClientId {
    /// Wraps a backend identifier.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the backend identifier.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// A zero-based policy workspace identifier.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceId(u32);

impl WorkspaceId {
    /// Creates a workspace identifier from a zero-based index.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Returns the zero-based workspace index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// An opaque output identifier assigned by a display-server backend.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OutputId(u64);

impl OutputId {
    /// Wraps a backend identifier.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the backend identifier.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// One connected display output and its root-coordinate geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Output {
    /// Backend-owned stable identifier.
    pub id: OutputId,
    /// Complete output rectangle before panels reserve space.
    pub geometry: Geometry,
    /// Whether the backend considers this the primary output.
    pub primary: bool,
}

/// Output associated with exact display-area coverage outside managed fullscreen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputCoverage(OutputId);

impl OutputCoverage {
    /// Records the output selected for an exactly covering client.
    #[must_use]
    pub const fn new(output: OutputId) -> Self {
        Self(output)
    }

    /// Returns the associated output.
    #[must_use]
    pub const fn output(self) -> OutputId {
        self.0
    }
}

/// Current output topology with deterministic client-to-output selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputSet {
    outputs: Vec<Output>,
}

impl OutputSet {
    /// Builds a topology, discarding duplicate identifiers and normalizing its
    /// primary output. An empty topology becomes a safe one-pixel fallback.
    #[must_use]
    pub fn new(outputs: impl IntoIterator<Item = Output>) -> Self {
        let mut seen = BTreeSet::new();
        let mut outputs = outputs
            .into_iter()
            .filter(|output| seen.insert(output.id))
            .collect::<Vec<_>>();
        if outputs.is_empty() {
            outputs.push(Output {
                id: OutputId::new(0),
                geometry: Geometry::new(0, 0, 1, 1),
                primary: true,
            });
        }
        let primary = outputs
            .iter()
            .position(|output| output.primary)
            .unwrap_or(0);
        for (index, output) in outputs.iter_mut().enumerate() {
            output.primary = index == primary;
        }
        Self { outputs }
    }

    /// Returns every output in backend discovery order.
    #[must_use]
    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    /// Returns the normalized primary output.
    #[must_use]
    pub fn primary(&self) -> Output {
        self.outputs
            .iter()
            .copied()
            .find(|output| output.primary)
            .unwrap_or(self.outputs[0])
    }

    /// Selects the output containing the largest part of a rectangle.
    ///
    /// Rectangles outside every output use the nearest output. Equal choices
    /// prefer the primary output and then backend discovery order.
    #[must_use]
    pub fn output_for(&self, geometry: Geometry) -> Output {
        let mut selected = self.outputs[0];
        let mut selected_overlap = intersection_area(selected.geometry, geometry);
        let mut selected_distance = rectangle_distance_squared(selected.geometry, geometry);
        for output in self.outputs.iter().copied().skip(1) {
            let overlap = intersection_area(output.geometry, geometry);
            let distance = rectangle_distance_squared(output.geometry, geometry);
            let better = overlap > selected_overlap
                || (overlap == selected_overlap
                    && ((overlap == 0 && distance < selected_distance)
                        || (distance == selected_distance && output.primary && !selected.primary)));
            if better {
                selected = output;
                selected_overlap = overlap;
                selected_distance = distance;
            }
        }
        selected
    }

    /// Returns the best output only when some part of the rectangle is on it.
    #[must_use]
    pub fn overlapping_output(&self, geometry: Geometry) -> Option<Output> {
        let output = self.output_for(geometry);
        (intersection_area(output.geometry, geometry) > 0).then_some(output)
    }
}

impl Default for OutputSet {
    fn default() -> Self {
        Self::new([])
    }
}

/// Relative direction through the configured workspace ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceDirection {
    /// Move toward lower indexes, wrapping from the first to the last.
    Previous,
    /// Move toward higher indexes, wrapping from the last to the first.
    Next,
    /// Move to the neighboring grid cell on the left.
    Left,
    /// Move to the neighboring grid cell on the right.
    Right,
    /// Move to the neighboring grid cell above.
    Up,
    /// Move to the neighboring grid cell below.
    Down,
}

/// Cardinal direction for window geometry operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CardinalDirection {
    /// Toward smaller horizontal coordinates.
    Left,
    /// Toward larger horizontal coordinates.
    Right,
    /// Toward smaller vertical coordinates.
    Up,
    /// Toward larger vertical coordinates.
    Down,
}

/// Eight-way direction for selecting a spatial window target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpatialDirection {
    /// Toward smaller horizontal coordinates.
    Left,
    /// Toward larger horizontal coordinates.
    Right,
    /// Toward smaller vertical coordinates.
    Up,
    /// Toward larger vertical coordinates.
    Down,
    /// Diagonally toward smaller horizontal and vertical coordinates.
    UpLeft,
    /// Diagonally toward larger horizontal and smaller vertical coordinates.
    UpRight,
    /// Diagonally toward smaller horizontal and larger vertical coordinates.
    DownLeft,
    /// Diagonally toward larger horizontal and vertical coordinates.
    DownRight,
}

/// Whether directional growth may advance through an edge it already touches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockingEdgePolicy {
    /// Keep a side fixed while it touches a candidate edge.
    Stop,
    /// Advance to the next edge beyond a currently touching candidate.
    Cross,
}

/// Placement of one window axis within target bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AxisPlacement {
    /// Preserve the offset the window had within its source bounds.
    Keep,
    /// Place this many pixels from the target's starting edge.
    Start(i32),
    /// Center the window on this axis.
    Center,
    /// Place this many pixels inward from the target's ending edge.
    End(i32),
}

/// Primary ordering used to place workspace indexes into a rectangular grid.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkspaceOrientation {
    /// Fill each row before advancing to the next row.
    #[default]
    Horizontal,
    /// Fill each column before advancing to the next column.
    Vertical,
}

/// Grid corner containing workspace zero.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkspaceCorner {
    /// Top-left corner.
    #[default]
    TopLeft,
    /// Top-right corner.
    TopRight,
    /// Bottom-right corner.
    BottomRight,
    /// Bottom-left corner.
    BottomLeft,
}

/// Validated rectangular arrangement of policy workspaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceLayout {
    count: u32,
    columns: u32,
    rows: u32,
    orientation: WorkspaceOrientation,
    corner: WorkspaceCorner,
}

impl WorkspaceLayout {
    /// Builds a safe layout, deriving one zero-valued dimension from `count`.
    ///
    /// Returns `None` when both dimensions are zero. Oversized dimensions are
    /// bounded by the workspace count so hostile external hints cannot make
    /// navigation perform unbounded work.
    #[must_use]
    pub fn new(
        count: u32,
        columns: u32,
        rows: u32,
        orientation: WorkspaceOrientation,
        corner: WorkspaceCorner,
    ) -> Option<Self> {
        let count = count.max(1);
        if columns == 0 && rows == 0 {
            return None;
        }
        let (columns, rows) = match (columns, rows) {
            (0, rows) => (count.div_ceil(rows.max(1)), rows),
            (columns, 0) => (columns, count.div_ceil(columns.max(1))),
            dimensions => dimensions,
        };
        Some(Self {
            count,
            columns: columns.clamp(1, count),
            rows: rows.clamp(1, count),
            orientation,
            corner,
        })
    }

    /// Creates the default one-row layout.
    #[must_use]
    pub fn one_row(count: u32) -> Self {
        let count = count.max(1);
        Self {
            count,
            columns: count,
            rows: 1,
            orientation: WorkspaceOrientation::Horizontal,
            corner: WorkspaceCorner::TopLeft,
        }
    }

    /// Returns the number of grid columns.
    #[must_use]
    pub const fn columns(self) -> u32 {
        self.columns
    }

    /// Returns the number of grid rows.
    #[must_use]
    pub const fn rows(self) -> u32 {
        self.rows
    }

    /// Finds a directional neighbor, optionally wrapping within its row or column.
    #[must_use]
    pub fn neighbor(
        self,
        workspace: WorkspaceId,
        direction: WorkspaceDirection,
        wrap: bool,
    ) -> WorkspaceId {
        if workspace.index() >= self.count
            || matches!(
                direction,
                WorkspaceDirection::Previous | WorkspaceDirection::Next
            )
        {
            return workspace;
        }
        let Some((column, row)) = self.coordinate(workspace) else {
            return workspace;
        };
        let limit = match direction {
            WorkspaceDirection::Left | WorkspaceDirection::Right => self.columns,
            WorkspaceDirection::Up | WorkspaceDirection::Down => self.rows,
            WorkspaceDirection::Previous | WorkspaceDirection::Next => 0,
        };
        let mut candidate_column = column;
        let mut candidate_row = row;
        for _ in 0..limit {
            match direction {
                WorkspaceDirection::Left if candidate_column > 0 => candidate_column -= 1,
                WorkspaceDirection::Left if wrap => candidate_column = self.columns - 1,
                WorkspaceDirection::Right if candidate_column + 1 < self.columns => {
                    candidate_column += 1;
                }
                WorkspaceDirection::Right if wrap => candidate_column = 0,
                WorkspaceDirection::Up if candidate_row > 0 => candidate_row -= 1,
                WorkspaceDirection::Up if wrap => candidate_row = self.rows - 1,
                WorkspaceDirection::Down if candidate_row + 1 < self.rows => candidate_row += 1,
                WorkspaceDirection::Down if wrap => candidate_row = 0,
                _ => return workspace,
            }
            if let Some(candidate) = self.workspace_at(candidate_column, candidate_row) {
                return candidate;
            }
            if !wrap {
                return workspace;
            }
        }
        workspace
    }

    fn coordinate(self, workspace: WorkspaceId) -> Option<(u32, u32)> {
        let index = workspace.index();
        if index >= self.count {
            return None;
        }
        let (mut column, mut row) = match self.orientation {
            WorkspaceOrientation::Horizontal => (index % self.columns, index / self.columns),
            WorkspaceOrientation::Vertical => (index / self.rows, index % self.rows),
        };
        if row >= self.rows || column >= self.columns {
            return None;
        }
        if matches!(
            self.corner,
            WorkspaceCorner::TopRight | WorkspaceCorner::BottomRight
        ) {
            column = self.columns - 1 - column;
        }
        if matches!(
            self.corner,
            WorkspaceCorner::BottomRight | WorkspaceCorner::BottomLeft
        ) {
            row = self.rows - 1 - row;
        }
        Some((column, row))
    }

    fn workspace_at(self, mut column: u32, mut row: u32) -> Option<WorkspaceId> {
        if column >= self.columns || row >= self.rows {
            return None;
        }
        if matches!(
            self.corner,
            WorkspaceCorner::TopRight | WorkspaceCorner::BottomRight
        ) {
            column = self.columns - 1 - column;
        }
        if matches!(
            self.corner,
            WorkspaceCorner::BottomRight | WorkspaceCorner::BottomLeft
        ) {
            row = self.rows - 1 - row;
        }
        let index = match self.orientation {
            WorkspaceOrientation::Horizontal => row
                .checked_mul(self.columns)
                .and_then(|base| base.checked_add(column))?,
            WorkspaceOrientation::Vertical => column
                .checked_mul(self.rows)
                .and_then(|base| base.checked_add(row))?,
        };
        (index < self.count).then_some(WorkspaceId::new(index))
    }
}

/// Workspaces on which a client is visible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceAssignment {
    /// The client belongs to one workspace.
    Workspace(WorkspaceId),
    /// The client is sticky and visible on every workspace.
    All,
}

impl Default for WorkspaceAssignment {
    fn default() -> Self {
        Self::Workspace(WorkspaceId::default())
    }
}

impl WorkspaceAssignment {
    /// Returns whether the assignment is visible on `workspace`.
    #[must_use]
    pub const fn is_visible_on(self, workspace: WorkspaceId) -> bool {
        match self {
            Self::Workspace(assigned) => assigned.0 == workspace.0,
            Self::All => true,
        }
    }
}

/// Policy-level target of a transient relationship.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransientTarget {
    /// A specific managed client.
    Client(ClientId),
    /// Every client sharing the transient's application group.
    Group,
}

/// Functional role of a top-level client, independent of display protocol.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ClientRole {
    /// A regular application window.
    #[default]
    Normal,
    /// A dialog associated with another window or application.
    Dialog,
    /// A compact utility window.
    Utility,
    /// A detachable application toolbar.
    Toolbar,
    /// A detachable application menu.
    Menu,
    /// A transient splash screen.
    Splash,
    /// A desktop surface that is not manipulated like an application.
    Desktop,
    /// A panel or dock surface.
    Dock,
    /// A drop-down menu surface.
    DropdownMenu,
    /// A pop-up menu surface.
    PopupMenu,
    /// A tooltip surface.
    Tooltip,
    /// A notification surface.
    Notification,
    /// A combo-box pop-up surface.
    Combo,
    /// A drag-and-drop feedback surface.
    DragAndDrop,
}

/// User-requested stacking preference independent of display protocol.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ClientLayer {
    /// Keep the client below ordinary application windows.
    Below,
    /// Use the default layer selected from the client's role.
    #[default]
    Normal,
    /// Keep the client above ordinary application windows and docks.
    Above,
}

/// Effective policy stacking layer, ordered from bottom to top.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum StackingLayer {
    /// Desktop background surfaces.
    Desktop,
    /// Clients explicitly kept below ordinary windows.
    Below,
    /// Ordinary application windows.
    Normal,
    /// Panels and docks.
    Dock,
    /// Clients explicitly kept above ordinary windows and docks.
    Above,
    /// Fullscreen clients.
    Fullscreen,
}

/// Adaptive restack operation selected from visible overlap.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RestackDecision {
    /// Keep the current stacking position because no peer obscures either side.
    #[default]
    Unchanged,
    /// Raise the target because an overlapping peer is above it.
    Raise,
    /// Lower the target because it covers an overlapping peer below it.
    Lower,
}

/// Operations the policy engine permits for a client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientCapabilities {
    /// Whether the client can receive keyboard focus.
    pub focusable: bool,
    /// Whether the client can be moved interactively.
    pub movable: bool,
    /// Whether the client can be resized interactively.
    pub resizable: bool,
    /// Whether the client can be minimized.
    pub minimizable: bool,
    /// Whether the client can be maximized.
    pub maximizable: bool,
    /// Whether the client can cover an output without decorations.
    pub fullscreenable: bool,
    /// Whether the client can be closed by the window manager.
    pub closable: bool,
}

/// User operations currently exposed for a managed client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientOperations {
    /// Move the client interactively.
    pub movable: bool,
    /// Resize the client interactively.
    pub resizable: bool,
    /// Minimize the client.
    pub minimizable: bool,
    /// Collapse the client to its titlebar.
    pub shadeable: bool,
    /// Maximize either client axis.
    pub maximizable: bool,
    /// Enter or leave fullscreen.
    pub fullscreenable: bool,
    /// Enable or disable server-side decorations.
    pub decoratable: bool,
    /// Move the client between workspaces.
    pub workspace_movable: bool,
    /// Close the client.
    pub closable: bool,
    /// Place the client above its normal layer.
    pub above: bool,
    /// Place the client below its normal layer.
    pub below: bool,
}

/// Server-side decoration elements selected for a client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientDecorations {
    /// Draw an outer border.
    pub border: bool,
    /// Draw a titlebar.
    pub titlebar: bool,
    /// Show a minimize control in the titlebar.
    pub minimize: bool,
    /// Show a maximize control in the titlebar.
    pub maximize: bool,
    /// Show a close control in the titlebar.
    pub close: bool,
}

impl ClientDecorations {
    /// A policy with no server-side decoration elements.
    pub const NONE: Self = Self {
        border: false,
        titlebar: false,
        minimize: false,
        maximize: false,
        close: false,
    };

    /// Returns whether any server-side decoration element is enabled.
    #[must_use]
    pub const fn is_present(self) -> bool {
        self.border || self.titlebar
    }

    /// Resolves visible decoration space against theme dimensions.
    #[must_use]
    pub const fn extents(self, border_width: u32, titlebar_height: u32) -> DecorationExtents {
        let border = if self.border { border_width } else { 0 };
        let titlebar = if self.titlebar { titlebar_height } else { 0 };
        DecorationExtents::new(border, border, border.saturating_add(titlebar), border)
    }
}

/// User preference layered over a client's natural decoration policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DecorationOverride {
    /// Follow client hints and configured application policy.
    #[default]
    Default,
    /// Force the role's standard server-side decorations.
    Decorated,
    /// Suppress all server-side decorations.
    Undecorated,
}

/// Protocol-neutral behavior selected for a client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientPolicy {
    /// Functional role used to select behavior.
    pub role: ClientRole,
    /// Operations exposed to users.
    pub capabilities: ClientCapabilities,
    /// Server-side decoration elements.
    pub decorations: ClientDecorations,
}

/// Backend-neutral presentation hints attached to a managed client.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClientPresentation {
    /// Omit the client from task-oriented window lists and focus cycling.
    pub skip_taskbar: bool,
    /// Omit the client from workspace pagers.
    pub skip_pager: bool,
    /// Draw the user's attention to the client.
    pub urgent: bool,
}

/// Active maximize axes and the geometry restored when they are cleared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaximizeState {
    /// Whether the horizontal axis fills the available area.
    pub horizontal: bool,
    /// Whether the vertical axis fills the available area.
    pub vertical: bool,
    restore: Geometry,
}

/// Active fullscreen state and the geometry restored when it is cleared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FullscreenState {
    restore: Geometry,
}

impl ClientPolicy {
    /// Returns the default policy for a functional client role.
    #[must_use]
    pub const fn for_role(role: ClientRole) -> Self {
        let standard_capabilities = ClientCapabilities {
            focusable: true,
            movable: true,
            resizable: true,
            minimizable: true,
            maximizable: true,
            fullscreenable: true,
            closable: true,
        };
        let standard_decorations = ClientDecorations {
            border: true,
            titlebar: true,
            minimize: true,
            maximize: true,
            close: true,
        };
        match role {
            ClientRole::Normal | ClientRole::Dialog | ClientRole::Utility => Self {
                role,
                capabilities: standard_capabilities,
                decorations: standard_decorations,
            },
            ClientRole::Toolbar | ClientRole::Menu => Self {
                role,
                capabilities: ClientCapabilities {
                    minimizable: false,
                    maximizable: false,
                    fullscreenable: false,
                    ..standard_capabilities
                },
                decorations: ClientDecorations {
                    minimize: false,
                    maximize: false,
                    ..standard_decorations
                },
            },
            ClientRole::Splash => Self {
                role,
                capabilities: ClientCapabilities {
                    focusable: false,
                    movable: true,
                    resizable: false,
                    minimizable: false,
                    maximizable: false,
                    fullscreenable: false,
                    closable: false,
                },
                decorations: ClientDecorations {
                    border: false,
                    titlebar: false,
                    minimize: false,
                    maximize: false,
                    close: false,
                },
            },
            ClientRole::Desktop
            | ClientRole::Dock
            | ClientRole::DropdownMenu
            | ClientRole::PopupMenu
            | ClientRole::Tooltip
            | ClientRole::Notification
            | ClientRole::Combo
            | ClientRole::DragAndDrop => Self {
                role,
                capabilities: ClientCapabilities {
                    focusable: false,
                    movable: false,
                    resizable: false,
                    minimizable: false,
                    maximizable: false,
                    fullscreenable: false,
                    closable: false,
                },
                decorations: ClientDecorations {
                    border: false,
                    titlebar: false,
                    minimize: false,
                    maximize: false,
                    close: false,
                },
            },
        }
    }

    /// Applies a user decoration preference while retaining this policy as the
    /// natural state restored when the preference is cleared.
    #[must_use]
    pub const fn with_decoration_override(self, preference: DecorationOverride) -> Self {
        policy_with_decoration_override(self, self.decorations, preference)
    }
}

const fn policy_with_decoration_override(
    mut policy: ClientPolicy,
    natural: ClientDecorations,
    preference: DecorationOverride,
) -> ClientPolicy {
    policy.decorations = match preference {
        DecorationOverride::Default => natural,
        DecorationOverride::Decorated => {
            let mut decorations = ClientPolicy::for_role(policy.role).decorations;
            decorations.minimize &= policy.capabilities.minimizable;
            decorations.maximize &= policy.capabilities.maximizable;
            decorations.close &= policy.capabilities.closable;
            decorations
        }
        DecorationOverride::Undecorated => ClientDecorations::NONE,
    };
    policy
}

/// Client geometry in root-window coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Geometry {
    /// Horizontal position.
    pub x: i32,
    /// Vertical position.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Signed changes applied to each edge during a relative resize.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResizeDeltas {
    /// Positive values grow the left edge outward.
    pub left: i32,
    /// Positive values grow the right edge outward.
    pub right: i32,
    /// Positive values grow the top edge outward.
    pub top: i32,
    /// Positive values grow the bottom edge outward.
    pub bottom: i32,
}

impl ResizeDeltas {
    /// Derives edge changes needed to transform one rectangle into another.
    #[must_use]
    pub fn between(current: Geometry, desired: Geometry) -> Self {
        Self {
            left: signed_difference(i64::from(current.x), i64::from(desired.x)),
            right: signed_difference(
                axis_end(desired.x, desired.width),
                axis_end(current.x, current.width),
            ),
            top: signed_difference(i64::from(current.y), i64::from(desired.y)),
            bottom: signed_difference(
                axis_end(desired.y, desired.height),
                axis_end(current.y, current.height),
            ),
        }
    }
}

/// One edge reservation with an inclusive span on the perpendicular axis.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EdgeReservation {
    /// Depth reserved inward from the output edge.
    pub depth: u32,
    /// Inclusive root-coordinate start of the reservation span.
    pub start: i32,
    /// Inclusive root-coordinate end of the reservation span.
    pub end: i32,
}

/// Space reserved at output edges by panels, docks, or user policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EdgeReservations {
    /// Left-edge reservation spanning vertical coordinates.
    pub left: EdgeReservation,
    /// Right-edge reservation spanning vertical coordinates.
    pub right: EdgeReservation,
    /// Top-edge reservation spanning horizontal coordinates.
    pub top: EdgeReservation,
    /// Bottom-edge reservation spanning horizontal coordinates.
    pub bottom: EdgeReservation,
}

/// Space reserved around client content for server-side decoration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecorationExtents {
    /// Pixels to the left of client content.
    pub left: u32,
    /// Pixels to the right of client content.
    pub right: u32,
    /// Pixels above client content.
    pub top: u32,
    /// Pixels below client content.
    pub bottom: u32,
}

impl DecorationExtents {
    /// Creates decoration extents for each content edge.
    #[must_use]
    pub const fn new(left: u32, right: u32, top: u32, bottom: u32) -> Self {
        Self {
            left,
            right,
            top,
            bottom,
        }
    }

    /// Expands content geometry to the outer decorated geometry.
    #[must_use]
    pub fn outer_geometry(self, content: Geometry) -> Geometry {
        Geometry::new(
            subtract_coordinate(content.x, self.left),
            subtract_coordinate(content.y, self.top),
            content
                .width
                .saturating_add(self.left)
                .saturating_add(self.right),
            content
                .height
                .saturating_add(self.top)
                .saturating_add(self.bottom),
        )
    }

    /// Contracts decorated outer geometry back to client content geometry.
    #[must_use]
    pub fn content_geometry(self, outer: Geometry) -> Geometry {
        Geometry::new(
            add_coordinate(outer.x, self.left),
            add_coordinate(outer.y, self.top),
            outer
                .width
                .saturating_sub(self.left)
                .saturating_sub(self.right),
            outer
                .height
                .saturating_sub(self.top)
                .saturating_sub(self.bottom),
        )
    }

    /// Returns the client's offset inside its decoration frame.
    #[must_use]
    pub fn content_offset(self) -> (i32, i32) {
        (
            i32::try_from(self.left).unwrap_or(i32::MAX),
            i32::try_from(self.top).unwrap_or(i32::MAX),
        )
    }
}

fn subtract_coordinate(coordinate: i32, amount: u32) -> i32 {
    let result = i64::from(coordinate).saturating_sub(i64::from(amount));
    i32::try_from(result).unwrap_or(i32::MIN)
}

/// A width and height pair used by client constraints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Size {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Size {
    /// Creates a non-empty size.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self {
            width: if width == 0 { 1 } else { width },
            height: if height == 0 { 1 } else { height },
        }
    }
}

/// ICCCM-compatible client size constraints.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SizeHints {
    /// Smallest accepted client size.
    pub minimum: Option<Size>,
    /// Largest accepted client size.
    pub maximum: Option<Size>,
    /// Base used when applying resize increments.
    pub base: Option<Size>,
    /// Per-axis resize increments.
    pub increment: Option<Size>,
    /// Permitted width-to-height ratio range.
    pub aspect: Option<AspectRange>,
}

/// A positive rational aspect ratio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AspectRatio {
    numerator: u32,
    denominator: u32,
}

impl AspectRatio {
    /// Creates a ratio, returning `None` if either component is zero.
    #[must_use]
    pub const fn new(numerator: u32, denominator: u32) -> Option<Self> {
        if numerator == 0 || denominator == 0 {
            None
        } else {
            Some(Self {
                numerator,
                denominator,
            })
        }
    }
}

/// Inclusive minimum and maximum aspect ratios.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AspectRange {
    /// Narrowest accepted width-to-height ratio.
    pub minimum: AspectRatio,
    /// Widest accepted width-to-height ratio.
    pub maximum: AspectRatio,
}

impl AspectRange {
    /// Creates an ordered range, returning `None` for contradictory ratios.
    #[must_use]
    pub fn new(minimum: AspectRatio, maximum: AspectRatio) -> Option<Self> {
        ratio_is_at_most(minimum, maximum).then_some(Self { minimum, maximum })
    }
}

impl SizeHints {
    /// Constrains a requested size to the client's valid size lattice.
    #[must_use]
    pub fn constrain(self, requested: Size) -> Size {
        let minimum = self.minimum.unwrap_or(Size::new(1, 1));
        let maximum = self.maximum.unwrap_or(Size {
            width: u32::MAX,
            height: u32::MAX,
        });
        let maximum = Size {
            width: maximum.width.max(minimum.width),
            height: maximum.height.max(minimum.height),
        };
        let mut result = Size {
            width: requested.width.clamp(minimum.width, maximum.width),
            height: requested.height.clamp(minimum.height, maximum.height),
        };

        if let Some(increment) = self.increment {
            let base = self.base.or(self.minimum).unwrap_or(Size::new(1, 1));
            result.width = snap_dimension(result.width, base.width, increment.width)
                .clamp(minimum.width, maximum.width);
            result.height = snap_dimension(result.height, base.height, increment.height)
                .clamp(minimum.height, maximum.height);
        }
        if let Some(aspect) = self.aspect {
            result = constrain_aspect(
                result,
                self.base.unwrap_or(Size {
                    width: 0,
                    height: 0,
                }),
                aspect,
            );
        }
        result
    }
}

fn ratio_is_at_most(left: AspectRatio, right: AspectRatio) -> bool {
    u64::from(left.numerator) * u64::from(right.denominator)
        <= u64::from(right.numerator) * u64::from(left.denominator)
}

fn constrain_aspect(size: Size, base: Size, range: AspectRange) -> Size {
    let width = size.width.saturating_sub(base.width).max(1);
    let mut height = size.height.saturating_sub(base.height).max(1);

    if u64::from(height) * u64::from(range.minimum.numerator)
        > u64::from(width) * u64::from(range.minimum.denominator)
    {
        height = scaled_height(width, range.minimum);
    }
    if u64::from(height) * u64::from(range.maximum.numerator)
        < u64::from(width) * u64::from(range.maximum.denominator)
    {
        height = scaled_height(width, range.maximum);
    }

    Size::new(
        width.saturating_add(base.width),
        height.saturating_add(base.height),
    )
}

fn scaled_height(width: u32, ratio: AspectRatio) -> u32 {
    let value = u64::from(width) * u64::from(ratio.denominator) / u64::from(ratio.numerator);
    u32::try_from(value.max(1)).unwrap_or(u32::MAX)
}

fn snap_dimension(requested: u32, base: u32, increment: u32) -> u32 {
    if increment <= 1 || requested <= base {
        return requested;
    }
    base.saturating_add(requested.saturating_sub(base) / increment * increment)
}

impl Geometry {
    /// Creates non-empty geometry.
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width: if width == 0 { 1 } else { width },
            height: if height == 0 { 1 } else { height },
        }
    }

    /// Translates geometry without allowing signed coordinate overflow.
    #[must_use]
    pub fn translated(self, x: i32, y: i32) -> Self {
        Self::new(
            add_signed_coordinate(self.x, i64::from(x)),
            add_signed_coordinate(self.y, i64::from(y)),
            self.width,
            self.height,
        )
    }

    /// Returns the largest rectangular area left after intersecting edge reservations.
    #[must_use]
    pub fn work_area(self, reservations: impl IntoIterator<Item = EdgeReservations>) -> Self {
        let mut left = 0;
        let mut right = 0;
        let mut top = 0;
        let mut bottom = 0;
        let x_end = coordinate_end(self.x, self.width);
        let y_end = coordinate_end(self.y, self.height);
        for reservation in reservations {
            if spans_overlap(reservation.left.start, reservation.left.end, self.y, y_end) {
                left = left.max(reservation.left.depth);
            }
            if spans_overlap(
                reservation.right.start,
                reservation.right.end,
                self.y,
                y_end,
            ) {
                right = right.max(reservation.right.depth);
            }
            if spans_overlap(reservation.top.start, reservation.top.end, self.x, x_end) {
                top = top.max(reservation.top.depth);
            }
            if spans_overlap(
                reservation.bottom.start,
                reservation.bottom.end,
                self.x,
                x_end,
            ) {
                bottom = bottom.max(reservation.bottom.depth);
            }
        }
        let left = left.min(self.width.saturating_sub(1));
        let right = right.min(self.width.saturating_sub(left).saturating_sub(1));
        let top = top.min(self.height.saturating_sub(1));
        let bottom = bottom.min(self.height.saturating_sub(top).saturating_sub(1));
        Self::new(
            add_coordinate(self.x, left),
            add_coordinate(self.y, top),
            self.width.saturating_sub(left).saturating_sub(right),
            self.height.saturating_sub(top).saturating_sub(bottom),
        )
    }

    /// Moves a rectangle fully inside bounds when its size permits, preserving
    /// its dimensions. Oversized axes align with the corresponding start edge.
    #[must_use]
    pub fn clamp_position(self, bounds: Self) -> Self {
        Self::new(
            clamp_placement_axis(i64::from(self.x), bounds.x, bounds.width, self.width),
            clamp_placement_axis(i64::from(self.y), bounds.y, bounds.height, self.height),
            self.width,
            self.height,
        )
    }

    /// Snaps moved geometry to the nearest matching bounds edges.
    #[must_use]
    pub fn snap_movement(self, bounds: Self, distance: u32) -> Self {
        Self::new(
            snap_axis_start(self.x, self.width, bounds.x, bounds.width, distance),
            snap_axis_start(self.y, self.height, bounds.y, bounds.height, distance),
            self.width,
            self.height,
        )
    }

    /// Snaps moved outer edges beside nearby rectangles.
    ///
    /// The closest edge on each axis wins. When adjacent edges snap, matching
    /// corners may also align within the same distance. Later targets win exact
    /// ties, allowing callers to supply bottom-to-top stacking order.
    #[must_use]
    pub fn snap_movement_to(self, targets: impl IntoIterator<Item = Self>, distance: u32) -> Self {
        let mut best_x = None;
        let mut best_y = None;
        let left = i64::from(self.x);
        let top = i64::from(self.y);
        let right = geometry_right(self);
        let bottom = geometry_bottom(self);

        for target in targets {
            let target_left = i64::from(target.x);
            let target_top = i64::from(target.y);
            let target_right = geometry_right(target);
            let target_bottom = geometry_bottom(target);
            let vertical_overlap = top < target_bottom && target_top < bottom;
            let horizontal_overlap = left < target_right && target_left < right;

            let adjacent_x = vertical_overlap
                .then(|| {
                    nearest_axis_snap(
                        left,
                        [
                            target_right,
                            target_left.saturating_sub(i64::from(self.width)),
                        ],
                        distance,
                    )
                })
                .flatten();
            let adjacent_y = horizontal_overlap
                .then(|| {
                    nearest_axis_snap(
                        top,
                        [
                            target_bottom,
                            target_top.saturating_sub(i64::from(self.height)),
                        ],
                        distance,
                    )
                })
                .flatten();

            update_axis_snap(&mut best_x, adjacent_x);
            update_axis_snap(&mut best_y, adjacent_y);
            if adjacent_x.is_some() {
                update_axis_snap(
                    &mut best_y,
                    nearest_axis_snap(
                        top,
                        [
                            target_top,
                            target_bottom.saturating_sub(i64::from(self.height)),
                        ],
                        distance,
                    ),
                );
            }
            if adjacent_y.is_some() {
                update_axis_snap(
                    &mut best_x,
                    nearest_axis_snap(
                        left,
                        [
                            target_left,
                            target_right.saturating_sub(i64::from(self.width)),
                        ],
                        distance,
                    ),
                );
            }
        }

        Self::new(
            best_x.map_or(self.x, |(_, position)| clamp_i64_to_i32(position)),
            best_y.map_or(self.y, |(_, position)| clamp_i64_to_i32(position)),
            self.width,
            self.height,
        )
    }

    /// Snaps bottom-right resize edges to matching bounds edges.
    #[must_use]
    pub fn snap_resize(self, bounds: Self, distance: u32) -> Self {
        Self::new(
            self.x,
            self.y,
            snap_axis_length(self.x, self.width, bounds.x, bounds.width, distance),
            snap_axis_length(self.y, self.height, bounds.y, bounds.height, distance),
        )
    }
}

/// Places a requested outer size within target bounds.
///
/// Unspecified axes represented by [`AxisPlacement::Keep`] retain their offset
/// from the source bounds, which makes output-to-output moves predictable.
/// The final rectangle is kept inside the target whenever its size permits.
#[must_use]
pub fn move_resize_geometry(
    current: Geometry,
    source_bounds: Geometry,
    target_bounds: Geometry,
    size: Size,
    x: AxisPlacement,
    y: AxisPlacement,
) -> Geometry {
    Geometry::new(
        place_axis(
            current.x,
            source_bounds.x,
            target_bounds.x,
            target_bounds.width,
            size.width,
            x,
        ),
        place_axis(
            current.y,
            source_bounds.y,
            target_bounds.y,
            target_bounds.height,
            size.height,
            y,
        ),
        size.width,
        size.height,
    )
    .clamp_position(target_bounds)
}

fn place_axis(
    current: i32,
    source_start: i32,
    target_start: i32,
    target_length: u32,
    object_length: u32,
    placement: AxisPlacement,
) -> i32 {
    let source_start = i64::from(source_start);
    let target_start = i64::from(target_start);
    let target_length = i64::from(target_length);
    let object_length = i64::from(object_length);
    let placed = match placement {
        AxisPlacement::Keep => {
            target_start.saturating_add(i64::from(current).saturating_sub(source_start))
        }
        AxisPlacement::Start(offset) => target_start.saturating_add(i64::from(offset)),
        AxisPlacement::Center => {
            target_start.saturating_add(target_length.saturating_sub(object_length) / 2)
        }
        AxisPlacement::End(inset) => target_start
            .saturating_add(target_length)
            .saturating_sub(object_length)
            .saturating_sub(i64::from(inset)),
    };
    clamp_i64_to_i32(placed)
}

/// Applies edge-relative resize deltas while preserving constrained anchors.
///
/// Non-zero changes smaller than an ICCCM size increment are promoted to one
/// increment, matching the behavior users expect from Openbox relative resize
/// actions. If size constraints alter the requested result, the opposite edge
/// remains fixed as far as the resulting dimensions permit.
#[must_use]
pub fn relative_resize_geometry(
    geometry: Geometry,
    deltas: ResizeDeltas,
    hints: SizeHints,
) -> Geometry {
    let increment = hints.increment.unwrap_or(Size::new(1, 1));
    let left = promote_resize_delta(deltas.left, increment.width);
    let right = promote_resize_delta(deltas.right, increment.width);
    let top = promote_resize_delta(deltas.top, increment.height);
    let bottom = promote_resize_delta(deltas.bottom, increment.height);
    let requested = Size::new(
        resize_dimension(geometry.width, left, right),
        resize_dimension(geometry.height, top, bottom),
    );
    let constrained = hints.constrain(requested);
    let width_difference = i64::from(geometry.width) - i64::from(constrained.width);
    let height_difference = i64::from(geometry.height) - i64::from(constrained.height);
    let x_offset = constrained_edge_offset(-left, width_difference);
    let y_offset = constrained_edge_offset(-top, height_difference);

    Geometry::new(
        add_signed_coordinate(geometry.x, x_offset),
        add_signed_coordinate(geometry.y, y_offset),
        constrained.width,
        constrained.height,
    )
}

/// Selects the nearest candidate in an eight-way spatial direction.
///
/// Candidate centers inside the direction's 90-degree cone always outrank
/// candidates outside it. Within either group, forward and perpendicular
/// distances are weighted equally, matching Openbox directional navigation
/// without its fixed-distance penalty; equal totals favor closer alignment.
/// Fully equal scores preserve input order, so a caller can use
/// most-recently-used order as a deterministic final tie-breaker.
#[must_use]
pub fn directional_target<T>(
    origin: T,
    origin_geometry: Geometry,
    candidates: impl IntoIterator<Item = (T, Geometry)>,
    direction: SpatialDirection,
) -> Option<T>
where
    T: Copy + Eq,
{
    let mut best = None;
    for (candidate, geometry) in candidates {
        if candidate == origin {
            continue;
        }
        let Some(score) = directional_score(origin_geometry, geometry, direction) else {
            continue;
        };
        if best.is_none_or(|(_, best_score)| score < best_score) {
            best = Some((candidate, score));
        }
    }
    best.map(|(candidate, _)| candidate)
}

fn directional_score(
    origin: Geometry,
    candidate: Geometry,
    direction: SpatialDirection,
) -> Option<(bool, u64, u64, u64)> {
    let horizontal = doubled_center(candidate.x, candidate.width)
        .saturating_sub(doubled_center(origin.x, origin.width));
    let vertical = doubled_center(candidate.y, candidate.height)
        .saturating_sub(doubled_center(origin.y, origin.height));
    let (forward, perpendicular) = match direction {
        SpatialDirection::Left => (horizontal.saturating_neg(), vertical),
        SpatialDirection::Right => (horizontal, vertical),
        SpatialDirection::Up => (vertical.saturating_neg(), horizontal),
        SpatialDirection::Down => (vertical, horizontal),
        SpatialDirection::UpLeft => (
            horizontal.saturating_add(vertical).saturating_neg(),
            vertical.saturating_sub(horizontal),
        ),
        SpatialDirection::UpRight => (
            horizontal.saturating_sub(vertical),
            horizontal.saturating_add(vertical),
        ),
        SpatialDirection::DownLeft => (
            vertical.saturating_sub(horizontal),
            horizontal.saturating_add(vertical),
        ),
        SpatialDirection::DownRight => (
            horizontal.saturating_add(vertical),
            vertical.saturating_sub(horizontal),
        ),
    };
    let distance = u64::try_from(forward)
        .ok()
        .filter(|distance| *distance > 0)?;
    let offset = perpendicular.unsigned_abs();
    Some((
        offset > distance,
        distance.saturating_add(offset),
        offset,
        distance,
    ))
}

fn doubled_center(start: i32, length: u32) -> i64 {
    i64::from(start)
        .saturating_mul(2)
        .saturating_add(i64::from(length))
}

/// Chooses Openbox-style opposite restacking from bottom-to-top rectangles.
///
/// The caller supplies only visible peers in the target's effective stacking
/// layer. Any overlapping peer above the target selects [`RestackDecision::Raise`].
/// Otherwise an overlapping peer below selects [`RestackDecision::Lower`].
/// With no overlap—or when the target is absent—the order is unchanged.
#[must_use]
pub fn adaptive_restack<T>(
    target: T,
    target_geometry: Geometry,
    stacking: impl IntoIterator<Item = (T, Geometry)>,
) -> RestackDecision
where
    T: Eq,
{
    let mut found_target = false;
    let mut overlaps_below = false;
    for (candidate, geometry) in stacking {
        if candidate == target {
            found_target = true;
        } else if geometries_intersect(target_geometry, geometry) {
            if found_target {
                return RestackDecision::Raise;
            }
            overlaps_below = true;
        }
    }
    if found_target && overlaps_below {
        RestackDecision::Lower
    } else {
        RestackDecision::Unchanged
    }
}

/// Moves geometry to the next overlapping obstacle edge or work-area edge.
///
/// A second invocation at a near edge advances to the corresponding far edge,
/// preserving Openbox's useful ability to step across adjacent windows.
#[must_use]
pub fn directional_move_geometry(
    geometry: Geometry,
    bounds: Geometry,
    obstacles: &[Geometry],
    direction: CardinalDirection,
) -> Geometry {
    let horizontal = matches!(
        direction,
        CardinalDirection::Left | CardinalDirection::Right
    );
    let current = i64::from(if horizontal { geometry.x } else { geometry.y });
    let subject_length = i64::from(if horizontal {
        geometry.width
    } else {
        geometry.height
    });
    let bounds_start = i64::from(if horizontal { bounds.x } else { bounds.y });
    let bounds_length = i64::from(if horizontal {
        bounds.width
    } else {
        bounds.height
    });
    let minimum = bounds_start;
    let maximum = bounds_start.saturating_add(bounds_length.saturating_sub(subject_length).max(0));
    let toward_start = matches!(direction, CardinalDirection::Left | CardinalDirection::Up);
    let mut destination = if toward_start { minimum } else { maximum };

    for obstacle in obstacles {
        let overlaps = if horizontal {
            spans_overlap(
                geometry.y,
                coordinate_end(geometry.y, geometry.height),
                obstacle.y,
                coordinate_end(obstacle.y, obstacle.height),
            )
        } else {
            spans_overlap(
                geometry.x,
                coordinate_end(geometry.x, geometry.width),
                obstacle.x,
                coordinate_end(obstacle.x, obstacle.width),
            )
        };
        if !overlaps {
            continue;
        }
        let obstacle_start = i64::from(if horizontal { obstacle.x } else { obstacle.y });
        let obstacle_length = i64::from(if horizontal {
            obstacle.width
        } else {
            obstacle.height
        });
        let before = obstacle_start.saturating_sub(subject_length);
        let after = obstacle_start.saturating_add(obstacle_length);
        for candidate in [before, after] {
            if candidate < minimum || candidate > maximum {
                continue;
            }
            let is_closer = if toward_start {
                candidate < current && candidate > destination
            } else {
                candidate > current && candidate < destination
            };
            if is_closer {
                destination = candidate;
            }
        }
    }

    let destination = i32::try_from(destination).unwrap_or(if destination.is_negative() {
        i32::MIN
    } else {
        i32::MAX
    });
    if horizontal {
        Geometry::new(destination, geometry.y, geometry.width, geometry.height)
    } else {
        Geometry::new(geometry.x, destination, geometry.width, geometry.height)
    }
}

/// Grows one rectangle edge toward the next obstacle or work-area edge.
///
/// With [`BlockingEdgePolicy::Stop`], a side already touching any candidate
/// edge remains unchanged. This is the first-pass behavior used by
/// [`grow_to_fill_geometry`].
#[must_use]
pub fn directional_grow_geometry(
    geometry: Geometry,
    bounds: Geometry,
    obstacles: &[Geometry],
    direction: CardinalDirection,
    blocking_edge: BlockingEdgePolicy,
) -> Geometry {
    let current = directional_side(geometry, direction);
    if blocking_edge == BlockingEdgePolicy::Stop
        && edge_candidates(geometry, obstacles, direction)
            .chain(std::iter::once(directional_boundary(bounds, direction)))
            .any(|candidate| candidate == current)
    {
        return geometry;
    }
    let target = nearest_edge(
        current,
        directional_boundary(bounds, direction),
        edge_candidates(geometry, obstacles, direction),
        direction,
    );
    geometry_with_side(geometry, direction, target)
}

/// Shrinks the edge opposite `direction` toward the next obstacle.
///
/// At least half of the original axis remains, matching Openbox's guard
/// against an edge action unexpectedly collapsing a window.
#[must_use]
pub fn directional_shrink_geometry(
    geometry: Geometry,
    bounds: Geometry,
    obstacles: &[Geometry],
    direction: CardinalDirection,
) -> Geometry {
    let moving_side = opposite_direction(direction);
    let current = directional_side(geometry, moving_side);
    let target = nearest_edge(
        current,
        directional_boundary(bounds, direction),
        edge_candidates(geometry, obstacles, direction),
        direction,
    );
    let half = i64::from(match direction {
        CardinalDirection::Left | CardinalDirection::Right => geometry.width / 2,
        CardinalDirection::Up | CardinalDirection::Down => geometry.height / 2,
    });
    let anchored_side = directional_side(geometry, direction);
    let target = match direction {
        CardinalDirection::Right | CardinalDirection::Down => {
            target.min(anchored_side.saturating_sub(half.max(1)))
        }
        CardinalDirection::Left | CardinalDirection::Up => {
            target.max(anchored_side.saturating_add(half.max(1)))
        }
    };
    geometry_with_side(geometry, moving_side, target)
}

/// Grows all four edges into surrounding free space.
///
/// The first pass leaves sides that already touch an obstacle in place. Only
/// when no side can grow does the second pass advance past those blockers.
#[must_use]
pub fn grow_to_fill_geometry(
    geometry: Geometry,
    bounds: Geometry,
    obstacles: &[Geometry],
) -> Geometry {
    let first = grow_all_edges(geometry, bounds, obstacles, BlockingEdgePolicy::Stop);
    if first == geometry {
        grow_all_edges(geometry, bounds, obstacles, BlockingEdgePolicy::Cross)
    } else {
        first
    }
}

fn grow_all_edges(
    geometry: Geometry,
    bounds: Geometry,
    obstacles: &[Geometry],
    blocking_edge: BlockingEdgePolicy,
) -> Geometry {
    let left = directional_grow_geometry(
        geometry,
        bounds,
        obstacles,
        CardinalDirection::Left,
        blocking_edge,
    );
    let right = directional_grow_geometry(
        geometry,
        bounds,
        obstacles,
        CardinalDirection::Right,
        blocking_edge,
    );
    let up = directional_grow_geometry(
        geometry,
        bounds,
        obstacles,
        CardinalDirection::Up,
        blocking_edge,
    );
    let down = directional_grow_geometry(
        geometry,
        bounds,
        obstacles,
        CardinalDirection::Down,
        blocking_edge,
    );
    let right_edge = axis_end(right.x, right.width);
    let bottom_edge = axis_end(down.y, down.height);
    Geometry::new(
        clamp_i64_to_i32(left.x.into()),
        clamp_i64_to_i32(up.y.into()),
        span_dimension(i64::from(left.x), right_edge),
        span_dimension(i64::from(up.y), bottom_edge),
    )
}

fn edge_candidates(
    geometry: Geometry,
    obstacles: &[Geometry],
    direction: CardinalDirection,
) -> impl Iterator<Item = i64> + '_ {
    obstacles
        .iter()
        .filter(move |obstacle| perpendicular_overlap(geometry, **obstacle, direction))
        .flat_map(move |obstacle| {
            let start = match direction {
                CardinalDirection::Left | CardinalDirection::Right => i64::from(obstacle.x),
                CardinalDirection::Up | CardinalDirection::Down => i64::from(obstacle.y),
            };
            let end = match direction {
                CardinalDirection::Left | CardinalDirection::Right => {
                    axis_end(obstacle.x, obstacle.width)
                }
                CardinalDirection::Up | CardinalDirection::Down => {
                    axis_end(obstacle.y, obstacle.height)
                }
            };
            [start, end]
        })
}

fn perpendicular_overlap(
    geometry: Geometry,
    obstacle: Geometry,
    direction: CardinalDirection,
) -> bool {
    match direction {
        CardinalDirection::Left | CardinalDirection::Right => spans_overlap(
            geometry.y,
            coordinate_end(geometry.y, geometry.height),
            obstacle.y,
            coordinate_end(obstacle.y, obstacle.height),
        ),
        CardinalDirection::Up | CardinalDirection::Down => spans_overlap(
            geometry.x,
            coordinate_end(geometry.x, geometry.width),
            obstacle.x,
            coordinate_end(obstacle.x, obstacle.width),
        ),
    }
}

fn directional_side(geometry: Geometry, direction: CardinalDirection) -> i64 {
    match direction {
        CardinalDirection::Left => i64::from(geometry.x),
        CardinalDirection::Right => axis_end(geometry.x, geometry.width),
        CardinalDirection::Up => i64::from(geometry.y),
        CardinalDirection::Down => axis_end(geometry.y, geometry.height),
    }
}

fn directional_boundary(bounds: Geometry, direction: CardinalDirection) -> i64 {
    match direction {
        CardinalDirection::Left => i64::from(bounds.x),
        CardinalDirection::Right => axis_end(bounds.x, bounds.width),
        CardinalDirection::Up => i64::from(bounds.y),
        CardinalDirection::Down => axis_end(bounds.y, bounds.height),
    }
}

fn nearest_edge(
    current: i64,
    boundary: i64,
    candidates: impl Iterator<Item = i64>,
    direction: CardinalDirection,
) -> i64 {
    let toward_start = matches!(direction, CardinalDirection::Left | CardinalDirection::Up);
    let boundary_is_beyond = if toward_start {
        boundary < current
    } else {
        boundary > current
    };
    let selected = if boundary_is_beyond {
        boundary
    } else {
        current
    };
    candidates.fold(selected, |selected, candidate| {
        let is_beyond = if toward_start {
            candidate < current && candidate >= boundary
        } else {
            candidate > current && candidate <= boundary
        };
        let is_nearer = if toward_start {
            candidate > selected
        } else {
            candidate < selected
        };
        if is_beyond && is_nearer {
            candidate
        } else {
            selected
        }
    })
}

fn opposite_direction(direction: CardinalDirection) -> CardinalDirection {
    match direction {
        CardinalDirection::Left => CardinalDirection::Right,
        CardinalDirection::Right => CardinalDirection::Left,
        CardinalDirection::Up => CardinalDirection::Down,
        CardinalDirection::Down => CardinalDirection::Up,
    }
}

fn geometry_with_side(geometry: Geometry, direction: CardinalDirection, target: i64) -> Geometry {
    let left = i64::from(geometry.x);
    let right = axis_end(geometry.x, geometry.width);
    let top = i64::from(geometry.y);
    let bottom = axis_end(geometry.y, geometry.height);
    match direction {
        CardinalDirection::Left => Geometry::new(
            clamp_i64_to_i32(target),
            geometry.y,
            span_dimension(target, right),
            geometry.height,
        ),
        CardinalDirection::Right => Geometry::new(
            geometry.x,
            geometry.y,
            span_dimension(left, target),
            geometry.height,
        ),
        CardinalDirection::Up => Geometry::new(
            geometry.x,
            clamp_i64_to_i32(target),
            geometry.width,
            span_dimension(target, bottom),
        ),
        CardinalDirection::Down => Geometry::new(
            geometry.x,
            geometry.y,
            geometry.width,
            span_dimension(top, target),
        ),
    }
}

fn axis_end(start: i32, length: u32) -> i64 {
    i64::from(start).saturating_add(i64::from(length))
}

fn span_dimension(start: i64, end: i64) -> u32 {
    u32::try_from(end.saturating_sub(start).clamp(1, i64::from(u32::MAX))).unwrap_or(u32::MAX)
}

fn signed_difference(left: i64, right: i64) -> i32 {
    clamp_i64_to_i32(left.saturating_sub(right))
}

fn clamp_i64_to_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(if value.is_negative() {
        i32::MIN
    } else {
        i32::MAX
    })
}

fn promote_resize_delta(delta: i32, increment: u32) -> i64 {
    let delta = i64::from(delta);
    let increment = i64::from(increment.max(1));
    if delta != 0 && delta.abs() < increment {
        increment * delta.signum()
    } else {
        delta
    }
}

fn resize_dimension(current: u32, first: i64, second: i64) -> u32 {
    let requested = i64::from(current)
        .saturating_add(first)
        .saturating_add(second)
        .clamp(1, i64::from(u32::MAX));
    u32::try_from(requested).unwrap_or(u32::MAX)
}

fn constrained_edge_offset(requested: i64, size_difference: i64) -> i64 {
    match requested.cmp(&0) {
        std::cmp::Ordering::Less => requested.max(size_difference),
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => requested.min(size_difference),
    }
}

fn add_signed_coordinate(coordinate: i32, amount: i64) -> i32 {
    let result = i64::from(coordinate).saturating_add(amount);
    i32::try_from(result).unwrap_or(if result.is_negative() {
        i32::MIN
    } else {
        i32::MAX
    })
}

/// Places an outer window rectangle on the least-overlap edge grid.
///
/// Candidate positions are derived from visible obstacle and work-area edges.
/// Overlapping an additional window carries a fixed penalty, matching the
/// useful Openbox behavior that prefers one substantial overlap over covering
/// several clients through narrow gaps.
#[must_use]
pub fn smart_placement(
    size: Size,
    bounds: Geometry,
    obstacles: &[Geometry],
    center_free_space: bool,
) -> Geometry {
    if size.width > bounds.width || size.height > bounds.height {
        return Geometry::new(bounds.x, bounds.y, size.width, size.height);
    }
    if !obstacles
        .iter()
        .any(|obstacle| geometries_intersect(*obstacle, bounds))
    {
        return if center_free_space {
            centered_placement(size, bounds, bounds)
        } else {
            Geometry::new(bounds.x, bounds.y, size.width, size.height)
        };
    }

    let obstacles = obstacles
        .iter()
        .copied()
        .filter(|obstacle| geometries_intersect(*obstacle, bounds))
        .collect::<Vec<_>>();
    let mut x_edges = Vec::with_capacity(2 + 2 * obstacles.len());
    let mut y_edges = Vec::with_capacity(2 + 2 * obstacles.len());
    x_edges.extend([i64::from(bounds.x), geometry_right(bounds)]);
    y_edges.extend([i64::from(bounds.y), geometry_bottom(bounds)]);
    for obstacle in obstacles.iter().copied() {
        x_edges.push(i64::from(obstacle.x));
        x_edges.push(geometry_right(obstacle));
        y_edges.push(i64::from(obstacle.y));
        y_edges.push(geometry_bottom(obstacle));
    }
    x_edges.sort_unstable();
    x_edges.dedup();
    y_edges.sort_unstable();
    y_edges.dedup();
    let width = i64::from(size.width);
    let height = i64::from(size.height);
    let mut best = Geometry::new(bounds.x, bounds.y, size.width, size.height);
    let mut best_score = u128::MAX;
    let mut overlap_columns = BTreeMap::<i32, Vec<(Geometry, u64)>>::new();

    'grid: for x_edge in &x_edges {
        for y_edge in &y_edges {
            for (x, y) in [
                (*x_edge, *y_edge),
                (*x_edge, y_edge.saturating_sub(height)),
                (x_edge.saturating_sub(width), *y_edge),
                (x_edge.saturating_sub(width), y_edge.saturating_sub(height)),
            ] {
                let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y)) else {
                    continue;
                };
                let candidate = Geometry::new(x, y, size.width, size.height);
                if !geometry_contains(bounds, candidate) {
                    continue;
                }
                let column = overlap_columns.entry(candidate.x).or_insert_with(|| {
                    horizontal_placement_overlaps(candidate.x, candidate.width, &obstacles)
                });
                let score = placement_overlap_score_in_column(
                    candidate.y,
                    candidate.height,
                    column,
                    best_score,
                );
                if score < best_score {
                    best = candidate;
                    best_score = score;
                }
                if best_score == 0 {
                    break 'grid;
                }
            }
        }
    }

    if center_free_space && best_score == 0 {
        center_in_free_field(best, bounds, &obstacles, &x_edges, &y_edges)
    } else {
        best
    }
}

/// Centers an outer window rectangle over an anchor and clamps it to bounds.
///
/// Passing the work area itself as `anchor` centers on the complete work area.
#[must_use]
pub fn centered_placement(size: Size, bounds: Geometry, anchor: Geometry) -> Geometry {
    let centered_x = i64::from(anchor.x)
        .saturating_add(i64::from(anchor.width) / 2)
        .saturating_sub(i64::from(size.width) / 2);
    let centered_y = i64::from(anchor.y)
        .saturating_add(i64::from(anchor.height) / 2)
        .saturating_sub(i64::from(size.height) / 2);
    Geometry::new(
        clamp_placement_axis(centered_x, bounds.x, bounds.width, size.width),
        clamp_placement_axis(centered_y, bounds.y, bounds.height, size.height),
        size.width,
        size.height,
    )
}

fn center_in_free_field(
    placement: Geometry,
    bounds: Geometry,
    obstacles: &[Geometry],
    x_edges: &[i64],
    y_edges: &[i64],
) -> Geometry {
    let right = geometry_right(placement);
    let bottom = geometry_bottom(placement);
    let right_index = x_edges.partition_point(|edge| *edge < right);
    let bottom_index = y_edges.partition_point(|edge| *edge < bottom);
    let Some(&initial_right) = x_edges.get(right_index) else {
        return placement;
    };
    let Some(&initial_bottom) = y_edges.get(bottom_index) else {
        return placement;
    };
    let (Ok(initial_width), Ok(initial_height)) = (
        u32::try_from(initial_right.saturating_sub(i64::from(placement.x))),
        u32::try_from(initial_bottom.saturating_sub(i64::from(placement.y))),
    ) else {
        return placement;
    };

    let mut expanded_right = right_index;
    for (index, edge) in x_edges
        .iter()
        .enumerate()
        .skip(right_index.saturating_add(1))
    {
        let width = edge.saturating_sub(i64::from(placement.x));
        let Ok(width) = u32::try_from(width) else {
            break;
        };
        let field = Geometry::new(placement.x, placement.y, width, initial_height);
        if !geometry_contains(bounds, field) || placement_overlap_score(field, obstacles) != 0 {
            break;
        }
        expanded_right = index;
    }

    let mut expanded_bottom = bottom_index;
    for (index, edge) in y_edges
        .iter()
        .enumerate()
        .skip(bottom_index.saturating_add(1))
    {
        let height = edge.saturating_sub(i64::from(placement.y));
        let Ok(height) = u32::try_from(height) else {
            break;
        };
        let field = Geometry::new(placement.x, placement.y, initial_width, height);
        if !geometry_contains(bounds, field) || placement_overlap_score(field, obstacles) != 0 {
            break;
        }
        expanded_bottom = index;
    }

    let mut field_width = i64::from(initial_width);
    let mut field_height = i64::from(initial_height);
    if expanded_right == right_index && expanded_bottom != bottom_index {
        field_height = y_edges[expanded_bottom].saturating_sub(i64::from(placement.y));
    } else if expanded_right != right_index && expanded_bottom == bottom_index {
        field_width = x_edges[expanded_right].saturating_sub(i64::from(placement.x));
    }
    let x = i64::from(placement.x)
        .saturating_add(field_width.saturating_sub(i64::from(placement.width)) / 2);
    let y = i64::from(placement.y)
        .saturating_add(field_height.saturating_sub(i64::from(placement.height)) / 2);
    Geometry::new(
        i32::try_from(x).unwrap_or(placement.x),
        i32::try_from(y).unwrap_or(placement.y),
        placement.width,
        placement.height,
    )
}

fn placement_overlap_score(candidate: Geometry, obstacles: &[Geometry]) -> u128 {
    placement_overlap_score_bounded(candidate, obstacles, u128::MAX)
}

fn placement_overlap_score_bounded(
    candidate: Geometry,
    obstacles: &[Geometry],
    upper_bound: u128,
) -> u128 {
    let mut score = 0_u128;
    for obstacle in obstacles {
        let width = geometry_right(candidate)
            .min(geometry_right(*obstacle))
            .saturating_sub(i64::from(candidate.x).max(i64::from(obstacle.x)));
        let height = geometry_bottom(candidate)
            .min(geometry_bottom(*obstacle))
            .saturating_sub(i64::from(candidate.y).max(i64::from(obstacle.y)));
        if width <= 0 || height <= 0 {
            continue;
        }
        // Both factors are bounded by the narrower rectangle's u32 dimension,
        // so the product is exact in u64.
        let area = u128::from(width.unsigned_abs() * height.unsigned_abs());
        score = score.saturating_add(area).saturating_add(6_000);
        if score >= upper_bound {
            return score;
        }
    }
    score
}

fn horizontal_placement_overlaps(
    x: i32,
    width: u32,
    obstacles: &[Geometry],
) -> Vec<(Geometry, u64)> {
    let right = i64::from(x).saturating_add(i64::from(width));
    obstacles
        .iter()
        .copied()
        .filter_map(|obstacle| {
            let overlap = right
                .min(geometry_right(obstacle))
                .saturating_sub(i64::from(x).max(i64::from(obstacle.x)));
            (overlap > 0).then(|| (obstacle, overlap.unsigned_abs()))
        })
        .collect()
}

fn placement_overlap_score_in_column(
    y: i32,
    height: u32,
    overlaps: &[(Geometry, u64)],
    upper_bound: u128,
) -> u128 {
    let bottom = i64::from(y).saturating_add(i64::from(height));
    let mut score = 0_u128;
    for (obstacle, width) in overlaps {
        let overlap = bottom
            .min(geometry_bottom(*obstacle))
            .saturating_sub(i64::from(y).max(i64::from(obstacle.y)));
        if overlap <= 0 {
            continue;
        }
        // Both factors fit in u32, so the product is exact in u64.
        let area = u128::from(width * overlap.unsigned_abs());
        score = score.saturating_add(area).saturating_add(6_000);
        if score >= upper_bound {
            return score;
        }
    }
    score
}

fn geometries_intersect(left: Geometry, right: Geometry) -> bool {
    i64::from(left.x) < geometry_right(right)
        && geometry_right(left) > i64::from(right.x)
        && i64::from(left.y) < geometry_bottom(right)
        && geometry_bottom(left) > i64::from(right.y)
}

fn intersection_area(left: Geometry, right: Geometry) -> u64 {
    let width = geometry_right(left)
        .min(geometry_right(right))
        .saturating_sub(i64::from(left.x).max(i64::from(right.x)));
    let height = geometry_bottom(left)
        .min(geometry_bottom(right))
        .saturating_sub(i64::from(left.y).max(i64::from(right.y)));
    if width <= 0 || height <= 0 {
        0
    } else {
        // Both factors are bounded by the narrower rectangle's u32 dimension,
        // so the product is exact in u64.
        width.unsigned_abs() * height.unsigned_abs()
    }
}

fn rectangle_distance_squared(left: Geometry, right: Geometry) -> u128 {
    let horizontal = if geometry_right(left) < i64::from(right.x) {
        i64::from(right.x).saturating_sub(geometry_right(left))
    } else if geometry_right(right) < i64::from(left.x) {
        i64::from(left.x).saturating_sub(geometry_right(right))
    } else {
        0
    };
    let vertical = if geometry_bottom(left) < i64::from(right.y) {
        i64::from(right.y).saturating_sub(geometry_bottom(left))
    } else if geometry_bottom(right) < i64::from(left.y) {
        i64::from(left.y).saturating_sub(geometry_bottom(right))
    } else {
        0
    };
    // Each gap is below 2^32, so its square is exact in u64; only the final
    // sum can exceed u64 and is therefore widened.
    let horizontal = horizontal.unsigned_abs();
    let vertical = vertical.unsigned_abs();
    u128::from(horizontal * horizontal) + u128::from(vertical * vertical)
}

fn geometry_contains(bounds: Geometry, candidate: Geometry) -> bool {
    i64::from(candidate.x) >= i64::from(bounds.x)
        && i64::from(candidate.y) >= i64::from(bounds.y)
        && geometry_right(candidate) <= geometry_right(bounds)
        && geometry_bottom(candidate) <= geometry_bottom(bounds)
}

fn geometry_right(geometry: Geometry) -> i64 {
    i64::from(geometry.x).saturating_add(i64::from(geometry.width))
}

fn geometry_bottom(geometry: Geometry) -> i64 {
    i64::from(geometry.y).saturating_add(i64::from(geometry.height))
}

fn clamp_placement_axis(position: i64, start: i32, length: u32, requested: u32) -> i32 {
    let start = i64::from(start);
    let maximum = start.saturating_add(i64::from(length.saturating_sub(requested)));
    i32::try_from(position.clamp(start, maximum)).unwrap_or(if position.is_negative() {
        i32::MIN
    } else {
        i32::MAX
    })
}

fn snap_axis_start(
    start: i32,
    length: u32,
    bounds_start: i32,
    bounds_length: u32,
    distance: u32,
) -> i32 {
    let start = i64::from(start);
    let near = i64::from(bounds_start);
    let far = near
        .saturating_add(i64::from(bounds_length))
        .saturating_sub(i64::from(length));
    let near_delta = start.abs_diff(near);
    let far_delta = start.abs_diff(far);
    let snapped = if near_delta <= u64::from(distance) && near_delta <= far_delta {
        near
    } else if far_delta <= u64::from(distance) {
        far
    } else {
        start
    };
    i32::try_from(snapped).unwrap_or(if snapped.is_negative() {
        i32::MIN
    } else {
        i32::MAX
    })
}

fn snap_axis_length(
    start: i32,
    length: u32,
    bounds_start: i32,
    bounds_length: u32,
    distance: u32,
) -> u32 {
    let start = i64::from(start);
    let edge = start.saturating_add(i64::from(length));
    let target = i64::from(bounds_start).saturating_add(i64::from(bounds_length));
    if edge.abs_diff(target) > u64::from(distance) || target <= start {
        return length;
    }
    u32::try_from(target - start).unwrap_or(u32::MAX).max(1)
}

fn nearest_axis_snap(current: i64, candidates: [i64; 2], distance: u32) -> Option<(u64, i64)> {
    candidates
        .into_iter()
        .map(|candidate| (current.abs_diff(candidate), candidate))
        .filter(|(delta, _)| *delta <= u64::from(distance))
        .min_by_key(|(delta, _)| *delta)
}

fn update_axis_snap(best: &mut Option<(u64, i64)>, candidate: Option<(u64, i64)>) {
    if let Some(candidate) = candidate
        && best.is_none_or(|best| candidate.0 <= best.0)
    {
        *best = Some(candidate);
    }
}

fn coordinate_end(start: i32, length: u32) -> i32 {
    add_coordinate(start, length.saturating_sub(1))
}

fn add_coordinate(coordinate: i32, amount: u32) -> i32 {
    i32::try_from(i64::from(coordinate).saturating_add(i64::from(amount))).unwrap_or(i32::MAX)
}

fn spans_overlap(first_start: i32, first_end: i32, second_start: i32, second_end: i32) -> bool {
    first_start <= first_end && first_start <= second_end && first_end >= second_start
}

/// State retained for a managed top-level client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Client {
    /// Backend identifier.
    pub id: ClientId,
    /// Last known geometry.
    pub geometry: Geometry,
    /// Client-supplied size constraints.
    pub size_hints: SizeHints,
    /// Anchor used when a resize omits coordinates.
    pub gravity: Gravity,
    /// Functional role, capabilities, and decoration policy.
    pub policy: ClientPolicy,
    /// Decoration policy derived from client hints and application rules.
    pub natural_decorations: ClientDecorations,
    /// Explicit user preference layered over natural decorations.
    pub decoration_override: DecorationOverride,
    /// User-interface presentation hints, independent of display protocol.
    pub presentation: ClientPresentation,
    /// Client or application group this client is transient for.
    pub transient_for: Option<TransientTarget>,
    /// Application group identifier, which need not be a managed client.
    pub group: Option<ClientId>,
    /// Whether this transient blocks interaction with its parent or group.
    pub modal: bool,
    /// Whether the client is managed but intentionally not mapped.
    pub iconic: bool,
    /// Whether only the client's server-side titlebar is visible.
    pub shaded: bool,
    /// Policy workspace membership, independent of display protocol.
    pub workspace: WorkspaceAssignment,
    /// User-requested stacking preference.
    pub layer: ClientLayer,
    /// Active maximize axes and their restore geometry.
    pub maximize: Option<MaximizeState>,
    /// Active fullscreen state and its restore geometry.
    pub fullscreen: Option<FullscreenState>,
    /// Exact output area covered without a managed fullscreen transition.
    pub output_coverage: Option<OutputCoverage>,
}

impl Client {
    /// Returns the content geometry that should survive after management ends.
    #[must_use]
    pub const fn unmanaged_geometry(self) -> Geometry {
        let mut geometry = match self.fullscreen {
            Some(fullscreen) => fullscreen.restore,
            None => self.geometry,
        };
        if let Some(maximize) = self.maximize {
            if maximize.horizontal {
                geometry.x = maximize.restore.x;
                geometry.width = maximize.restore.width;
            }
            if maximize.vertical {
                geometry.y = maximize.restore.y;
                geometry.height = maximize.restore.height;
            }
        }
        geometry
    }

    /// Resolves the user operations available in the client's current state.
    #[must_use]
    pub const fn operations(self) -> ClientOperations {
        let capabilities = self.policy.capabilities;
        let fullscreen = self.fullscreen.is_some();
        let (workspace_movable, above, below) = match self.policy.role {
            ClientRole::Desktop => (false, false, false),
            ClientRole::Dock => (true, false, true),
            ClientRole::Normal
            | ClientRole::Dialog
            | ClientRole::Utility
            | ClientRole::Toolbar
            | ClientRole::Menu => (true, !fullscreen, !fullscreen),
            ClientRole::Splash
            | ClientRole::DropdownMenu
            | ClientRole::PopupMenu
            | ClientRole::Tooltip
            | ClientRole::Notification
            | ClientRole::Combo
            | ClientRole::DragAndDrop => (true, false, false),
        };
        ClientOperations {
            movable: capabilities.movable,
            resizable: capabilities.resizable && !fullscreen,
            minimizable: capabilities.minimizable,
            shadeable: self.policy.decorations.titlebar && !fullscreen,
            maximizable: capabilities.maximizable && !fullscreen,
            fullscreenable: capabilities.fullscreenable,
            decoratable: ClientPolicy::for_role(self.policy.role)
                .decorations
                .is_present()
                && !fullscreen,
            workspace_movable,
            closable: capabilities.closable,
            above,
            below,
        }
    }

    /// Resolves the effective stacking layer from role and requested state.
    #[must_use]
    pub const fn stacking_layer(self) -> StackingLayer {
        if self.fullscreen.is_some() {
            return StackingLayer::Fullscreen;
        }
        self.base_stacking_layer()
    }

    const fn base_stacking_layer(self) -> StackingLayer {
        match self.policy.role {
            ClientRole::Desktop => StackingLayer::Desktop,
            ClientRole::Dock if matches!(self.layer, ClientLayer::Normal) => StackingLayer::Dock,
            _ => match self.layer {
                ClientLayer::Below => StackingLayer::Below,
                ClientLayer::Normal => StackingLayer::Normal,
                ClientLayer::Above => StackingLayer::Above,
            },
        }
    }
}

/// ICCCM window gravity, expressed without X11 protocol types.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Gravity {
    /// Keep the north-west corner fixed.
    #[default]
    NorthWest,
    /// Keep the north edge centered.
    North,
    /// Keep the north-east corner fixed.
    NorthEast,
    /// Keep the west edge centered.
    West,
    /// Keep the center fixed.
    Center,
    /// Keep the east edge centered.
    East,
    /// Keep the south-west corner fixed.
    SouthWest,
    /// Keep the south edge centered.
    South,
    /// Keep the south-east corner fixed.
    SouthEast,
    /// Keep client coordinates fixed independently of decorations.
    Static,
    /// No useful gravity was specified.
    Forget,
}

impl Gravity {
    /// Adjusts omitted coordinates so the requested anchor remains stationary.
    #[must_use]
    pub fn adjust_resize(
        self,
        geometry: Geometry,
        new_size: Size,
        x_was_requested: bool,
        y_was_requested: bool,
    ) -> (i32, i32) {
        let width_delta = i64::from(new_size.width) - i64::from(geometry.width);
        let height_delta = i64::from(new_size.height) - i64::from(geometry.height);
        let horizontal_divisor = match self {
            Self::North | Self::Center | Self::South => Some(2),
            Self::NorthEast | Self::East | Self::SouthEast => Some(1),
            _ => None,
        };
        let vertical_divisor = match self {
            Self::West | Self::Center | Self::East => Some(2),
            Self::SouthWest | Self::South | Self::SouthEast => Some(1),
            _ => None,
        };
        let x = if x_was_requested {
            geometry.x
        } else {
            adjust_coordinate(geometry.x, width_delta, horizontal_divisor)
        };
        let y = if y_was_requested {
            geometry.y
        } else {
            adjust_coordinate(geometry.y, height_delta, vertical_divisor)
        };
        (x, y)
    }
}

fn adjust_coordinate(coordinate: i32, delta: i64, divisor: Option<i64>) -> i32 {
    let Some(divisor) = divisor else {
        return coordinate;
    };
    let adjusted = i64::from(coordinate).saturating_sub(delta / divisor);
    i32::try_from(adjusted).unwrap_or(if adjusted.is_negative() {
        i32::MIN
    } else {
        i32::MAX
    })
}

/// Ordered set of managed clients and focus history.
#[derive(Debug)]
pub struct ClientSet {
    clients: BTreeMap<ClientId, Client>,
    management_order: Vec<ClientId>,
    stacking: Vec<ClientId>,
    workspace_count: u32,
    current_workspace: WorkspaceId,
    last_workspace: WorkspaceId,
    workspace_layout: WorkspaceLayout,
    focus_order: BTreeMap<WorkspaceId, Vec<ClientId>>,
    focused: Option<ClientId>,
    showing_desktop: bool,
}

impl Default for ClientSet {
    fn default() -> Self {
        Self {
            clients: BTreeMap::new(),
            management_order: Vec::new(),
            stacking: Vec::new(),
            workspace_count: 1,
            current_workspace: WorkspaceId::default(),
            last_workspace: WorkspaceId::default(),
            workspace_layout: WorkspaceLayout::one_row(1),
            focus_order: BTreeMap::new(),
            focused: None,
            showing_desktop: false,
        }
    }
}

const fn remap_removed_workspace(
    assigned: WorkspaceId,
    removed: WorkspaceId,
    remaining_count: u32,
) -> WorkspaceId {
    let index = assigned.index();
    if index > removed.index() {
        WorkspaceId::new(index - 1)
    } else if index >= remaining_count {
        WorkspaceId::new(remaining_count - 1)
    } else {
        assigned
    }
}

impl ClientSet {
    /// Adds a client, or refreshes its geometry if it is already managed.
    ///
    /// Returns `true` only when a new client was added.
    pub fn manage(&mut self, client: Client) -> bool {
        let mut client = client;
        client.workspace = self.valid_assignment(client.workspace);
        if client.decoration_override == DecorationOverride::Default {
            client.natural_decorations = client.policy.decorations;
        }
        client.policy = policy_with_decoration_override(
            client.policy,
            client.natural_decorations,
            client.decoration_override,
        );
        if let Some(existing) = self.clients.get_mut(&client.id) {
            let previous_workspace = existing.workspace;
            *existing = client;
            if previous_workspace != client.workspace {
                self.record_workspace_membership(client.id, client.workspace);
                self.recover_focus();
            }
            return false;
        }

        self.management_order.push(client.id);
        self.stacking.push(client.id);
        let history_workspace = match client.workspace {
            WorkspaceAssignment::Workspace(workspace) => workspace,
            WorkspaceAssignment::All => self.current_workspace,
        };
        self.focus_order
            .entry(history_workspace)
            .or_default()
            .push(client.id);
        self.clients.insert(client.id, client);
        true
    }

    /// Removes a client and all references to it.
    pub fn unmanage(&mut self, id: ClientId) -> bool {
        if self.clients.remove(&id).is_none() {
            return false;
        }
        self.management_order.retain(|candidate| *candidate != id);
        self.stacking.retain(|candidate| *candidate != id);
        for history in self.focus_order.values_mut() {
            history.retain(|candidate| *candidate != id);
        }
        if self.focused == Some(id) {
            self.recover_focus();
        }
        for client in self.clients.values_mut() {
            if client.transient_for == Some(TransientTarget::Client(id)) {
                client.transient_for = None;
            }
        }
        true
    }

    /// Marks a managed client focused and most recently used.
    pub fn focus(&mut self, id: ClientId) -> bool {
        if self.clients.get(&id).is_none_or(|client| {
            client.iconic
                || !client.policy.capabilities.focusable
                || !self.is_visible_client(client)
        }) {
            return false;
        }
        let history = self.focus_order.entry(self.current_workspace).or_default();
        history.retain(|candidate| *candidate != id);
        history.push(id);
        self.focused = Some(id);
        true
    }

    /// Moves a managed client to the least-recent end of this workspace's focus history.
    ///
    /// Current focus is intentionally unchanged. This supports ordered action
    /// sequences that demote a client before selecting a fallback target.
    pub fn focus_to_bottom(&mut self, id: ClientId) -> bool {
        if !self.clients.contains_key(&id) {
            return false;
        }
        let history = self.focus_order.entry(self.current_workspace).or_default();
        let previous = history.iter().position(|candidate| *candidate == id);
        history.retain(|candidate| *candidate != id);
        history.insert(0, id);
        previous != Some(0)
    }

    /// Replaces current focus with the most recent valid target other than `old`.
    ///
    /// Shaded, iconic, hidden, ordinary task-list-skipped, and non-focusable
    /// clients are excluded. Modal, urgent, and dialog targets retain Openbox's
    /// skip-taskbar exception. Redirects are resolved before de-duplication.
    /// When no fallback exists, focus is cleared.
    pub fn focus_fallback_from(&mut self, old: ClientId) -> Option<ClientId> {
        if self.focused != Some(old) {
            return self.focused;
        }
        let history = self
            .focus_order
            .get(&self.current_workspace)
            .into_iter()
            .flatten()
            .rev()
            .copied();
        let fallback = self.stacking.iter().rev().copied();
        let mut seen = std::collections::BTreeSet::new();
        let target = history
            .chain(fallback)
            .filter_map(|requested| self.focus_target(requested))
            .find(|target| {
                *target != old
                    && seen.insert(*target)
                    && self.clients.get(target).is_some_and(|client| {
                        !client.shaded && self.is_automatic_focus_candidate(client)
                    })
            });
        self.focused = None;
        if let Some(target) = target {
            let _ = self.focus(target);
        }
        self.focused
    }

    /// Returns the number of configured workspaces.
    #[must_use]
    pub const fn workspace_count(&self) -> u32 {
        self.workspace_count
    }

    /// Returns the active workspace.
    #[must_use]
    pub const fn current_workspace(&self) -> WorkspaceId {
        self.current_workspace
    }

    /// Returns the previously active workspace.
    #[must_use]
    pub const fn last_workspace(&self) -> WorkspaceId {
        self.last_workspace
    }

    /// Returns whether ordinary clients are temporarily hidden to expose the desktop.
    #[must_use]
    pub const fn showing_desktop(&self) -> bool {
        self.showing_desktop
    }

    /// Enters or leaves desktop-showing mode without changing iconic state.
    pub fn set_showing_desktop(&mut self, showing: bool) -> bool {
        if self.showing_desktop == showing {
            return false;
        }
        self.showing_desktop = showing;
        self.recover_focus();
        true
    }

    /// Resolves an adjacent workspace using wraparound navigation.
    #[must_use]
    pub const fn workspace_in_direction(&self, direction: WorkspaceDirection) -> WorkspaceId {
        match direction {
            WorkspaceDirection::Previous if self.current_workspace.index() == 0 => {
                WorkspaceId::new(self.workspace_count - 1)
            }
            WorkspaceDirection::Previous => WorkspaceId::new(self.current_workspace.index() - 1),
            WorkspaceDirection::Next
                if self.current_workspace.index() + 1 == self.workspace_count =>
            {
                WorkspaceId::new(0)
            }
            WorkspaceDirection::Next => WorkspaceId::new(self.current_workspace.index() + 1),
            WorkspaceDirection::Left
            | WorkspaceDirection::Right
            | WorkspaceDirection::Up
            | WorkspaceDirection::Down => self.current_workspace,
        }
    }

    /// Returns the active workspace's neighbor in the configured grid.
    #[must_use]
    pub fn workspace_in_grid_direction(
        &self,
        direction: WorkspaceDirection,
        wrap: bool,
    ) -> WorkspaceId {
        self.workspace_layout
            .neighbor(self.current_workspace, direction, wrap)
    }

    /// Replaces the workspace grid when it describes this workspace set.
    pub fn set_workspace_layout(&mut self, layout: WorkspaceLayout) -> bool {
        if layout.count != self.workspace_count || layout == self.workspace_layout {
            return false;
        }
        self.workspace_layout = layout;
        true
    }

    /// Reconfigures the workspace set, preserving clients and valid histories.
    ///
    /// A count of zero is normalized to one. Clients on removed workspaces move
    /// to the final surviving workspace.
    pub fn set_workspace_count(&mut self, count: u32) -> bool {
        let count = count.max(1);
        if count == self.workspace_count {
            return false;
        }
        self.workspace_count = count;
        self.workspace_layout = WorkspaceLayout::one_row(count);
        let last = WorkspaceId::new(count - 1);
        if self.current_workspace.index() >= count {
            self.current_workspace = last;
        }
        if self.last_workspace.index() >= count {
            self.last_workspace = last;
        }
        self.focus_order
            .retain(|workspace, _| workspace.index() < count);
        let moved = self
            .clients
            .iter_mut()
            .filter_map(|(id, client)| match client.workspace {
                WorkspaceAssignment::Workspace(workspace) if workspace.index() >= count => {
                    client.workspace = WorkspaceAssignment::Workspace(last);
                    Some(*id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for id in moved {
            self.remove_from_focus_history(id);
            self.focus_order.entry(last).or_default().push(id);
        }
        self.recover_focus();
        true
    }

    /// Inserts an empty workspace at a zero-based index.
    ///
    /// Existing clients and focus histories at or after the insertion point
    /// shift right. Inserting at the current workspace leaves the numeric
    /// current index unchanged, making the new empty workspace visible.
    pub fn insert_workspace(&mut self, workspace: WorkspaceId) -> bool {
        let index = workspace.index();
        let Some(count) = self.workspace_count.checked_add(1) else {
            return false;
        };
        if index > self.workspace_count {
            return false;
        }

        for client in self.clients.values_mut() {
            if let WorkspaceAssignment::Workspace(assigned) = &mut client.workspace
                && assigned.index() >= index
            {
                *assigned = WorkspaceId::new(assigned.index() + 1);
            }
        }
        let histories = std::mem::take(&mut self.focus_order);
        for (assigned, history) in histories {
            let shifted = if assigned.index() >= index {
                WorkspaceId::new(assigned.index() + 1)
            } else {
                assigned
            };
            self.focus_order.insert(shifted, history);
        }
        if self.current_workspace.index() > index {
            self.current_workspace = WorkspaceId::new(self.current_workspace.index() + 1);
        }
        if self.last_workspace.index() >= index {
            self.last_workspace = WorkspaceId::new(self.last_workspace.index() + 1);
        }
        self.workspace_count = count;
        self.workspace_layout = WorkspaceLayout::one_row(count);
        self.recover_focus();
        true
    }

    /// Removes and merges a workspace at a zero-based index.
    ///
    /// A non-final workspace merges into the workspace that follows it; the
    /// final workspace merges into its predecessor. At least one workspace is
    /// always retained.
    pub fn remove_workspace(&mut self, workspace: WorkspaceId) -> bool {
        let index = workspace.index();
        if self.workspace_count <= 1 || index >= self.workspace_count {
            return false;
        }
        let count = self.workspace_count - 1;
        for client in self.clients.values_mut() {
            if let WorkspaceAssignment::Workspace(assigned) = &mut client.workspace {
                *assigned = remap_removed_workspace(*assigned, workspace, count);
            }
        }
        let histories = std::mem::take(&mut self.focus_order);
        for (assigned, history) in histories {
            let shifted = remap_removed_workspace(assigned, workspace, count);
            let merged = self.focus_order.entry(shifted).or_default();
            for id in history {
                if !merged.contains(&id) {
                    merged.push(id);
                }
            }
        }
        self.current_workspace = remap_removed_workspace(self.current_workspace, workspace, count);
        self.last_workspace = remap_removed_workspace(self.last_workspace, workspace, count);
        self.workspace_count = count;
        self.workspace_layout = WorkspaceLayout::one_row(count);
        self.recover_focus();
        true
    }

    /// Switches to a valid workspace and restores its most recent focus.
    pub fn switch_workspace(&mut self, workspace: WorkspaceId) -> bool {
        if workspace.index() >= self.workspace_count || workspace == self.current_workspace {
            return false;
        }
        self.last_workspace = self.current_workspace;
        self.current_workspace = workspace;
        self.recover_focus();
        true
    }

    /// Changes a client's workspace membership.
    pub fn assign_workspace(&mut self, id: ClientId, assignment: WorkspaceAssignment) -> bool {
        let assignment = self.valid_assignment(assignment);
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        if client.workspace == assignment {
            return false;
        }
        client.workspace = assignment;
        self.record_workspace_membership(id, assignment);
        self.recover_focus();
        true
    }

    /// Moves a client and its Openbox-compatible transient family as one unit.
    ///
    /// Specific descendants follow their top parent. A group transient follows
    /// an ordinary member of its application group, while moving that group
    /// transient directly moves only its own specific-descendant branch.
    ///
    /// Returns the identifiers whose assignments changed.
    pub fn assign_workspace_family(
        &mut self,
        id: ClientId,
        assignment: WorkspaceAssignment,
    ) -> Vec<ClientId> {
        let assignment = self.valid_assignment(assignment);
        let Some(root) = self.family_root(id) else {
            return Vec::new();
        };
        let family = self.transient_descendants(root);
        let mut changed = Vec::with_capacity(family.len());
        for member in family {
            if self.clients.get_mut(&member).is_some_and(|client| {
                if client.workspace == assignment {
                    false
                } else {
                    client.workspace = assignment;
                    true
                }
            }) {
                self.record_workspace_membership(member, assignment);
                changed.push(member);
            }
        }
        if !changed.is_empty() {
            self.recover_focus();
        }
        changed
    }

    /// Returns whether a managed client is on the active workspace.
    #[must_use]
    pub fn is_visible(&self, id: ClientId) -> bool {
        self.clients
            .get(&id)
            .is_some_and(|client| self.is_visible_client(client))
    }

    /// Marks a managed client highest in the stacking order.
    pub fn raise(&mut self, id: ClientId) -> bool {
        if !self.clients.contains_key(&id) {
            return false;
        }
        if let Some(position) = self.stacking.iter().position(|candidate| *candidate == id) {
            self.stacking[position..].rotate_left(1);
        } else {
            self.stacking.push(id);
        }
        true
    }

    /// Marks a managed client lowest in its effective policy layer.
    pub fn lower(&mut self, id: ClientId) -> bool {
        if !self.clients.contains_key(&id) {
            return false;
        }
        if let Some(position) = self.stacking.iter().position(|candidate| *candidate == id) {
            self.stacking[..=position].rotate_right(1);
        } else {
            self.stacking.insert(0, id);
        }
        true
    }

    /// Replaces stacking order with a backend-observed bottom-to-top order.
    ///
    /// Unknown and duplicate identifiers are discarded. Managed clients absent
    /// from `order` retain their previous relative order at the bottom.
    pub fn sync_stacking(&mut self, order: impl IntoIterator<Item = ClientId>) {
        let mut observed = Vec::new();
        for id in order {
            if self.clients.contains_key(&id) && !observed.contains(&id) {
                observed.push(id);
            }
        }
        let mut stacking = Vec::with_capacity(self.stacking.len());
        stacking.extend(
            self.stacking
                .iter()
                .copied()
                .filter(|id| !observed.contains(id)),
        );
        stacking.extend(observed);
        self.stacking = stacking;
    }

    /// Updates a managed client's geometry.
    pub fn set_geometry(&mut self, id: ClientId, geometry: Geometry) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        client.geometry = geometry;
        if let Some(maximize) = client.maximize.as_mut() {
            if !maximize.horizontal {
                maximize.restore.x = geometry.x;
                maximize.restore.width = geometry.width;
            }
            if !maximize.vertical {
                maximize.restore.y = geometry.y;
                maximize.restore.height = geometry.height;
            }
        }
        true
    }

    /// Updates exact output coverage discovered by a display backend.
    pub fn set_output_coverage(&mut self, id: ClientId, coverage: Option<OutputCoverage>) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        if client.output_coverage == coverage {
            return false;
        }
        client.output_coverage = coverage;
        true
    }

    /// Changes maximize axes and returns geometry to apply when state changed.
    pub fn set_maximized(
        &mut self,
        id: ClientId,
        horizontal: bool,
        vertical: bool,
        available: Geometry,
    ) -> Option<Geometry> {
        let client = self.clients.get_mut(&id)?;
        if (horizontal || vertical) && !client.policy.capabilities.maximizable {
            return None;
        }
        if let Some(fullscreen) = client.fullscreen {
            let visible = client.geometry;
            client.geometry = fullscreen.restore;
            let _ = update_maximized_geometry(client, horizontal, vertical, available);
            let restore = client.geometry;
            client.geometry = visible;
            client.fullscreen = Some(FullscreenState { restore });
            return None;
        }
        update_maximized_geometry(client, horizontal, vertical, available)
    }

    /// Changes fullscreen state and returns geometry to apply when it changed.
    pub fn set_fullscreen(
        &mut self,
        id: ClientId,
        fullscreen: bool,
        output: Geometry,
    ) -> Option<Geometry> {
        let client = self.clients.get_mut(&id)?;
        if fullscreen && !client.policy.capabilities.fullscreenable {
            return None;
        }
        match (client.fullscreen, fullscreen) {
            (None, false) => None,
            (Some(_), true) if client.geometry == output => None,
            (Some(state), true) => {
                client.geometry = output;
                client.fullscreen = Some(state);
                Some(output)
            }
            (None, true) => {
                client.fullscreen = Some(FullscreenState {
                    restore: client.geometry,
                });
                client.geometry = output;
                Some(output)
            }
            (Some(state), false) => {
                client.fullscreen = None;
                client.geometry = state.restore;
                Some(state.restore)
            }
        }
    }

    /// Updates a managed client's requested stacking layer.
    pub fn set_layer(&mut self, id: ClientId, layer: ClientLayer) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        if client.layer == layer {
            return false;
        }
        client.layer = layer;
        true
    }

    /// Updates size constraints for a managed client.
    pub fn set_size_hints(&mut self, id: ClientId, size_hints: SizeHints) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        client.size_hints = size_hints;
        true
    }

    /// Updates window gravity for a managed client.
    pub fn set_gravity(&mut self, id: ClientId, gravity: Gravity) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        client.gravity = gravity;
        true
    }

    /// Updates a managed client's functional policy.
    pub fn set_policy(&mut self, id: ClientId, policy: ClientPolicy) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        let effective =
            policy_with_decoration_override(policy, policy.decorations, client.decoration_override);
        if client.natural_decorations == policy.decorations && client.policy == effective {
            return false;
        }
        client.natural_decorations = policy.decorations;
        client.policy = effective;
        true
    }

    /// Toggles a reversible user override for server-side decorations.
    ///
    /// Returns the effective policy when the client supports decorations.
    pub fn toggle_decorations(&mut self, id: ClientId) -> Option<ClientPolicy> {
        let client = self.clients.get_mut(&id)?;
        if !ClientPolicy::for_role(client.policy.role)
            .decorations
            .is_present()
            || client.fullscreen.is_some()
        {
            return None;
        }
        client.decoration_override = match client.decoration_override {
            DecorationOverride::Default if client.policy.decorations.is_present() => {
                DecorationOverride::Undecorated
            }
            DecorationOverride::Default => DecorationOverride::Decorated,
            DecorationOverride::Decorated | DecorationOverride::Undecorated => {
                DecorationOverride::Default
            }
        };
        client.policy = policy_with_decoration_override(
            client.policy,
            client.natural_decorations,
            client.decoration_override,
        );
        Some(client.policy)
    }

    /// Applies an explicit user decoration preference.
    ///
    /// [`DecorationOverride::Default`] restores the client's live hints and
    /// configured application policy. Fullscreen clients and roles that can
    /// never carry decorations reject the request. The effective policy is
    /// returned only when the preference changed.
    pub fn set_decoration_override(
        &mut self,
        id: ClientId,
        preference: DecorationOverride,
    ) -> Option<ClientPolicy> {
        let client = self.clients.get_mut(&id)?;
        if !ClientPolicy::for_role(client.policy.role)
            .decorations
            .is_present()
            || client.fullscreen.is_some()
            || client.decoration_override == preference
        {
            return None;
        }
        client.decoration_override = preference;
        client.policy = policy_with_decoration_override(
            client.policy,
            client.natural_decorations,
            client.decoration_override,
        );
        Some(client.policy)
    }

    /// Replaces task-list, pager, and attention presentation hints.
    pub fn set_presentation(&mut self, id: ClientId, presentation: ClientPresentation) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        if client.presentation == presentation {
            return false;
        }
        client.presentation = presentation;
        true
    }

    /// Updates transient, group, and modal relationships for a managed client.
    pub fn set_relationships(
        &mut self,
        id: ClientId,
        transient_for: Option<TransientTarget>,
        group: Option<ClientId>,
        modal: bool,
    ) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        let transient_for = transient_for
            .filter(|target| !matches!(target, TransientTarget::Client(parent) if *parent == id));
        if (client.transient_for, client.group, client.modal) == (transient_for, group, modal) {
            return false;
        }
        client.transient_for = transient_for;
        client.group = group;
        client.modal = modal;
        true
    }

    /// Updates only the modal state of a managed client.
    pub fn set_modal(&mut self, id: ClientId, modal: bool) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        client.modal = modal;
        true
    }

    /// Updates whether a managed client is iconified.
    pub fn set_iconic(&mut self, id: ClientId, iconic: bool) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        client.iconic = iconic;
        if iconic && self.focused == Some(id) {
            self.recover_focus();
        }
        true
    }

    /// Updates whether a titlebar-bearing, non-fullscreen client is shaded.
    pub fn set_shaded(&mut self, id: ClientId, shaded: bool) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        if shaded && (!client.policy.decorations.titlebar || client.fullscreen.is_some()) {
            return false;
        }
        if client.shaded == shaded {
            return false;
        }
        client.shaded = shaded;
        true
    }

    /// Resolves a focus request through the topmost modal transient chain.
    #[must_use]
    pub fn focus_target(&self, requested: ClientId) -> Option<ClientId> {
        if self
            .clients
            .get(&requested)
            .is_none_or(|client| client.iconic || !self.is_visible_client(client))
        {
            return None;
        }
        let mut target = requested;
        let mut visited = Vec::new();
        while !visited.contains(&target) {
            visited.push(target);
            let target_group = self.clients.get(&target).and_then(|client| client.group);
            let modal = self.stacking.iter().rev().copied().find(|candidate| {
                !visited.contains(candidate)
                    && self.clients.get(candidate).is_some_and(|client| {
                        client.modal
                            && !client.iconic
                            && self.is_visible_client(client)
                            && match client.transient_for {
                                Some(TransientTarget::Client(parent)) => parent == target,
                                Some(TransientTarget::Group) => {
                                    client.group.is_some() && client.group == target_group
                                }
                                None => false,
                            }
                    })
            });
            let Some(modal) = modal else {
                break;
            };
            target = modal;
        }
        Some(target)
    }

    /// Returns whether two clients belong to the same specific-transient or
    /// application-group family for focus policy.
    #[must_use]
    pub fn clients_are_related(&self, left: ClientId, right: ClientId) -> bool {
        if left == right {
            return self.clients.contains_key(&left);
        }
        let (Some(left_client), Some(right_client)) =
            (self.clients.get(&left), self.clients.get(&right))
        else {
            return false;
        };
        if left_client.group.is_some() && left_client.group == right_client.group {
            return true;
        }
        self.clients_share_transient_family(left, right)
    }

    /// Returns whether two clients share one specific-transient tree.
    #[must_use]
    pub fn clients_share_transient_family(&self, left: ClientId, right: ClientId) -> bool {
        if left == right {
            return self.clients.contains_key(&left);
        }
        self.family_root(left).is_some_and(|left_root| {
            self.family_root(right)
                .is_some_and(|right_root| left_root == right_root)
        })
    }

    /// Returns a managed client.
    #[must_use]
    pub fn get(&self, id: ClientId) -> Option<&Client> {
        self.clients.get(&id)
    }

    /// Returns whether an identifier is managed.
    #[must_use]
    pub fn contains(&self, id: ClientId) -> bool {
        self.clients.contains_key(&id)
    }

    /// Returns the focused client.
    #[must_use]
    pub const fn focused(&self) -> Option<ClientId> {
        self.focused
    }

    /// Returns visible focus targets in most-recently-used order.
    ///
    /// Modal redirects are resolved before de-duplication, so a blocked parent
    /// and its active modal appear as one cycle candidate.
    #[must_use]
    pub fn focus_cycle_candidates(&self) -> Vec<ClientId> {
        let history = self
            .focus_order
            .get(&self.current_workspace)
            .into_iter()
            .flatten()
            .rev()
            .copied();
        let fallback = self.stacking.iter().rev().copied();
        let mut seen = std::collections::BTreeSet::new();
        self.focused
            .into_iter()
            .chain(history)
            .chain(fallback)
            .filter_map(|requested| self.focus_target(requested))
            .filter(|target| {
                self.clients.get(target).is_some_and(|client| {
                    self.is_automatic_focus_candidate(client) && seen.insert(*target)
                })
            })
            .collect()
    }

    /// Clears focus without changing focus history.
    pub fn clear_focus(&mut self) {
        self.focused = None;
    }

    /// Iterates from bottom to top of the stacking order.
    pub fn stacking(&self) -> impl ExactSizeIterator<Item = ClientId> + '_ {
        self.stacking.iter().copied()
    }

    /// Returns bottom-to-top policy order with parents below specific transients.
    #[must_use]
    pub fn policy_stacking(&self, outputs: &OutputSet) -> Vec<ClientId> {
        let layers = self.effective_layer_table(outputs);
        let mut ordered = Vec::with_capacity(self.stacking.len());
        let mut visited = Vec::with_capacity(self.stacking.len());
        for layer in [
            StackingLayer::Desktop,
            StackingLayer::Below,
            StackingLayer::Normal,
            StackingLayer::Dock,
            StackingLayer::Above,
            StackingLayer::Fullscreen,
        ] {
            for id in self.stacking.iter().copied() {
                if self.memoized_layer(&layers, id, outputs) == Some(layer) {
                    self.visit_stacking_parent(
                        id,
                        layer,
                        outputs,
                        &layers,
                        &mut visited,
                        &mut ordered,
                    );
                }
            }
        }
        ordered
    }

    /// Computes every stacked client's effective layer once, sorted by id.
    fn effective_layer_table(&self, outputs: &OutputSet) -> Vec<(ClientId, Option<StackingLayer>)> {
        let mut layers: Vec<(ClientId, Option<StackingLayer>)> = self
            .stacking
            .iter()
            .map(|id| (*id, self.effective_stacking_layer(*id, outputs)))
            .collect();
        layers.sort_unstable_by_key(|entry| entry.0);
        layers
    }

    fn memoized_layer(
        &self,
        layers: &[(ClientId, Option<StackingLayer>)],
        id: ClientId,
        outputs: &OutputSet,
    ) -> Option<StackingLayer> {
        layers
            .binary_search_by_key(&id, |entry| entry.0)
            .ok()
            .map_or_else(
                || self.effective_stacking_layer(id, outputs),
                |index| layers[index].1,
            )
    }

    /// Resolves a client's layer, inheriting any higher specific-parent layer.
    #[must_use]
    pub fn effective_stacking_layer(
        &self,
        id: ClientId,
        outputs: &OutputSet,
    ) -> Option<StackingLayer> {
        let mut layer = self.client_stacking_layer(id, outputs)?;
        let mut current = id;
        // The iteration bound replaces a visited set: an acyclic chain has at
        // most `len` distinct parents, and in a pathological transient cycle
        // the repeated `max` over the same members is idempotent.
        for _ in 0..=self.clients.len() {
            let Some(TransientTarget::Client(parent)) = self
                .clients
                .get(&current)
                .and_then(|client| client.transient_for)
            else {
                break;
            };
            let Some(parent) = self.clients.get(&parent) else {
                break;
            };
            layer = layer.max(self.client_stacking_layer_of(parent, outputs));
            current = parent.id;
        }
        Some(layer)
    }

    /// Iterates in the order clients first became managed.
    pub fn management_order(&self) -> impl ExactSizeIterator<Item = ClientId> + '_ {
        self.management_order.iter().copied()
    }

    /// Returns the number of managed clients.
    #[must_use]
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    /// Returns whether no clients are managed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    fn valid_assignment(&self, assignment: WorkspaceAssignment) -> WorkspaceAssignment {
        match assignment {
            WorkspaceAssignment::Workspace(workspace)
                if workspace.index() >= self.workspace_count =>
            {
                WorkspaceAssignment::Workspace(self.current_workspace)
            }
            valid => valid,
        }
    }

    fn is_visible_client(&self, client: &Client) -> bool {
        client.workspace.is_visible_on(self.current_workspace)
            && (!self.showing_desktop
                || matches!(client.policy.role, ClientRole::Desktop | ClientRole::Dock))
    }

    fn is_automatic_focus_candidate(&self, client: &Client) -> bool {
        !client.iconic
            && client.policy.capabilities.focusable
            && self.is_visible_client(client)
            && (!client.presentation.skip_taskbar
                || client.modal
                || client.presentation.urgent
                || client.policy.role == ClientRole::Dialog)
    }

    fn record_workspace_membership(&mut self, id: ClientId, assignment: WorkspaceAssignment) {
        self.remove_from_focus_history(id);
        let workspace = match assignment {
            WorkspaceAssignment::Workspace(workspace) => workspace,
            WorkspaceAssignment::All => self.current_workspace,
        };
        self.focus_order.entry(workspace).or_default().push(id);
    }

    fn remove_from_focus_history(&mut self, id: ClientId) {
        for history in self.focus_order.values_mut() {
            history.retain(|candidate| *candidate != id);
        }
    }

    fn recover_focus(&mut self) {
        let focusable = |candidate: &ClientId| {
            self.clients.get(candidate).is_some_and(|client| {
                !client.iconic
                    && client.policy.capabilities.focusable
                    && self.is_visible_client(client)
            })
        };
        self.focused = self
            .focus_order
            .get(&self.current_workspace)
            .into_iter()
            .flatten()
            .rev()
            .copied()
            .find(&focusable)
            .or_else(|| self.stacking.iter().rev().copied().find(&focusable));
    }

    fn family_root(&self, id: ClientId) -> Option<ClientId> {
        let client = self.clients.get(&id)?;
        // Fast path: most clients are not specific transients at all.
        let Some(TransientTarget::Client(first_parent)) = client.transient_for else {
            return Some(id);
        };
        if !self.clients.contains_key(&first_parent) {
            return Some(id);
        }
        let mut root = first_parent;
        let mut visited = vec![id];
        while !visited.contains(&root) {
            visited.push(root);
            let Some(TransientTarget::Client(parent)) = self
                .clients
                .get(&root)
                .and_then(|client| client.transient_for)
            else {
                break;
            };
            if !self.clients.contains_key(&parent) {
                break;
            }
            root = parent;
        }
        Some(root)
    }

    fn transient_descendants(&self, root: ClientId) -> Vec<ClientId> {
        let mut family = Vec::new();
        let mut pending = vec![root];
        if let Some(root_client) = self.clients.get(&root)
            && root_client.transient_for != Some(TransientTarget::Group)
            && !matches!(
                root_client.policy.role,
                ClientRole::Desktop | ClientRole::Dock | ClientRole::Splash
            )
            && let Some(group) = root_client.group
        {
            pending.extend(self.management_order.iter().copied().filter(|candidate| {
                self.clients.get(candidate).is_some_and(|client| {
                    client.group == Some(group)
                        && client.transient_for == Some(TransientTarget::Group)
                })
            }));
        }
        let mut seen = std::collections::BTreeSet::new();
        while let Some(parent) = pending.pop() {
            if !seen.insert(parent) {
                continue;
            }
            family.push(parent);
            pending.extend(self.management_order.iter().copied().filter(|candidate| {
                self.clients.get(candidate).is_some_and(|client| {
                    client.transient_for == Some(TransientTarget::Client(parent))
                })
            }));
        }
        family
    }

    fn visit_stacking_parent(
        &self,
        id: ClientId,
        layer: StackingLayer,
        outputs: &OutputSet,
        layers: &[(ClientId, Option<StackingLayer>)],
        visited: &mut Vec<ClientId>,
        ordered: &mut Vec<ClientId>,
    ) {
        if visited.contains(&id) {
            return;
        }
        visited.push(id);
        if let Some(client) = self.clients.get(&id)
            && client.transient_for == Some(TransientTarget::Group)
            && let Some(group) = client.group
        {
            for member in self.stacking.iter().copied().filter(|candidate| {
                *candidate != id
                    && self.clients.get(candidate).is_some_and(|candidate| {
                        candidate.group == Some(group)
                            && candidate.transient_for != Some(TransientTarget::Group)
                            && !matches!(
                                candidate.policy.role,
                                ClientRole::Desktop | ClientRole::Dock | ClientRole::Splash
                            )
                    })
                    && !self.is_self_or_specific_descendant(id, *candidate)
                    && self.memoized_layer(layers, *candidate, outputs) == Some(layer)
            }) {
                self.visit_stacking_parent(member, layer, outputs, layers, visited, ordered);
            }
        }
        if let Some(TransientTarget::Client(parent)) = self
            .clients
            .get(&id)
            .and_then(|client| client.transient_for)
            && self.memoized_layer(layers, parent, outputs) == Some(layer)
        {
            self.visit_stacking_parent(parent, layer, outputs, layers, visited, ordered);
        }
        ordered.push(id);
    }

    fn client_stacking_layer(&self, id: ClientId, outputs: &OutputSet) -> Option<StackingLayer> {
        Some(self.client_stacking_layer_of(self.clients.get(&id)?, outputs))
    }

    fn client_stacking_layer_of(&self, client: &Client, outputs: &OutputSet) -> StackingLayer {
        if client.fullscreen.is_some() {
            if self.fullscreen_layer_is_active(client, outputs) {
                StackingLayer::Fullscreen
            } else {
                client.base_stacking_layer()
            }
        } else if client.maximize.is_none()
            && !matches!(client.policy.role, ClientRole::Desktop | ClientRole::Dock)
            && client.output_coverage.is_some()
            && self.output_coverage_is_active(client, outputs)
        {
            StackingLayer::Fullscreen
        } else {
            client.stacking_layer()
        }
    }

    fn output_coverage_is_active(&self, client: &Client, outputs: &OutputSet) -> bool {
        let Some(coverage) = client.output_coverage else {
            return false;
        };
        let Some(focused) = self.focused else {
            return true;
        };
        if self.is_self_or_specific_descendant(client.id, focused)
            || !client.workspace.is_visible_on(self.current_workspace)
        {
            return true;
        }
        self.clients
            .get(&focused)
            .is_none_or(|focused| outputs.output_for(focused.geometry).id != coverage.output())
    }

    fn fullscreen_layer_is_active(&self, client: &Client, outputs: &OutputSet) -> bool {
        let Some(focused) = self.focused else {
            return true;
        };
        if self.is_self_or_specific_descendant(client.id, focused)
            || !client.workspace.is_visible_on(self.current_workspace)
        {
            return true;
        }
        self.clients.get(&focused).is_none_or(|focused| {
            outputs.output_for(focused.geometry).id != outputs.output_for(client.geometry).id
        })
    }

    fn is_self_or_specific_descendant(&self, ancestor: ClientId, client: ClientId) -> bool {
        let mut current = client;
        // The iteration bound replaces a visited set: the answer only depends
        // on whether `ancestor` is reachable, so revisiting nodes of a
        // pathological transient cycle cannot change the result.
        for _ in 0..=self.clients.len() {
            if current == ancestor {
                return true;
            }
            let Some(TransientTarget::Client(parent)) = self
                .clients
                .get(&current)
                .and_then(|client| client.transient_for)
            else {
                break;
            };
            current = parent;
        }
        false
    }
}

fn update_maximized_geometry(
    client: &mut Client,
    horizontal: bool,
    vertical: bool,
    available: Geometry,
) -> Option<Geometry> {
    let old_horizontal = client.maximize.is_some_and(|state| state.horizontal);
    let old_vertical = client.maximize.is_some_and(|state| state.vertical);
    let mut restore = client
        .maximize
        .map_or(client.geometry, |state| state.restore);
    if old_horizontal == horizontal && old_vertical == vertical {
        let geometry = Geometry::new(
            if horizontal {
                available.x
            } else {
                client.geometry.x
            },
            if vertical {
                available.y
            } else {
                client.geometry.y
            },
            if horizontal {
                available.width
            } else {
                client.geometry.width
            },
            if vertical {
                available.height
            } else {
                client.geometry.height
            },
        );
        if geometry == client.geometry {
            return None;
        }
        client.geometry = geometry;
        return Some(geometry);
    }
    if horizontal && !old_horizontal {
        restore.x = client.geometry.x;
        restore.width = client.geometry.width;
    }
    if vertical && !old_vertical {
        restore.y = client.geometry.y;
        restore.height = client.geometry.height;
    }
    let geometry = Geometry::new(
        if horizontal {
            available.x
        } else if old_horizontal {
            restore.x
        } else {
            client.geometry.x
        },
        if vertical {
            available.y
        } else if old_vertical {
            restore.y
        } else {
            client.geometry.y
        },
        if horizontal {
            available.width
        } else if old_horizontal {
            restore.width
        } else {
            client.geometry.width
        },
        if vertical {
            available.height
        } else if old_vertical {
            restore.height
        } else {
            client.geometry.height
        },
    );
    client.geometry = geometry;
    client.maximize = (horizontal || vertical).then_some(MaximizeState {
        horizontal,
        vertical,
        restore,
    });
    Some(geometry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(raw: u64) -> Client {
        Client {
            id: ClientId::new(raw),
            geometry: Geometry::new(0, 0, 640, 480),
            size_hints: SizeHints::default(),
            gravity: Gravity::default(),
            policy: ClientPolicy::for_role(ClientRole::Normal),
            natural_decorations: ClientPolicy::for_role(ClientRole::Normal).decorations,
            decoration_override: DecorationOverride::Default,
            presentation: ClientPresentation::default(),
            transient_for: None,
            group: None,
            modal: false,
            iconic: false,
            shaded: false,
            workspace: WorkspaceAssignment::default(),
            layer: ClientLayer::Normal,
            maximize: None,
            fullscreen: None,
            output_coverage: None,
        }
    }

    #[test]
    fn managing_the_same_client_twice_does_not_duplicate_it() {
        let mut clients = ClientSet::default();
        assert!(clients.manage(client(7)));
        assert!(!clients.manage(client(7)));
        assert_eq!(clients.len(), 1);
        assert_eq!(clients.stacking().collect::<Vec<_>>(), [ClientId::new(7)]);
    }

    #[test]
    fn removing_the_focused_client_uses_most_recent_survivor() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.manage(client(2));
        clients.focus(ClientId::new(1));
        clients.focus(ClientId::new(2));

        assert!(clients.unmanage(ClientId::new(2)));
        assert_eq!(clients.focused(), Some(ClientId::new(1)));
    }

    #[test]
    fn showing_desktop_hides_ordinary_clients_without_iconifying_them() {
        let mut clients = ClientSet::default();
        let normal = client(1);
        let mut desktop = client(2);
        desktop.policy = ClientPolicy::for_role(ClientRole::Desktop);
        let mut dock = client(3);
        dock.policy = ClientPolicy::for_role(ClientRole::Dock);
        clients.manage(normal);
        clients.manage(desktop);
        clients.manage(dock);
        clients.focus(ClientId::new(1));

        assert!(clients.set_showing_desktop(true));
        assert!(clients.showing_desktop());
        assert!(!clients.is_visible(ClientId::new(1)));
        assert!(clients.is_visible(ClientId::new(2)));
        assert!(clients.is_visible(ClientId::new(3)));
        assert_eq!(clients.focused(), None);
        assert!(!clients.get(ClientId::new(1)).unwrap().iconic);
        assert!(!clients.set_showing_desktop(true));

        assert!(clients.set_showing_desktop(false));
        assert!(clients.is_visible(ClientId::new(1)));
        assert_eq!(clients.focused(), Some(ClientId::new(1)));
    }

    #[test]
    fn shading_is_limited_to_decorated_nonfullscreen_clients() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        assert!(clients.set_shaded(ClientId::new(1), true));
        assert!(clients.get(ClientId::new(1)).unwrap().shaded);
        assert!(
            clients
                .get(ClientId::new(1))
                .unwrap()
                .operations()
                .shadeable
        );
        assert!(clients.set_shaded(ClientId::new(1), false));

        let mut dock = client(2);
        dock.policy = ClientPolicy::for_role(ClientRole::Dock);
        clients.manage(dock);
        assert!(!clients.set_shaded(ClientId::new(2), true));

        clients.set_fullscreen(ClientId::new(1), true, Geometry::new(0, 0, 800, 600));
        assert!(
            !clients
                .get(ClientId::new(1))
                .unwrap()
                .operations()
                .shadeable
        );
        assert!(!clients.set_shaded(ClientId::new(1), true));
    }

    #[test]
    fn focus_cycle_candidates_are_mru_visible_and_modal_aware() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(2);
        clients.manage(client(1));
        clients.manage(client(2));
        let mut hidden = client(3);
        hidden.workspace = WorkspaceAssignment::Workspace(WorkspaceId::new(1));
        clients.manage(hidden);
        let mut dock = client(4);
        dock.policy = ClientPolicy::for_role(ClientRole::Dock);
        clients.manage(dock);
        clients.focus(ClientId::new(1));
        clients.focus(ClientId::new(2));

        assert_eq!(
            clients.focus_cycle_candidates(),
            [ClientId::new(2), ClientId::new(1)]
        );

        let mut modal = client(5);
        modal.transient_for = Some(TransientTarget::Client(ClientId::new(1)));
        modal.modal = true;
        clients.manage(modal);
        assert_eq!(
            clients.focus_cycle_candidates(),
            [ClientId::new(2), ClientId::new(5)]
        );

        let mut skipped = client(6);
        skipped.presentation.skip_taskbar = true;
        clients.manage(skipped);
        assert_eq!(
            clients.focus_cycle_candidates(),
            [ClientId::new(2), ClientId::new(5)]
        );

        let mut urgent = skipped.presentation;
        urgent.urgent = true;
        clients.set_presentation(ClientId::new(6), urgent);
        assert_eq!(
            clients.focus_cycle_candidates(),
            [ClientId::new(2), ClientId::new(6), ClientId::new(5)],
            "urgent clients retain the Openbox skip-taskbar exception"
        );
    }

    #[test]
    fn focus_order_demotion_and_fallback_filter_invalid_targets() {
        let mut clients = ClientSet::default();
        for id in 1..=3 {
            clients.manage(client(id));
        }
        clients.focus(ClientId::new(1));
        clients.focus(ClientId::new(2));
        clients.focus(ClientId::new(3));

        assert!(clients.focus_to_bottom(ClientId::new(3)));
        assert!(!clients.focus_to_bottom(ClientId::new(3)));
        assert!(!clients.focus_to_bottom(ClientId::new(99)));
        assert_eq!(clients.focused(), Some(ClientId::new(3)));
        clients.focus(ClientId::new(1));
        assert_eq!(
            clients.focus_cycle_candidates(),
            [ClientId::new(1), ClientId::new(2), ClientId::new(3)]
        );

        assert_eq!(
            clients.focus_fallback_from(ClientId::new(99)),
            Some(ClientId::new(1)),
            "a non-focused action target must not change focus"
        );
        assert_eq!(
            clients.focus_fallback_from(ClientId::new(1)),
            Some(ClientId::new(2))
        );
        clients.set_shaded(ClientId::new(1), true);
        clients.set_shaded(ClientId::new(3), true);
        assert_eq!(clients.focus_fallback_from(ClientId::new(2)), None);
        assert_eq!(clients.focused(), None);
    }

    #[test]
    fn presentation_hints_are_replaced_atomically() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        let presentation = ClientPresentation {
            skip_taskbar: true,
            skip_pager: true,
            urgent: true,
        };

        assert!(clients.set_presentation(ClientId::new(1), presentation));
        assert_eq!(
            clients
                .get(ClientId::new(1))
                .map(|client| client.presentation),
            Some(presentation)
        );
        assert!(!clients.set_presentation(ClientId::new(1), presentation));
        assert!(!clients.set_presentation(ClientId::new(99), presentation));
    }

    #[test]
    fn workspace_switching_restores_independent_focus_history() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(3);
        clients.manage(client(1));
        let mut second = client(2);
        second.workspace = WorkspaceAssignment::Workspace(WorkspaceId::new(1));
        clients.manage(second);
        clients.focus(ClientId::new(1));

        assert!(clients.switch_workspace(WorkspaceId::new(1)));
        assert_eq!(clients.focused(), Some(ClientId::new(2)));
        assert!(!clients.is_visible(ClientId::new(1)));
        assert!(clients.is_visible(ClientId::new(2)));

        assert!(clients.switch_workspace(WorkspaceId::new(0)));
        assert_eq!(clients.focused(), Some(ClientId::new(1)));
    }

    #[test]
    fn last_workspace_tracks_and_toggles_the_previous_selection() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(3);

        assert_eq!(clients.last_workspace(), WorkspaceId::new(0));
        assert!(clients.switch_workspace(WorkspaceId::new(2)));
        assert_eq!(clients.last_workspace(), WorkspaceId::new(0));
        assert!(clients.switch_workspace(clients.last_workspace()));
        assert_eq!(clients.current_workspace(), WorkspaceId::new(0));
        assert_eq!(clients.last_workspace(), WorkspaceId::new(2));
        assert!(clients.switch_workspace(clients.last_workspace()));
        assert_eq!(clients.current_workspace(), WorkspaceId::new(2));
        assert_eq!(clients.last_workspace(), WorkspaceId::new(0));
    }

    #[test]
    fn workspace_insertion_and_removal_shift_and_merge_membership() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(3);
        for (id, workspace) in [(1, 0), (2, 1), (3, 2)] {
            let mut managed = client(id);
            managed.workspace = WorkspaceAssignment::Workspace(WorkspaceId::new(workspace));
            clients.manage(managed);
        }
        clients.switch_workspace(WorkspaceId::new(1));

        assert!(clients.insert_workspace(WorkspaceId::new(1)));
        assert_eq!(clients.workspace_count(), 4);
        assert_eq!(clients.current_workspace(), WorkspaceId::new(1));
        assert_eq!(
            clients.focused(),
            None,
            "inserted current workspace is empty"
        );
        assert_eq!(
            clients.get(ClientId::new(2)).unwrap().workspace,
            WorkspaceAssignment::Workspace(WorkspaceId::new(2))
        );
        assert_eq!(
            clients.get(ClientId::new(3)).unwrap().workspace,
            WorkspaceAssignment::Workspace(WorkspaceId::new(3))
        );
        assert!(!clients.insert_workspace(WorkspaceId::new(5)));

        assert!(clients.remove_workspace(WorkspaceId::new(1)));
        assert_eq!(clients.workspace_count(), 3);
        assert_eq!(clients.current_workspace(), WorkspaceId::new(1));
        assert_eq!(clients.focused(), Some(ClientId::new(2)));
        assert_eq!(
            clients.get(ClientId::new(2)).unwrap().workspace,
            WorkspaceAssignment::Workspace(WorkspaceId::new(1))
        );

        assert!(clients.remove_workspace(WorkspaceId::new(2)));
        assert_eq!(clients.workspace_count(), 2);
        assert_eq!(
            clients.get(ClientId::new(3)).unwrap().workspace,
            WorkspaceAssignment::Workspace(WorkspaceId::new(1)),
            "the final workspace merges into its predecessor"
        );
        assert!(!clients.remove_workspace(WorkspaceId::new(2)));
    }

    #[test]
    fn moving_the_focused_client_away_recovers_visible_focus() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(2);
        clients.manage(client(1));
        clients.manage(client(2));
        clients.focus(ClientId::new(1));
        clients.focus(ClientId::new(2));

        assert!(clients.assign_workspace(
            ClientId::new(2),
            WorkspaceAssignment::Workspace(WorkspaceId::new(1)),
        ));
        assert_eq!(clients.focused(), Some(ClientId::new(1)));
        assert!(!clients.focus(ClientId::new(2)));

        clients.switch_workspace(WorkspaceId::new(1));
        assert_eq!(clients.focused(), Some(ClientId::new(2)));
    }

    #[test]
    fn sticky_clients_remain_visible_and_focusable_everywhere() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(2);
        let mut sticky = client(1);
        sticky.workspace = WorkspaceAssignment::All;
        clients.manage(sticky);
        clients.focus(ClientId::new(1));

        clients.switch_workspace(WorkspaceId::new(1));
        assert!(clients.is_visible(ClientId::new(1)));
        assert_eq!(clients.focused(), Some(ClientId::new(1)));
        assert!(clients.focus(ClientId::new(1)));
    }

    #[test]
    fn shrinking_workspaces_moves_clients_to_last_survivor() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(4);
        let mut client = client(1);
        client.workspace = WorkspaceAssignment::Workspace(WorkspaceId::new(3));
        clients.manage(client);
        clients.switch_workspace(WorkspaceId::new(3));

        assert!(clients.set_workspace_count(2));
        assert_eq!(clients.workspace_count(), 2);
        assert_eq!(clients.current_workspace(), WorkspaceId::new(1));
        assert_eq!(
            clients.get(ClientId::new(1)).unwrap().workspace,
            WorkspaceAssignment::Workspace(WorkspaceId::new(1))
        );
        assert_eq!(clients.focused(), Some(ClientId::new(1)));
    }

    #[test]
    fn invalid_workspace_assignments_are_normalized_to_current() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(2);
        clients.switch_workspace(WorkspaceId::new(1));
        let mut invalid = client(1);
        invalid.workspace = WorkspaceAssignment::Workspace(WorkspaceId::new(99));

        clients.manage(invalid);

        assert_eq!(
            clients.get(ClientId::new(1)).unwrap().workspace,
            WorkspaceAssignment::Workspace(WorkspaceId::new(1))
        );
    }

    #[test]
    fn relative_workspace_navigation_wraps_in_both_directions() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(3);

        assert_eq!(
            clients.workspace_in_direction(WorkspaceDirection::Previous),
            WorkspaceId::new(2)
        );
        assert_eq!(
            clients.workspace_in_direction(WorkspaceDirection::Next),
            WorkspaceId::new(1)
        );
        clients.switch_workspace(WorkspaceId::new(2));
        assert_eq!(
            clients.workspace_in_direction(WorkspaceDirection::Next),
            WorkspaceId::new(0)
        );
    }

    #[test]
    fn horizontal_workspace_grid_navigates_and_wraps_ragged_rows() {
        let layout = WorkspaceLayout::new(
            5,
            3,
            0,
            WorkspaceOrientation::Horizontal,
            WorkspaceCorner::TopLeft,
        )
        .unwrap();
        assert_eq!((layout.columns(), layout.rows()), (3, 2));
        assert_eq!(
            layout.neighbor(WorkspaceId::new(0), WorkspaceDirection::Down, false),
            WorkspaceId::new(3)
        );
        assert_eq!(
            layout.neighbor(WorkspaceId::new(2), WorkspaceDirection::Down, true),
            WorkspaceId::new(2)
        );
        assert_eq!(
            layout.neighbor(WorkspaceId::new(4), WorkspaceDirection::Right, true),
            WorkspaceId::new(3)
        );
    }

    #[test]
    fn vertical_bottom_right_layout_maps_direction_to_user_visible_geometry() {
        let layout = WorkspaceLayout::new(
            6,
            3,
            2,
            WorkspaceOrientation::Vertical,
            WorkspaceCorner::BottomRight,
        )
        .unwrap();

        assert_eq!(
            layout.neighbor(WorkspaceId::new(0), WorkspaceDirection::Left, false),
            WorkspaceId::new(2)
        );
        assert_eq!(
            layout.neighbor(WorkspaceId::new(0), WorkspaceDirection::Up, false),
            WorkspaceId::new(1)
        );
        assert_eq!(
            layout.neighbor(WorkspaceId::new(0), WorkspaceDirection::Right, true),
            WorkspaceId::new(4)
        );
    }

    #[test]
    fn workspace_layout_rejects_two_derived_dimensions_and_bounds_hostile_sizes() {
        assert!(
            WorkspaceLayout::new(
                4,
                0,
                0,
                WorkspaceOrientation::Horizontal,
                WorkspaceCorner::TopLeft,
            )
            .is_none()
        );
        let bounded = WorkspaceLayout::new(
            4,
            u32::MAX,
            u32::MAX,
            WorkspaceOrientation::Horizontal,
            WorkspaceCorner::TopLeft,
        )
        .unwrap();
        assert_eq!((bounded.columns(), bounded.rows()), (4, 4));
    }

    #[test]
    fn geometry_never_becomes_empty() {
        assert_eq!(Geometry::new(2, 3, 0, 0), Geometry::new(2, 3, 1, 1));
    }

    #[test]
    fn translated_geometry_saturates_coordinates() {
        assert_eq!(
            Geometry::new(i32::MAX - 2, i32::MIN + 2, 40, 30).translated(10, -10),
            Geometry::new(i32::MAX, i32::MIN, 40, 30)
        );
    }

    #[test]
    fn absolute_placement_resolves_anchors_and_preserves_output_offsets() {
        let source = Geometry::new(0, 0, 800, 600);
        let target = Geometry::new(800, 20, 1000, 700);
        let current = Geometry::new(100, 80, 300, 200);
        assert_eq!(
            move_resize_geometry(
                current,
                source,
                target,
                Size::new(400, 300),
                AxisPlacement::Keep,
                AxisPlacement::Keep,
            ),
            Geometry::new(900, 100, 400, 300)
        );
        assert_eq!(
            move_resize_geometry(
                current,
                source,
                target,
                Size::new(400, 300),
                AxisPlacement::Center,
                AxisPlacement::End(25),
            ),
            Geometry::new(1100, 395, 400, 300)
        );
        assert_eq!(
            move_resize_geometry(
                current,
                source,
                target,
                Size::new(400, 300),
                AxisPlacement::Start(30),
                AxisPlacement::Start(40),
            ),
            Geometry::new(830, 60, 400, 300)
        );
    }

    #[test]
    fn absolute_placement_clamps_offsets_and_oversized_rectangles() {
        let bounds = Geometry::new(10, 20, 400, 300);
        assert_eq!(
            move_resize_geometry(
                Geometry::new(0, 0, 100, 100),
                bounds,
                bounds,
                Size::new(100, 100),
                AxisPlacement::End(-500),
                AxisPlacement::Start(-500),
            ),
            Geometry::new(310, 20, 100, 100)
        );
        assert_eq!(
            move_resize_geometry(
                Geometry::new(0, 0, 100, 100),
                bounds,
                bounds,
                Size::new(800, 600),
                AxisPlacement::Center,
                AxisPlacement::Center,
            ),
            Geometry::new(10, 20, 800, 600)
        );
    }

    #[test]
    fn directional_targets_cover_cardinal_and_diagonal_neighbors() {
        let origin = Geometry::new(300, 300, 100, 100);
        let candidates = [
            (1_u8, origin),
            (2, Geometry::new(100, 300, 100, 100)),
            (3, Geometry::new(500, 300, 100, 100)),
            (4, Geometry::new(300, 100, 100, 100)),
            (5, Geometry::new(300, 500, 100, 100)),
            (6, Geometry::new(100, 100, 100, 100)),
            (7, Geometry::new(500, 100, 100, 100)),
            (8, Geometry::new(100, 500, 100, 100)),
            (9, Geometry::new(500, 500, 100, 100)),
        ];
        for (direction, expected) in [
            (SpatialDirection::Left, 2),
            (SpatialDirection::Right, 3),
            (SpatialDirection::Up, 4),
            (SpatialDirection::Down, 5),
            (SpatialDirection::UpLeft, 6),
            (SpatialDirection::UpRight, 7),
            (SpatialDirection::DownLeft, 8),
            (SpatialDirection::DownRight, 9),
        ] {
            assert_eq!(
                directional_target(1, origin, candidates, direction),
                Some(expected),
                "wrong target for {direction:?}"
            );
        }
    }

    #[test]
    fn directional_targets_prioritize_the_cone_and_keep_stable_ties() {
        let origin = Geometry::new(0, 0, 100, 100);
        assert_eq!(
            directional_target(
                1_u8,
                origin,
                [
                    (2, Geometry::new(10, 500, 100, 100)),
                    (3, Geometry::new(2_000_000, 0, 100, 100)),
                ],
                SpatialDirection::Right,
            ),
            Some(3),
            "an in-cone target must beat a closer off-axis target"
        );
        assert_eq!(
            directional_target(
                1_u8,
                origin,
                [
                    (2, Geometry::new(200, -50, 100, 100)),
                    (3, Geometry::new(200, 50, 100, 100)),
                ],
                SpatialDirection::Right,
            ),
            Some(2),
            "equal scores must preserve caller ordering"
        );
        assert_eq!(
            directional_target(1_u8, origin, [(2, origin)], SpatialDirection::Right),
            None
        );
    }

    #[test]
    fn adaptive_restack_raises_lowers_or_preserves_order_from_overlap() {
        let target = Geometry::new(100, 100, 200, 150);
        let below = Geometry::new(50, 50, 100, 100);
        let above = Geometry::new(250, 200, 100, 100);
        assert_eq!(
            adaptive_restack(2_u8, target, [(1, below), (2, target), (3, above)]),
            RestackDecision::Raise,
            "overlap above takes precedence over overlap below"
        );
        assert_eq!(
            adaptive_restack(2_u8, target, [(1, below), (2, target)]),
            RestackDecision::Lower
        );
        assert_eq!(
            adaptive_restack(
                2_u8,
                target,
                [
                    (1, Geometry::new(0, 0, 50, 50)),
                    (2, target),
                    (3, Geometry::new(300, 100, 50, 50)),
                ],
            ),
            RestackDecision::Unchanged,
            "touching edges do not obscure a rectangle"
        );
        assert_eq!(
            adaptive_restack(9_u8, target, [(1, below), (2, target), (3, above)]),
            RestackDecision::Unchanged,
            "an absent target cannot be restacked"
        );
    }

    #[test]
    fn relative_resize_moves_changed_start_edges() {
        let geometry = Geometry::new(100, 80, 200, 100);
        assert_eq!(
            relative_resize_geometry(
                geometry,
                ResizeDeltas {
                    left: 20,
                    right: 30,
                    top: 10,
                    bottom: 40,
                },
                SizeHints::default(),
            ),
            Geometry::new(80, 70, 250, 150)
        );
        assert_eq!(
            relative_resize_geometry(
                geometry,
                ResizeDeltas {
                    left: -25,
                    bottom: -30,
                    ..ResizeDeltas::default()
                },
                SizeHints::default(),
            ),
            Geometry::new(125, 80, 175, 70)
        );
    }

    #[test]
    fn relative_resize_honors_size_limits_and_increments() {
        let minimum = SizeHints {
            minimum: Some(Size::new(50, 1)),
            ..SizeHints::default()
        };
        assert_eq!(
            relative_resize_geometry(
                Geometry::new(100, 20, 100, 50),
                ResizeDeltas {
                    left: -80,
                    ..ResizeDeltas::default()
                },
                minimum,
            ),
            Geometry::new(150, 20, 50, 50)
        );

        let increments = SizeHints {
            base: Some(Size::new(10, 10)),
            increment: Some(Size::new(20, 10)),
            ..SizeHints::default()
        };
        assert_eq!(
            relative_resize_geometry(
                Geometry::new(100, 20, 90, 50),
                ResizeDeltas {
                    left: 3,
                    ..ResizeDeltas::default()
                },
                increments,
            ),
            Geometry::new(80, 20, 110, 50)
        );
    }

    #[test]
    fn directional_move_steps_across_near_and_far_obstacle_edges() {
        let bounds = Geometry::new(0, 0, 800, 600);
        let left = Geometry::new(100, 150, 100, 200);
        let right = Geometry::new(500, 150, 100, 200);
        let subject = Geometry::new(300, 200, 100, 80);
        let obstacles = [left, right];

        let against_left =
            directional_move_geometry(subject, bounds, &obstacles, CardinalDirection::Left);
        assert_eq!(against_left, Geometry::new(200, 200, 100, 80));
        assert_eq!(
            directional_move_geometry(against_left, bounds, &obstacles, CardinalDirection::Left,),
            Geometry::new(0, 200, 100, 80)
        );

        let against_right =
            directional_move_geometry(subject, bounds, &obstacles, CardinalDirection::Right);
        assert_eq!(against_right, Geometry::new(400, 200, 100, 80));
        assert_eq!(
            directional_move_geometry(against_right, bounds, &obstacles, CardinalDirection::Right,),
            Geometry::new(600, 200, 100, 80)
        );
    }

    #[test]
    fn directional_move_ignores_nonoverlapping_obstacles() {
        let bounds = Geometry::new(10, 20, 400, 300);
        let subject = Geometry::new(100, 100, 80, 60);
        let obstacle = Geometry::new(200, 200, 100, 50);
        assert_eq!(
            directional_move_geometry(subject, bounds, &[obstacle], CardinalDirection::Right,),
            Geometry::new(330, 100, 80, 60)
        );
        assert_eq!(
            directional_move_geometry(subject, bounds, &[obstacle], CardinalDirection::Up,),
            Geometry::new(100, 20, 80, 60)
        );
    }

    #[test]
    fn directional_grow_stops_at_and_then_crosses_blocking_edges() {
        let bounds = Geometry::new(0, 0, 800, 600);
        let subject = Geometry::new(300, 200, 100, 80);
        let obstacle = Geometry::new(500, 150, 100, 200);

        let grown = directional_grow_geometry(
            subject,
            bounds,
            &[obstacle],
            CardinalDirection::Right,
            BlockingEdgePolicy::Cross,
        );
        assert_eq!(grown, Geometry::new(300, 200, 200, 80));
        assert_eq!(
            directional_grow_geometry(
                grown,
                bounds,
                &[obstacle],
                CardinalDirection::Right,
                BlockingEdgePolicy::Stop,
            ),
            grown
        );
        assert_eq!(
            directional_grow_geometry(
                grown,
                bounds,
                &[obstacle],
                CardinalDirection::Right,
                BlockingEdgePolicy::Cross,
            ),
            Geometry::new(300, 200, 300, 80)
        );
    }

    #[test]
    fn directional_shrink_moves_the_opposite_edge_and_keeps_half() {
        let bounds = Geometry::new(0, 0, 800, 600);
        let subject = Geometry::new(300, 200, 100, 80);
        assert_eq!(
            directional_shrink_geometry(subject, bounds, &[], CardinalDirection::Right,),
            Geometry::new(350, 200, 50, 80)
        );
        assert_eq!(
            directional_shrink_geometry(
                subject,
                bounds,
                &[Geometry::new(320, 150, 10, 200)],
                CardinalDirection::Right,
            ),
            Geometry::new(320, 200, 80, 80)
        );
        assert_eq!(
            directional_shrink_geometry(subject, bounds, &[], CardinalDirection::Up),
            Geometry::new(300, 200, 100, 40)
        );
    }

    #[test]
    fn grow_to_fill_crosses_blockers_only_when_every_side_is_blocked() {
        let bounds = Geometry::new(0, 0, 800, 600);
        let subject = Geometry::new(300, 200, 100, 100);
        let blockers = [
            Geometry::new(200, 200, 100, 100),
            Geometry::new(400, 200, 100, 100),
            Geometry::new(300, 100, 100, 100),
            Geometry::new(300, 300, 100, 100),
        ];
        assert_eq!(
            grow_to_fill_geometry(subject, bounds, &blockers),
            Geometry::new(200, 100, 300, 300)
        );
        assert_eq!(
            grow_to_fill_geometry(subject, bounds, &[blockers[1]]),
            Geometry::new(0, 0, 400, 600)
        );
    }

    #[test]
    fn resize_deltas_capture_all_changed_edges() {
        assert_eq!(
            ResizeDeltas::between(
                Geometry::new(100, 80, 200, 100),
                Geometry::new(70, 60, 250, 150),
            ),
            ResizeDeltas {
                left: 30,
                right: 20,
                top: 20,
                bottom: 30,
            }
        );
    }

    #[test]
    fn smart_placement_centers_in_empty_and_partitioned_free_space() {
        let bounds = Geometry::new(0, 0, 800, 600);
        let size = Size::new(200, 100);
        assert_eq!(
            smart_placement(size, bounds, &[], true),
            Geometry::new(300, 250, 200, 100)
        );
        assert_eq!(
            smart_placement(size, bounds, &[], false),
            Geometry::new(0, 0, 200, 100)
        );
        assert_eq!(
            smart_placement(size, bounds, &[Geometry::new(0, 0, 400, 600)], true),
            Geometry::new(500, 250, 200, 100)
        );
    }

    #[test]
    fn smart_placement_prefers_one_overlap_and_bounds_hostile_sizes() {
        let bounds = Geometry::new(10, 20, 400, 300);
        let obstacles = [
            Geometry::new(10, 20, 180, 300),
            Geometry::new(210, 20, 100, 300),
            Geometry::new(310, 20, 100, 300),
        ];
        let placed = smart_placement(Size::new(220, 100), bounds, &obstacles, false);
        assert_eq!(placed, Geometry::new(10, 20, 220, 100));
        assert_eq!(
            smart_placement(Size::new(500, 400), bounds, &[], true),
            Geometry::new(10, 20, 500, 400)
        );
    }

    #[test]
    fn placement_column_cache_preserves_exact_overlap_scores() {
        let obstacles = [
            Geometry::new(-30, 20, 80, 100),
            Geometry::new(45, -10, 150, 70),
            Geometry::new(100, 90, 200, 160),
            Geometry::new(i32::MAX - 10, i32::MAX - 10, u32::MAX, u32::MAX),
        ];
        for candidate in [
            Geometry::new(0, 0, 120, 90),
            Geometry::new(40, 50, 220, 170),
            Geometry::new(i32::MAX - 20, i32::MAX - 20, 100, 100),
        ] {
            let column = horizontal_placement_overlaps(candidate.x, candidate.width, &obstacles);
            assert_eq!(
                placement_overlap_score(candidate, &obstacles),
                placement_overlap_score_in_column(
                    candidate.y,
                    candidate.height,
                    &column,
                    u128::MAX,
                )
            );
        }
    }

    #[test]
    fn centered_placement_uses_anchor_and_stays_inside_work_area() {
        let bounds = Geometry::new(0, 30, 800, 570);
        assert_eq!(
            centered_placement(
                Size::new(200, 100),
                bounds,
                Geometry::new(650, 500, 300, 200)
            ),
            Geometry::new(600, 500, 200, 100)
        );
    }

    #[test]
    fn decoration_extents_expand_around_content_without_overflow() {
        let extents = DecorationExtents::new(2, 3, 24, 4);
        assert_eq!(
            extents.outer_geometry(Geometry::new(50, 40, 640, 480)),
            Geometry::new(48, 16, 645, 508)
        );
        assert_eq!(
            extents.content_geometry(Geometry::new(48, 16, 645, 508)),
            Geometry::new(50, 40, 640, 480)
        );
        assert_eq!(extents.content_offset(), (2, 24));
        assert_eq!(
            extents.outer_geometry(Geometry::new(i32::MIN, i32::MIN, u32::MAX, u32::MAX)),
            Geometry::new(i32::MIN, i32::MIN, u32::MAX, u32::MAX)
        );
    }

    #[test]
    fn output_selection_uses_overlap_distance_and_primary_tiebreaks() {
        let outputs = OutputSet::new([
            Output {
                id: OutputId::new(10),
                geometry: Geometry::new(0, 0, 800, 600),
                primary: false,
            },
            Output {
                id: OutputId::new(20),
                geometry: Geometry::new(800, 0, 800, 600),
                primary: true,
            },
        ]);

        assert_eq!(outputs.primary().id, OutputId::new(20));
        assert_eq!(
            outputs.output_for(Geometry::new(600, 100, 300, 200)).id,
            OutputId::new(10)
        );
        assert_eq!(
            outputs.output_for(Geometry::new(2100, 100, 100, 100)).id,
            OutputId::new(20)
        );
        assert_eq!(
            outputs.overlapping_output(Geometry::new(2100, 100, 100, 100)),
            None
        );
        assert_eq!(
            outputs.output_for(Geometry::new(700, 100, 200, 100)).id,
            OutputId::new(20)
        );
    }

    #[test]
    fn output_topology_normalizes_duplicates_and_empty_fallbacks() {
        let duplicate = OutputId::new(7);
        let outputs = OutputSet::new([
            Output {
                id: duplicate,
                geometry: Geometry::new(0, 0, 640, 480),
                primary: false,
            },
            Output {
                id: duplicate,
                geometry: Geometry::new(640, 0, 640, 480),
                primary: true,
            },
        ]);
        assert_eq!(outputs.outputs().len(), 1);
        assert!(outputs.primary().primary);

        let fallback = OutputSet::default();
        assert_eq!(fallback.outputs().len(), 1);
        assert_eq!(fallback.primary().geometry, Geometry::new(0, 0, 1, 1));
    }

    #[test]
    fn clamping_position_preserves_size_and_handles_oversized_axes() {
        let bounds = Geometry::new(800, 30, 800, 570);
        assert_eq!(
            Geometry::new(1700, -20, 300, 200).clamp_position(bounds),
            Geometry::new(1300, 30, 300, 200)
        );
        assert_eq!(
            Geometry::new(900, 100, 1000, 700).clamp_position(bounds),
            Geometry::new(800, 30, 1000, 700)
        );
    }

    #[test]
    fn work_area_uses_deepest_intersecting_reservation_per_edge() {
        let output = Geometry::new(0, 0, 800, 600);
        let shallow_top = EdgeReservations {
            top: EdgeReservation {
                depth: 20,
                start: 0,
                end: 799,
            },
            ..EdgeReservations::default()
        };
        let deep_top_and_left = EdgeReservations {
            left: EdgeReservation {
                depth: 30,
                start: 100,
                end: 500,
            },
            top: EdgeReservation {
                depth: 40,
                start: 200,
                end: 700,
            },
            ..EdgeReservations::default()
        };

        assert_eq!(
            output.work_area([shallow_top, deep_top_and_left]),
            Geometry::new(30, 40, 770, 560)
        );
    }

    #[test]
    fn work_area_ignores_nonintersecting_partial_reservations() {
        let output = Geometry::new(100, 100, 800, 600);
        let reservation = EdgeReservations {
            top: EdgeReservation {
                depth: 50,
                start: 0,
                end: 99,
            },
            ..EdgeReservations::default()
        };

        assert_eq!(output.work_area([reservation]), output);
    }

    #[test]
    fn hostile_reservations_cannot_make_work_area_empty() {
        let output = Geometry::new(0, 0, 10, 10);
        let reservation = EdgeReservations {
            left: EdgeReservation {
                depth: u32::MAX,
                start: 0,
                end: 9,
            },
            right: EdgeReservation {
                depth: u32::MAX,
                start: 0,
                end: 9,
            },
            top: EdgeReservation {
                depth: u32::MAX,
                start: 0,
                end: 9,
            },
            bottom: EdgeReservation {
                depth: u32::MAX,
                start: 0,
                end: 9,
            },
        };

        assert_eq!(output.work_area([reservation]), Geometry::new(9, 9, 1, 1));
    }

    #[test]
    fn movement_snaps_nearest_outer_edges_within_resistance() {
        let bounds = Geometry::new(2, 26, 796, 572);
        assert_eq!(
            Geometry::new(7, 31, 360, 120).snap_movement(bounds, 10),
            Geometry::new(2, 26, 360, 120)
        );
        assert_eq!(
            Geometry::new(433, 473, 360, 120).snap_movement(bounds, 10),
            Geometry::new(438, 478, 360, 120)
        );
        assert_eq!(
            Geometry::new(20, 50, 360, 120).snap_movement(bounds, 10),
            Geometry::new(20, 50, 360, 120)
        );
    }

    #[test]
    fn movement_snaps_beside_nearby_rectangles_and_aligns_corners() {
        let target = Geometry::new(300, 100, 200, 150);
        assert_eq!(
            Geometry::new(94, 106, 200, 150).snap_movement_to([target], 10),
            Geometry::new(100, 100, 200, 150)
        );
        assert_eq!(
            Geometry::new(506, 194, 200, 50).snap_movement_to([target], 10),
            Geometry::new(500, 200, 200, 50)
        );
    }

    #[test]
    fn movement_window_snap_requires_perpendicular_overlap_and_uses_nearest_edge() {
        let distant_axis = Geometry::new(300, 300, 200, 100);
        assert_eq!(
            Geometry::new(94, 100, 200, 100).snap_movement_to([distant_axis], 10),
            Geometry::new(94, 100, 200, 100)
        );

        let farther = Geometry::new(300, 100, 200, 100);
        let nearer = Geometry::new(296, 100, 200, 100);
        assert_eq!(
            Geometry::new(94, 100, 200, 100).snap_movement_to([farther, nearer], 10),
            Geometry::new(96, 100, 200, 100)
        );
    }

    #[test]
    fn resize_snaps_bottom_right_edges_within_resistance() {
        let bounds = Geometry::new(2, 26, 796, 572);
        assert_eq!(
            Geometry::new(2, 26, 791, 567).snap_resize(bounds, 10),
            bounds
        );
        assert_eq!(
            Geometry::new(2, 26, 700, 500).snap_resize(bounds, 10),
            Geometry::new(2, 26, 700, 500)
        );
    }

    #[test]
    fn normal_and_toolbar_roles_select_distinct_capabilities() {
        let normal = ClientPolicy::for_role(ClientRole::Normal);
        assert!(normal.capabilities.resizable);
        assert!(normal.decorations.minimize);
        assert_eq!(
            normal.decorations.extents(2, 24),
            DecorationExtents::new(2, 2, 26, 2)
        );

        let toolbar = ClientPolicy::for_role(ClientRole::Toolbar);
        assert!(!toolbar.capabilities.minimizable);
        assert!(!toolbar.capabilities.maximizable);
        assert!(!toolbar.decorations.minimize);
        assert!(toolbar.decorations.titlebar);
    }

    #[test]
    fn special_surfaces_are_not_decorated_or_manipulated() {
        for role in [
            ClientRole::Desktop,
            ClientRole::Dock,
            ClientRole::Notification,
            ClientRole::Tooltip,
        ] {
            let policy = ClientPolicy::for_role(role);
            assert_eq!(
                policy.decorations.extents(2, 24),
                DecorationExtents::default()
            );
            assert!(!policy.capabilities.movable);
            assert!(!policy.capabilities.closable);
        }
    }

    #[test]
    fn full_maximize_round_trips_exact_restore_geometry() {
        let mut clients = ClientSet::default();
        let mut original = client(1);
        original.geometry = Geometry::new(40, 50, 640, 480);
        clients.manage(original);
        let available = Geometry::new(2, 26, 796, 572);

        assert_eq!(
            clients.set_maximized(ClientId::new(1), true, true, available),
            Some(available)
        );
        assert_eq!(clients.get(ClientId::new(1)).unwrap().geometry, available);
        assert_eq!(
            clients.set_maximized(ClientId::new(1), false, false, available),
            Some(original.geometry)
        );
        assert!(clients.get(ClientId::new(1)).unwrap().maximize.is_none());
    }

    #[test]
    fn unmanaged_geometry_unwinds_fullscreen_and_maximize_state() {
        let mut clients = ClientSet::default();
        let mut original = client(1);
        original.geometry = Geometry::new(40, 50, 640, 480);
        clients.manage(original);
        let available = Geometry::new(2, 26, 796, 572);
        let output = Geometry::new(0, 0, 800, 600);
        clients.set_maximized(ClientId::new(1), true, true, available);
        clients.set_fullscreen(ClientId::new(1), true, output);

        assert_eq!(
            clients.get(ClientId::new(1)).unwrap().unmanaged_geometry(),
            original.geometry
        );
    }

    #[test]
    fn maximize_axes_restore_independently() {
        let mut clients = ClientSet::default();
        let mut original = client(1);
        original.geometry = Geometry::new(40, 50, 640, 480);
        clients.manage(original);
        let available = Geometry::new(2, 26, 796, 572);

        assert_eq!(
            clients.set_maximized(ClientId::new(1), true, false, available),
            Some(Geometry::new(2, 50, 796, 480))
        );
        assert_eq!(
            clients.set_maximized(ClientId::new(1), true, true, available),
            Some(available)
        );
        assert_eq!(
            clients.set_maximized(ClientId::new(1), false, true, available),
            Some(Geometry::new(40, 26, 640, 572))
        );
        assert_eq!(
            clients.set_maximized(ClientId::new(1), false, false, available),
            Some(original.geometry)
        );
    }

    #[test]
    fn nonmaximizable_role_rejects_entering_maximize_state() {
        let mut clients = ClientSet::default();
        let mut dock = client(1);
        dock.policy = ClientPolicy::for_role(ClientRole::Dock);
        clients.manage(dock);

        assert_eq!(
            clients.set_maximized(ClientId::new(1), true, true, Geometry::new(0, 0, 800, 600)),
            None
        );
        assert!(clients.get(ClientId::new(1)).unwrap().maximize.is_none());
    }

    #[test]
    fn maximized_geometry_tracks_available_area_changes() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.set_maximized(ClientId::new(1), true, true, Geometry::new(2, 26, 796, 572));

        assert_eq!(
            clients.set_maximized(
                ClientId::new(1),
                true,
                true,
                Geometry::new(0, 24, 1024, 744),
            ),
            Some(Geometry::new(0, 24, 1024, 744))
        );
    }

    #[test]
    fn fullscreen_round_trips_and_tracks_output_changes() {
        let mut clients = ClientSet::default();
        let original = client(1);
        clients.manage(original);
        let first_output = Geometry::new(0, 0, 800, 600);
        let second_output = Geometry::new(800, 0, 1024, 768);

        assert_eq!(
            clients.set_fullscreen(ClientId::new(1), true, first_output),
            Some(first_output)
        );
        assert_eq!(
            clients.set_fullscreen(ClientId::new(1), true, second_output),
            Some(second_output)
        );
        assert_eq!(
            clients.set_fullscreen(ClientId::new(1), false, second_output),
            Some(original.geometry)
        );
        assert!(clients.get(ClientId::new(1)).unwrap().fullscreen.is_none());
    }

    #[test]
    fn fullscreen_preserves_maximize_state_across_work_area_changes() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.set_maximized(ClientId::new(1), true, true, Geometry::new(0, 30, 800, 570));
        clients.set_fullscreen(ClientId::new(1), true, Geometry::new(0, 0, 800, 600));

        assert_eq!(
            clients.set_maximized(ClientId::new(1), true, true, Geometry::new(0, 50, 800, 550)),
            None
        );
        assert_eq!(
            clients.set_fullscreen(ClientId::new(1), false, Geometry::new(0, 0, 800, 600)),
            Some(Geometry::new(0, 50, 800, 550))
        );
        assert!(
            clients
                .get(ClientId::new(1))
                .unwrap()
                .maximize
                .is_some_and(|state| state.horizontal && state.vertical)
        );
    }

    #[test]
    fn roles_and_requested_state_resolve_stacking_layers() {
        let mut normal = client(1);
        assert_eq!(normal.stacking_layer(), StackingLayer::Normal);
        normal.layer = ClientLayer::Above;
        assert_eq!(normal.stacking_layer(), StackingLayer::Above);
        normal.fullscreen = Some(FullscreenState {
            restore: normal.geometry,
        });
        assert_eq!(normal.stacking_layer(), StackingLayer::Fullscreen);

        let mut dock = client(2);
        dock.policy = ClientPolicy::for_role(ClientRole::Dock);
        assert_eq!(dock.stacking_layer(), StackingLayer::Dock);
        dock.layer = ClientLayer::Below;
        assert_eq!(dock.stacking_layer(), StackingLayer::Below);

        let mut desktop = client(3);
        desktop.policy = ClientPolicy::for_role(ClientRole::Desktop);
        desktop.layer = ClientLayer::Above;
        assert_eq!(desktop.stacking_layer(), StackingLayer::Desktop);
    }

    #[test]
    fn user_operations_follow_role_capabilities_and_runtime_state() {
        let mut normal = client(1);
        let operations = normal.operations();
        assert!(operations.movable && operations.resizable && operations.maximizable);
        assert!(operations.decoratable);
        assert!(operations.workspace_movable && operations.above && operations.below);

        normal.fullscreen = Some(FullscreenState {
            restore: normal.geometry,
        });
        let fullscreen = normal.operations();
        assert!(fullscreen.movable && fullscreen.minimizable && fullscreen.fullscreenable);
        assert!(!fullscreen.resizable && !fullscreen.maximizable);
        assert!(!fullscreen.decoratable);
        assert!(!fullscreen.above && !fullscreen.below);

        let mut dock = client(2);
        dock.policy = ClientPolicy::for_role(ClientRole::Dock);
        let dock = dock.operations();
        assert!(dock.workspace_movable && dock.below);
        assert!(!dock.above && !dock.movable && !dock.closable && !dock.decoratable);

        let mut desktop = client(3);
        desktop.policy = ClientPolicy::for_role(ClientRole::Desktop);
        let desktop = desktop.operations();
        assert!(!desktop.workspace_movable && !desktop.above && !desktop.below);
    }

    #[test]
    fn decoration_override_round_trips_and_tracks_natural_policy() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));

        let undecorated = clients
            .toggle_decorations(ClientId::new(1))
            .expect("normal client supports decorations");
        assert_eq!(undecorated.decorations, ClientDecorations::NONE);
        assert_eq!(
            clients.get(ClientId::new(1)).unwrap().decoration_override,
            DecorationOverride::Undecorated
        );

        let mut changed = ClientPolicy::for_role(ClientRole::Normal);
        changed.capabilities.maximizable = false;
        changed.decorations.maximize = false;
        clients.set_policy(ClientId::new(1), changed);
        assert_eq!(
            clients.get(ClientId::new(1)).unwrap().policy.decorations,
            ClientDecorations::NONE
        );

        let restored = clients
            .toggle_decorations(ClientId::new(1))
            .expect("normal client supports decorations");
        assert_eq!(restored, changed);
        assert_eq!(
            clients.get(ClientId::new(1)).unwrap().decoration_override,
            DecorationOverride::Default
        );

        let mut naturally_undecorated = changed;
        naturally_undecorated.decorations = ClientDecorations::NONE;
        clients.set_policy(ClientId::new(1), naturally_undecorated);
        let forced = clients
            .toggle_decorations(ClientId::new(1))
            .expect("normal client supports decorations");
        assert!(forced.decorations.titlebar);
        assert!(!forced.decorations.maximize);
        assert_eq!(
            clients
                .toggle_decorations(ClientId::new(1))
                .expect("normal client supports decorations")
                .decorations,
            ClientDecorations::NONE
        );

        assert!(
            clients
                .set_decoration_override(ClientId::new(1), DecorationOverride::Undecorated,)
                .is_some()
        );
        assert!(
            clients
                .set_decoration_override(ClientId::new(1), DecorationOverride::Undecorated,)
                .is_none(),
            "repeating an explicit preference is idempotent"
        );
        assert_eq!(
            clients
                .set_decoration_override(ClientId::new(1), DecorationOverride::Default)
                .expect("restoring the natural policy changes the preference")
                .decorations,
            ClientDecorations::NONE
        );
    }

    #[test]
    fn raising_changes_stacking_but_not_management_order() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.manage(client(2));

        clients.raise(ClientId::new(1));

        assert_eq!(
            clients.management_order().collect::<Vec<_>>(),
            [ClientId::new(1), ClientId::new(2)]
        );
        assert_eq!(
            clients.stacking().collect::<Vec<_>>(),
            [ClientId::new(2), ClientId::new(1)]
        );
    }

    #[test]
    fn lowering_changes_stacking_but_not_management_order() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.manage(client(2));

        clients.lower(ClientId::new(2));

        assert_eq!(
            clients.management_order().collect::<Vec<_>>(),
            [ClientId::new(1), ClientId::new(2)]
        );
        assert_eq!(
            clients.stacking().collect::<Vec<_>>(),
            [ClientId::new(2), ClientId::new(1)]
        );
    }

    #[test]
    fn backend_stacking_discards_unknowns_and_preserves_unobserved_clients() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.manage(client(2));
        clients.manage(client(3));

        clients.sync_stacking([
            ClientId::new(3),
            ClientId::new(99),
            ClientId::new(3),
            ClientId::new(1),
        ]);

        assert_eq!(
            clients.stacking().collect::<Vec<_>>(),
            [ClientId::new(2), ClientId::new(3), ClientId::new(1)]
        );
    }

    #[test]
    fn policy_stacking_keeps_transient_chains_above_their_parents() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.manage(client(2));
        clients.manage(client(3));
        clients.set_relationships(
            ClientId::new(2),
            Some(TransientTarget::Client(ClientId::new(1))),
            None,
            false,
        );
        clients.set_relationships(
            ClientId::new(3),
            Some(TransientTarget::Client(ClientId::new(2))),
            None,
            false,
        );
        clients.raise(ClientId::new(1));

        assert_eq!(
            clients.policy_stacking(&OutputSet::default()),
            [ClientId::new(1), ClientId::new(2), ClientId::new(3)]
        );
    }

    #[test]
    fn transient_inherits_a_higher_parent_layer_without_losing_own_layer() {
        let mut clients = ClientSet::default();
        let mut parent = client(1);
        parent.layer = ClientLayer::Above;
        clients.manage(parent);
        clients.manage(client(2));
        clients.set_relationships(
            ClientId::new(2),
            Some(TransientTarget::Client(ClientId::new(1))),
            None,
            false,
        );
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(2), &OutputSet::default()),
            Some(StackingLayer::Above)
        );

        assert!(clients.set_layer(ClientId::new(2), ClientLayer::Above));
        assert!(!clients.set_layer(ClientId::new(2), ClientLayer::Above));
        assert!(clients.set_layer(ClientId::new(1), ClientLayer::Normal));
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(2), &OutputSet::default()),
            Some(StackingLayer::Above)
        );
    }

    #[test]
    fn exact_output_coverage_tracks_focus_family_workspace_and_output() {
        let outputs = OutputSet::new([
            Output {
                id: OutputId::new(10),
                geometry: Geometry::new(0, 0, 800, 600),
                primary: true,
            },
            Output {
                id: OutputId::new(20),
                geometry: Geometry::new(800, 0, 800, 600),
                primary: false,
            },
        ]);
        let mut clients = ClientSet::default();
        let mut covering = client(1);
        covering.geometry = Geometry::new(0, 0, 800, 600);
        covering.output_coverage = Some(OutputCoverage::new(OutputId::new(10)));
        clients.manage(covering);
        clients.manage(client(2));

        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(1), &outputs),
            Some(StackingLayer::Fullscreen)
        );
        clients.focus(ClientId::new(2));
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(1), &outputs),
            Some(StackingLayer::Normal)
        );

        clients.set_geometry(ClientId::new(2), Geometry::new(900, 50, 300, 200));
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(1), &outputs),
            Some(StackingLayer::Fullscreen)
        );

        let mut child = client(3);
        child.geometry = Geometry::new(50, 50, 200, 100);
        clients.manage(child);
        clients.set_relationships(
            ClientId::new(3),
            Some(TransientTarget::Client(ClientId::new(1))),
            None,
            false,
        );
        clients.focus(ClientId::new(3));
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(1), &outputs),
            Some(StackingLayer::Fullscreen)
        );
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(3), &outputs),
            Some(StackingLayer::Fullscreen)
        );

        clients.set_workspace_count(2);
        clients.assign_workspace_family(
            ClientId::new(1),
            WorkspaceAssignment::Workspace(WorkspaceId::new(1)),
        );
        clients.focus(ClientId::new(2));
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(1), &outputs),
            Some(StackingLayer::Fullscreen)
        );
    }

    #[test]
    fn managed_fullscreen_yields_to_focused_windows_on_the_same_output() {
        let outputs = OutputSet::new([
            Output {
                id: OutputId::new(10),
                geometry: Geometry::new(0, 0, 800, 600),
                primary: true,
            },
            Output {
                id: OutputId::new(20),
                geometry: Geometry::new(800, 0, 800, 600),
                primary: false,
            },
        ]);
        let mut clients = ClientSet::default();
        let mut fullscreen = client(1);
        fullscreen.geometry = Geometry::new(0, 0, 800, 600);
        fullscreen.fullscreen = Some(FullscreenState {
            restore: Geometry::new(50, 50, 400, 300),
        });
        clients.manage(fullscreen);
        clients.focus(ClientId::new(1));
        clients.manage(client(2));

        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(1), &outputs),
            Some(StackingLayer::Fullscreen)
        );
        clients.focus(ClientId::new(2));
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(1), &outputs),
            Some(StackingLayer::Normal)
        );
        assert_eq!(
            clients.policy_stacking(&outputs),
            [ClientId::new(1), ClientId::new(2)]
        );

        clients.set_geometry(ClientId::new(2), Geometry::new(900, 50, 300, 200));
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(1), &outputs),
            Some(StackingLayer::Fullscreen)
        );
    }

    #[test]
    fn maximize_and_managed_fullscreen_take_precedence_over_output_coverage() {
        let outputs = OutputSet::new([Output {
            id: OutputId::new(10),
            geometry: Geometry::new(0, 0, 800, 600),
            primary: true,
        }]);
        let mut clients = ClientSet::default();
        let mut covering = client(1);
        covering.geometry = Geometry::new(0, 0, 800, 600);
        covering.output_coverage = Some(OutputCoverage::new(OutputId::new(10)));
        clients.manage(covering);
        clients.focus(ClientId::new(1));

        clients.set_maximized(ClientId::new(1), true, true, Geometry::new(0, 0, 800, 600));
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(1), &outputs),
            Some(StackingLayer::Normal)
        );
        clients.set_fullscreen(ClientId::new(1), true, Geometry::new(0, 0, 800, 600));
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(1), &outputs),
            Some(StackingLayer::Fullscreen)
        );
    }

    #[test]
    fn moving_any_specific_transient_moves_the_complete_family() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(2);
        clients.manage(client(1));
        clients.manage(client(2));
        clients.manage(client(3));
        clients.set_relationships(
            ClientId::new(2),
            Some(TransientTarget::Client(ClientId::new(1))),
            None,
            false,
        );
        clients.set_relationships(
            ClientId::new(3),
            Some(TransientTarget::Client(ClientId::new(2))),
            None,
            false,
        );

        let changed = clients.assign_workspace_family(
            ClientId::new(2),
            WorkspaceAssignment::Workspace(WorkspaceId::new(1)),
        );
        assert_eq!(
            changed,
            [ClientId::new(1), ClientId::new(2), ClientId::new(3)]
        );
        assert!(changed.iter().all(|id| {
            clients.get(*id).unwrap().workspace
                == WorkspaceAssignment::Workspace(WorkspaceId::new(1))
        }));
    }

    #[test]
    fn cyclic_transient_stacking_and_family_moves_terminate() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(2);
        clients.manage(client(1));
        clients.manage(client(2));
        clients.set_relationships(
            ClientId::new(1),
            Some(TransientTarget::Client(ClientId::new(2))),
            None,
            false,
        );
        clients.set_relationships(
            ClientId::new(2),
            Some(TransientTarget::Client(ClientId::new(1))),
            None,
            false,
        );

        assert_eq!(clients.policy_stacking(&OutputSet::default()).len(), 2);
        assert_eq!(
            clients
                .assign_workspace_family(
                    ClientId::new(1),
                    WorkspaceAssignment::Workspace(WorkspaceId::new(1)),
                )
                .len(),
            2
        );
    }

    #[test]
    fn group_transients_stay_above_members_and_follow_their_workspace() {
        let mut clients = ClientSet::default();
        clients.set_workspace_count(2);
        for id in 1..=3 {
            clients.manage(client(id));
        }
        let group = Some(ClientId::new(99));
        clients.set_relationships(ClientId::new(1), None, group, false);
        clients.set_relationships(ClientId::new(2), Some(TransientTarget::Group), group, false);
        clients.set_relationships(
            ClientId::new(3),
            Some(TransientTarget::Client(ClientId::new(2))),
            group,
            false,
        );

        clients.raise(ClientId::new(1));
        assert_eq!(
            clients.policy_stacking(&OutputSet::default()),
            [ClientId::new(1), ClientId::new(2), ClientId::new(3)]
        );

        let changed = clients.assign_workspace_family(
            ClientId::new(1),
            WorkspaceAssignment::Workspace(WorkspaceId::new(1)),
        );
        assert_eq!(changed.len(), 3);
        assert!(changed.iter().all(|id| {
            clients.get(*id).unwrap().workspace
                == WorkspaceAssignment::Workspace(WorkspaceId::new(1))
        }));

        let changed = clients.assign_workspace_family(
            ClientId::new(2),
            WorkspaceAssignment::Workspace(WorkspaceId::new(0)),
        );
        assert_eq!(changed, [ClientId::new(2), ClientId::new(3)]);
        assert_eq!(
            clients.get(ClientId::new(1)).unwrap().workspace,
            WorkspaceAssignment::Workspace(WorkspaceId::new(1))
        );
    }

    #[test]
    fn multiple_group_transients_do_not_become_each_others_children() {
        let mut clients = ClientSet::default();
        for id in 1..=3 {
            clients.manage(client(id));
        }
        let group = Some(ClientId::new(99));
        clients.set_relationships(ClientId::new(1), None, group, false);
        clients.set_relationships(ClientId::new(2), Some(TransientTarget::Group), group, false);
        clients.set_relationships(ClientId::new(3), Some(TransientTarget::Group), group, false);

        clients.raise(ClientId::new(1));
        assert_eq!(
            clients.policy_stacking(&OutputSet::default()),
            [ClientId::new(1), ClientId::new(2), ClientId::new(3)]
        );
        assert_eq!(
            clients.assign_workspace_family(ClientId::new(2), WorkspaceAssignment::All),
            [ClientId::new(2)]
        );
    }

    #[test]
    fn focus_relationships_cover_transient_trees_and_application_groups() {
        let mut clients = ClientSet::default();
        for id in 1..=5 {
            clients.manage(client(id));
        }
        clients.set_relationships(
            ClientId::new(2),
            Some(TransientTarget::Client(ClientId::new(1))),
            None,
            false,
        );
        clients.set_relationships(
            ClientId::new(3),
            Some(TransientTarget::Client(ClientId::new(2))),
            None,
            false,
        );
        let group = ClientId::new(99);
        clients.set_relationships(ClientId::new(4), None, Some(group), false);
        clients.set_relationships(ClientId::new(5), None, Some(group), false);

        assert!(clients.clients_are_related(ClientId::new(1), ClientId::new(3)));
        assert!(clients.clients_are_related(ClientId::new(4), ClientId::new(5)));
        assert!(clients.clients_share_transient_family(ClientId::new(1), ClientId::new(3)));
        assert!(
            !clients.clients_share_transient_family(ClientId::new(4), ClientId::new(5)),
            "application groups are not specific-transient trees"
        );
        assert!(!clients.clients_are_related(ClientId::new(1), ClientId::new(4)));
        assert!(!clients.clients_are_related(ClientId::new(1), ClientId::new(42)));
    }

    #[test]
    fn size_hints_clamp_and_snap_to_base_relative_increments() {
        let hints = SizeHints {
            minimum: Some(Size::new(50, 40)),
            maximum: Some(Size::new(200, 160)),
            base: Some(Size::new(10, 10)),
            increment: Some(Size::new(20, 15)),
            aspect: None,
        };

        assert_eq!(hints.constrain(Size::new(47, 500)), Size::new(50, 160));
        assert_eq!(hints.constrain(Size::new(99, 89)), Size::new(90, 85));
    }

    #[test]
    fn invalid_maximum_is_raised_to_the_minimum() {
        let hints = SizeHints {
            minimum: Some(Size::new(100, 80)),
            maximum: Some(Size::new(20, 10)),
            ..SizeHints::default()
        };
        assert_eq!(hints.constrain(Size::new(50, 50)), Size::new(100, 80));
    }

    #[test]
    fn aspect_ratio_adjusts_height_after_other_constraints() {
        let square = AspectRatio::new(1, 1).expect("positive ratio");
        let hints = SizeHints {
            aspect: AspectRange::new(square, square),
            ..SizeHints::default()
        };
        assert_eq!(hints.constrain(Size::new(400, 100)), Size::new(400, 400));
        assert_eq!(hints.constrain(Size::new(80, 300)), Size::new(80, 80));
    }

    #[test]
    fn aspect_ratio_is_applied_to_content_above_the_base_size() {
        let ratio = AspectRatio::new(2, 1).expect("positive ratio");
        let hints = SizeHints {
            base: Some(Size {
                width: 10,
                height: 20,
            }),
            aspect: AspectRange::new(ratio, ratio),
            ..SizeHints::default()
        };
        assert_eq!(hints.constrain(Size::new(110, 220)), Size::new(110, 70));
    }

    #[test]
    fn south_east_gravity_keeps_the_bottom_right_anchor_fixed() {
        let geometry = Geometry::new(100, 80, 400, 100);
        assert_eq!(
            Gravity::SouthEast.adjust_resize(geometry, Size::new(600, 160), false, false),
            (-100, 20)
        );
    }

    #[test]
    fn explicit_coordinates_override_gravity_on_their_axis() {
        let geometry = Geometry::new(100, 80, 400, 100);
        assert_eq!(
            Gravity::Center.adjust_resize(geometry, Size::new(600, 160), true, false),
            (100, 50)
        );
    }

    #[test]
    fn modal_transient_redirects_parent_focus() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.manage(client(2));
        clients.set_relationships(
            ClientId::new(2),
            Some(TransientTarget::Client(ClientId::new(1))),
            None,
            true,
        );
        assert_eq!(
            clients.focus_target(ClientId::new(1)),
            Some(ClientId::new(2))
        );
    }

    #[test]
    fn group_modal_redirects_focus_for_group_members() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.manage(client(2));
        let group = Some(ClientId::new(99));
        clients.set_relationships(ClientId::new(1), None, group, false);
        clients.set_relationships(ClientId::new(2), Some(TransientTarget::Group), group, true);
        assert_eq!(
            clients.focus_target(ClientId::new(1)),
            Some(ClientId::new(2))
        );
    }

    #[test]
    fn cyclic_modal_relationships_terminate() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.manage(client(2));
        clients.set_relationships(
            ClientId::new(1),
            Some(TransientTarget::Client(ClientId::new(2))),
            None,
            true,
        );
        clients.set_relationships(
            ClientId::new(2),
            Some(TransientTarget::Client(ClientId::new(1))),
            None,
            true,
        );
        assert!(clients.focus_target(ClientId::new(1)).is_some());
    }

    #[test]
    fn iconifying_focused_client_falls_back_to_visible_history() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.manage(client(2));
        clients.focus(ClientId::new(1));
        clients.focus(ClientId::new(2));
        clients.set_iconic(ClientId::new(2), true);
        assert_eq!(clients.focused(), Some(ClientId::new(1)));
        assert_eq!(clients.focus_target(ClientId::new(2)), None);
    }

    #[test]
    fn iconified_modal_does_not_block_its_parent() {
        let mut clients = ClientSet::default();
        clients.manage(client(1));
        clients.manage(client(2));
        clients.set_relationships(
            ClientId::new(2),
            Some(TransientTarget::Client(ClientId::new(1))),
            None,
            true,
        );
        clients.set_iconic(ClientId::new(2), true);
        assert_eq!(
            clients.focus_target(ClientId::new(1)),
            Some(ClientId::new(1))
        );
    }
}
