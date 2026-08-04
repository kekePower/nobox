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
            self.focused = self.focus_order.last().copied();
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

    /// Updates a managed client's geometry.
    pub fn set_geometry(&mut self, id: ClientId, geometry: Geometry) -> bool {
        let Some(client) = self.clients.get_mut(&id) else {
            return false;
        };
        client.geometry = geometry;
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
}
