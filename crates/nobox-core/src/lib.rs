//! Protocol-neutral window-management state.

use std::collections::BTreeMap;

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
    /// Resolves visible decoration space against theme dimensions.
    #[must_use]
    pub const fn extents(self, border_width: u32, titlebar_height: u32) -> DecorationExtents {
        let border = if self.border { border_width } else { 0 };
        let titlebar = if self.titlebar { titlebar_height } else { 0 };
        DecorationExtents::new(border, border, border.saturating_add(titlebar), border)
    }
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
    u128::from(left.numerator) * u128::from(right.denominator)
        <= u128::from(right.numerator) * u128::from(left.denominator)
}

fn constrain_aspect(size: Size, base: Size, range: AspectRange) -> Size {
    let width = size.width.saturating_sub(base.width).max(1);
    let mut height = size.height.saturating_sub(base.height).max(1);

    if u128::from(height) * u128::from(range.minimum.numerator)
        > u128::from(width) * u128::from(range.minimum.denominator)
    {
        height = scaled_height(width, range.minimum);
    }
    if u128::from(height) * u128::from(range.maximum.numerator)
        < u128::from(width) * u128::from(range.maximum.denominator)
    {
        height = scaled_height(width, range.maximum);
    }

    Size::new(
        width.saturating_add(base.width),
        height.saturating_add(base.height),
    )
}

fn scaled_height(width: u32, ratio: AspectRatio) -> u32 {
    let value = u128::from(width) * u128::from(ratio.denominator) / u128::from(ratio.numerator);
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
    /// Client or application group this client is transient for.
    pub transient_for: Option<TransientTarget>,
    /// Application group identifier, which need not be a managed client.
    pub group: Option<ClientId>,
    /// Whether this transient blocks interaction with its parent or group.
    pub modal: bool,
    /// Whether the client is managed but intentionally not mapped.
    pub iconic: bool,
    /// Policy workspace membership, independent of display protocol.
    pub workspace: WorkspaceAssignment,
    /// User-requested stacking preference.
    pub layer: ClientLayer,
    /// Active maximize axes and their restore geometry.
    pub maximize: Option<MaximizeState>,
    /// Active fullscreen state and its restore geometry.
    pub fullscreen: Option<FullscreenState>,
}

impl Client {
    /// Resolves the effective stacking layer from role and requested state.
    #[must_use]
    pub const fn stacking_layer(self) -> StackingLayer {
        if self.fullscreen.is_some() {
            return StackingLayer::Fullscreen;
        }
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
    workspace_layout: WorkspaceLayout,
    focus_order: BTreeMap<WorkspaceId, Vec<ClientId>>,
    focused: Option<ClientId>,
}

impl Default for ClientSet {
    fn default() -> Self {
        Self {
            clients: BTreeMap::new(),
            management_order: Vec::new(),
            stacking: Vec::new(),
            workspace_count: 1,
            current_workspace: WorkspaceId::default(),
            workspace_layout: WorkspaceLayout::one_row(1),
            focus_order: BTreeMap::new(),
            focused: None,
        }
    }
}

impl ClientSet {
    /// Adds a client, or refreshes its geometry if it is already managed.
    ///
    /// Returns `true` only when a new client was added.
    pub fn manage(&mut self, client: Client) -> bool {
        let mut client = client;
        client.workspace = self.valid_assignment(client.workspace);
        if let Some(existing) = self.clients.get_mut(&client.id) {
            let previous_workspace = existing.workspace;
            existing.geometry = client.geometry;
            existing.size_hints = client.size_hints;
            existing.gravity = client.gravity;
            existing.policy = client.policy;
            existing.transient_for = client.transient_for;
            existing.group = client.group;
            existing.modal = client.modal;
            existing.iconic = client.iconic;
            existing.workspace = client.workspace;
            existing.layer = client.layer;
            existing.maximize = client.maximize;
            existing.fullscreen = client.fullscreen;
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
                || !self.is_visible_client(*client)
        }) {
            return false;
        }
        let history = self.focus_order.entry(self.current_workspace).or_default();
        history.retain(|candidate| *candidate != id);
        history.push(id);
        self.focused = Some(id);
        true
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

    /// Switches to a valid workspace and restores its most recent focus.
    pub fn switch_workspace(&mut self, workspace: WorkspaceId) -> bool {
        if workspace.index() >= self.workspace_count || workspace == self.current_workspace {
            return false;
        }
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

    /// Moves a client and its specific transient family as one policy unit.
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
            .is_some_and(|client| self.is_visible_client(*client))
    }

    /// Marks a managed client highest in the stacking order.
    pub fn raise(&mut self, id: ClientId) -> bool {
        if !self.clients.contains_key(&id) {
            return false;
        }
        self.stacking.retain(|candidate| *candidate != id);
        self.stacking.push(id);
        true
    }

    /// Replaces stacking order with a backend-observed bottom-to-top order.
    ///
    /// Unknown and duplicate identifiers are discarded. Managed clients absent
    /// from `order` retain their previous relative order at the bottom.
    pub fn sync_stacking(&mut self, order: impl IntoIterator<Item = ClientId>) {
        let mut seen = std::collections::BTreeSet::new();
        let observed = order
            .into_iter()
            .filter(|id| self.clients.contains_key(id) && seen.insert(*id))
            .collect::<Vec<_>>();
        let mut stacking = Vec::with_capacity(self.stacking.len());
        stacking.extend(
            self.stacking
                .iter()
                .copied()
                .filter(|id| !seen.contains(id)),
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
        client.policy = policy;
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

    /// Resolves a focus request through the topmost modal transient chain.
    #[must_use]
    pub fn focus_target(&self, requested: ClientId) -> Option<ClientId> {
        if self
            .clients
            .get(&requested)
            .is_none_or(|client| client.iconic || !self.is_visible_client(*client))
        {
            return None;
        }
        let mut target = requested;
        let mut visited = std::collections::BTreeSet::new();
        while visited.insert(target) {
            let target_group = self.clients.get(&target).and_then(|client| client.group);
            let modal = self.stacking.iter().rev().copied().find(|candidate| {
                !visited.contains(candidate)
                    && self.clients.get(candidate).is_some_and(|client| {
                        client.modal
                            && !client.iconic
                            && self.is_visible_client(*client)
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
    pub fn policy_stacking(&self) -> Vec<ClientId> {
        let mut ordered = Vec::with_capacity(self.stacking.len());
        let mut visited = std::collections::BTreeSet::new();
        for layer in [
            StackingLayer::Desktop,
            StackingLayer::Below,
            StackingLayer::Normal,
            StackingLayer::Dock,
            StackingLayer::Above,
            StackingLayer::Fullscreen,
        ] {
            for id in self.stacking.iter().copied() {
                if self.effective_stacking_layer(id) == Some(layer) {
                    self.visit_stacking_parent(id, layer, &mut visited, &mut ordered);
                }
            }
        }
        ordered
    }

    /// Resolves a client's layer, inheriting any higher specific-parent layer.
    #[must_use]
    pub fn effective_stacking_layer(&self, id: ClientId) -> Option<StackingLayer> {
        let mut layer = self.clients.get(&id)?.stacking_layer();
        let mut current = id;
        let mut visited = std::collections::BTreeSet::new();
        while visited.insert(current) {
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
            layer = layer.max(parent.stacking_layer());
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

    fn is_visible_client(&self, client: Client) -> bool {
        client.workspace.is_visible_on(self.current_workspace)
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
        self.focused = self
            .focus_order
            .get(&self.current_workspace)
            .into_iter()
            .flatten()
            .rev()
            .copied()
            .find(|candidate| {
                self.clients.get(candidate).is_some_and(|client| {
                    !client.iconic
                        && client.policy.capabilities.focusable
                        && self.is_visible_client(*client)
                })
            })
            .or_else(|| {
                self.stacking.iter().rev().copied().find(|candidate| {
                    self.clients.get(candidate).is_some_and(|client| {
                        !client.iconic
                            && client.policy.capabilities.focusable
                            && self.is_visible_client(*client)
                    })
                })
            });
    }

    fn family_root(&self, id: ClientId) -> Option<ClientId> {
        self.clients.get(&id)?;
        let mut root = id;
        let mut visited = std::collections::BTreeSet::new();
        while visited.insert(root) {
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
        let mut family = Vec::with_capacity(self.clients.len());
        let mut pending = vec![root];
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
        visited: &mut std::collections::BTreeSet<ClientId>,
        ordered: &mut Vec<ClientId>,
    ) {
        if !visited.insert(id) {
            return;
        }
        if let Some(TransientTarget::Client(parent)) = self
            .clients
            .get(&id)
            .and_then(|client| client.transient_for)
            && self.effective_stacking_layer(parent) == Some(layer)
        {
            self.visit_stacking_parent(parent, layer, visited, ordered);
        }
        ordered.push(id);
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
            transient_for: None,
            group: None,
            modal: false,
            iconic: false,
            workspace: WorkspaceAssignment::default(),
            layer: ClientLayer::Normal,
            maximize: None,
            fullscreen: None,
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
    fn decoration_extents_expand_around_content_without_overflow() {
        let extents = DecorationExtents::new(2, 3, 24, 4);
        assert_eq!(
            extents.outer_geometry(Geometry::new(50, 40, 640, 480)),
            Geometry::new(48, 16, 645, 508)
        );
        assert_eq!(extents.content_offset(), (2, 24));
        assert_eq!(
            extents.outer_geometry(Geometry::new(i32::MIN, i32::MIN, u32::MAX, u32::MAX)),
            Geometry::new(i32::MIN, i32::MIN, u32::MAX, u32::MAX)
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
            clients.policy_stacking(),
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
            clients.effective_stacking_layer(ClientId::new(2)),
            Some(StackingLayer::Above)
        );

        clients.set_layer(ClientId::new(2), ClientLayer::Above);
        clients.set_layer(ClientId::new(1), ClientLayer::Normal);
        assert_eq!(
            clients.effective_stacking_layer(ClientId::new(2)),
            Some(StackingLayer::Above)
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

        assert_eq!(clients.policy_stacking().len(), 2);
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
