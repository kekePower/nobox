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
        result
    }
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
}
