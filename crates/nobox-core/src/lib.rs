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

/// Operations the policy engine permits for a client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientCapabilities {
    /// Whether the client can be moved interactively.
    pub movable: bool,
    /// Whether the client can be resized interactively.
    pub resizable: bool,
    /// Whether the client can be minimized.
    pub minimizable: bool,
    /// Whether the client can be maximized.
    pub maximizable: bool,
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

impl ClientPolicy {
    /// Returns the default policy for a functional client role.
    #[must_use]
    pub const fn for_role(role: ClientRole) -> Self {
        let standard_capabilities = ClientCapabilities {
            movable: true,
            resizable: true,
            minimizable: true,
            maximizable: true,
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
                    movable: true,
                    resizable: false,
                    minimizable: false,
                    maximizable: false,
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
                    movable: false,
                    resizable: false,
                    minimizable: false,
                    maximizable: false,
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
    /// Active maximize axes and their restore geometry.
    pub maximize: Option<MaximizeState>,
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
#[derive(Debug, Default)]
pub struct ClientSet {
    clients: BTreeMap<ClientId, Client>,
    management_order: Vec<ClientId>,
    stacking: Vec<ClientId>,
    focus_order: Vec<ClientId>,
    focused: Option<ClientId>,
}

impl ClientSet {
    /// Adds a client, or refreshes its geometry if it is already managed.
    ///
    /// Returns `true` only when a new client was added.
    pub fn manage(&mut self, client: Client) -> bool {
        if let Some(existing) = self.clients.get_mut(&client.id) {
            existing.geometry = client.geometry;
            existing.size_hints = client.size_hints;
            existing.gravity = client.gravity;
            existing.policy = client.policy;
            existing.transient_for = client.transient_for;
            existing.group = client.group;
            existing.modal = client.modal;
            existing.iconic = client.iconic;
            existing.maximize = client.maximize;
            return false;
        }

        self.management_order.push(client.id);
        self.stacking.push(client.id);
        self.focus_order.push(client.id);
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
        self.focus_order.retain(|candidate| *candidate != id);
        if self.focused == Some(id) {
            self.focused = self.focus_order.iter().rev().copied().find(|candidate| {
                self.clients
                    .get(candidate)
                    .is_some_and(|client| !client.iconic)
            });
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
        if !self.clients.contains_key(&id) {
            return false;
        }
        self.focus_order.retain(|candidate| *candidate != id);
        self.focus_order.push(id);
        self.focused = Some(id);
        true
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
        client.transient_for = transient_for
            .filter(|target| !matches!(target, TransientTarget::Client(parent) if *parent == id));
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
            self.focused = self.focus_order.iter().rev().copied().find(|candidate| {
                *candidate != id
                    && self
                        .clients
                        .get(candidate)
                        .is_some_and(|client| !client.iconic)
            });
        }
        true
    }

    /// Resolves a focus request through the topmost modal transient chain.
    #[must_use]
    pub fn focus_target(&self, requested: ClientId) -> Option<ClientId> {
        if self
            .clients
            .get(&requested)
            .is_none_or(|client| client.iconic)
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
            maximize: None,
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
