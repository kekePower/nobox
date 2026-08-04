//! X11 window-manager backend.

use nobox_config::{Config, MouseModifier, RgbColor};
use nobox_core::{Client, ClientId, ClientSet, Geometry};
use thiserror::Error;
use tracing::{debug, info, warn};
use x11rb::{
    COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE,
    connection::Connection,
    errors::{ConnectError, ConnectionError, ReplyError, ReplyOrIdError},
    protocol::{
        Event,
        xproto::{
            AtomEnum, ButtonIndex, ButtonPressEvent, ChangeWindowAttributesAux, ConfigWindow,
            ConfigureRequestEvent, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
            EventMask, GrabMode, InputFocus, MapState, ModMask, StackMode, Window, WindowClass,
        },
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        UTF8_STRING,
        WM_STATE,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST,
        _NET_CLIENT_LIST_STACKING,
        _NET_CURRENT_DESKTOP,
        _NET_NUMBER_OF_DESKTOPS,
        _NET_SUPPORTED,
        _NET_SUPPORTING_WM_CHECK,
        _NET_WM_NAME,
    }
}

fn root_events() -> EventMask {
    EventMask::SUBSTRUCTURE_REDIRECT
        | EventMask::SUBSTRUCTURE_NOTIFY
        | EventMask::STRUCTURE_NOTIFY
        | EventMask::PROPERTY_CHANGE
        | EventMask::BUTTON_PRESS
        | EventMask::BUTTON_RELEASE
}

fn client_events() -> EventMask {
    EventMask::STRUCTURE_NOTIFY
        | EventMask::PROPERTY_CHANGE
        | EventMask::FOCUS_CHANGE
        | EventMask::BUTTON_PRESS
}

/// A running connection that owns the X11 window-manager selection.
pub struct WindowManager {
    connection: RustConnection,
    screen_index: usize,
    root: Window,
    support_window: Window,
    atoms: Atoms,
    config: Config,
    clients: ClientSet,
    border_pixels: BorderPixels,
    drag: Option<Drag>,
}

impl WindowManager {
    /// Connects to an X server and claims its root window.
    ///
    /// No replacement attempt is made: starting nobox inside another window
    /// manager fails safely.
    ///
    /// # Errors
    ///
    /// Returns an error if the display cannot be reached, another manager owns
    /// the root, or X11 setup fails.
    pub fn connect(display: Option<&str>, config: Config) -> Result<Self, X11Error> {
        let (connection, screen_index) = x11rb::connect(display)?;
        let screen = connection
            .setup()
            .roots
            .get(screen_index)
            .ok_or(X11Error::InvalidScreen(screen_index))?;
        let root = screen.root;
        let colormap = screen.default_colormap;

        let claim = connection.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(root_events()),
        )?;
        if let Err(error) = claim.check() {
            return Err(X11Error::RootClaim(error));
        }

        let atoms = Atoms::new(&connection)?.reply()?;
        let support_window = connection.generate_id()?;
        connection.create_window(
            COPY_DEPTH_FROM_PARENT,
            support_window,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;

        let border_pixels = BorderPixels {
            active: allocate_color(&connection, colormap, config.theme.active_border)?,
            inactive: allocate_color(&connection, colormap, config.theme.inactive_border)?,
        };

        let mut wm = Self {
            connection,
            screen_index,
            root,
            support_window,
            atoms,
            config,
            clients: ClientSet::default(),
            border_pixels,
            drag: None,
        };
        wm.publish_identity()?;
        wm.grab_mouse_actions()?;
        wm.manage_existing_windows()?;
        wm.connection.flush()?;
        Ok(wm)
    }

    /// Processes X11 events until the connection closes or a fatal error occurs.
    ///
    /// # Errors
    ///
    /// Returns an error when communication with the X server fails.
    pub fn run(mut self) -> Result<(), X11Error> {
        info!(
            display = ?self.connection.setup().vendor,
            screen = self.screen_index,
            root = format_args!("{:#x}", self.root),
            "nobox owns the X11 root window"
        );
        loop {
            let event = self.connection.wait_for_event()?;
            self.handle_event(event)?;
            self.connection.flush()?;
        }
    }

    fn publish_identity(&self) -> Result<(), X11Error> {
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_SUPPORTING_WM_CHECK,
            AtomEnum::WINDOW,
            &[self.support_window],
        )?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.support_window,
            self.atoms._NET_SUPPORTING_WM_CHECK,
            AtomEnum::WINDOW,
            &[self.support_window],
        )?;
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.support_window,
            self.atoms._NET_WM_NAME,
            self.atoms.UTF8_STRING,
            b"nobox",
        )?;

        let supported = [
            self.atoms._NET_ACTIVE_WINDOW,
            self.atoms._NET_CLIENT_LIST,
            self.atoms._NET_CLIENT_LIST_STACKING,
            self.atoms._NET_CURRENT_DESKTOP,
            self.atoms._NET_NUMBER_OF_DESKTOPS,
            self.atoms._NET_SUPPORTING_WM_CHECK,
            self.atoms._NET_WM_NAME,
        ];
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_SUPPORTED,
            AtomEnum::ATOM,
            &supported,
        )?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_NUMBER_OF_DESKTOPS,
            AtomEnum::CARDINAL,
            &[1],
        )?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_CURRENT_DESKTOP,
            AtomEnum::CARDINAL,
            &[0],
        )?;
        self.update_client_lists()
    }

    fn grab_mouse_actions(&self) -> Result<(), X11Error> {
        let modifier = self.modifier_mask();
        for button in [
            self.config.mouse.move_button,
            self.config.mouse.resize_button,
        ] {
            self.connection.grab_button(
                false,
                self.root,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::BUTTON_MOTION,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                NONE,
                NONE,
                ButtonIndex::from(button),
                modifier,
            )?;
        }
        Ok(())
    }

    fn modifier_mask(&self) -> ModMask {
        match self.config.mouse.modifier {
            MouseModifier::Alt => ModMask::M1,
            MouseModifier::Super => ModMask::M4,
        }
    }

    fn manage_existing_windows(&mut self) -> Result<(), X11Error> {
        let children = self.connection.query_tree(self.root)?.reply()?.children;
        for window in children {
            let attributes = self.connection.get_window_attributes(window)?.reply()?;
            if !attributes.override_redirect && attributes.map_state != MapState::UNMAPPED {
                self.manage(window, false)?;
            }
        }
        Ok(())
    }

    fn manage(&mut self, window: Window, map: bool) -> Result<(), X11Error> {
        let attributes = self.connection.get_window_attributes(window)?.reply()?;
        if attributes.override_redirect {
            if map {
                self.connection.map_window(window)?;
            }
            return Ok(());
        }

        let geometry = self.connection.get_geometry(window)?.reply()?;
        let id = client_id(window);
        let is_new = self.clients.manage(Client {
            id,
            geometry: Geometry::new(
                i32::from(geometry.x),
                i32::from(geometry.y),
                u32::from(geometry.width),
                u32::from(geometry.height),
            ),
        });

        self.connection.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new()
                .event_mask(client_events())
                .border_pixel(self.border_pixels.inactive),
        )?;
        self.connection.configure_window(
            window,
            &ConfigureWindowAux::new().border_width(self.config.theme.border_width),
        )?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms.WM_STATE,
            self.atoms.WM_STATE,
            &[1, NONE],
        )?;
        if map {
            self.connection.map_window(window)?;
        }

        if is_new {
            info!(window = format_args!("{window:#x}"), "managing X11 client");
            self.update_client_lists()?;
        }
        if self.config.focus.focus_new {
            self.focus(window)?;
        }
        Ok(())
    }

    fn unmanage(&mut self, window: Window) -> Result<(), X11Error> {
        if !self.clients.unmanage(client_id(window)) {
            return Ok(());
        }
        info!(
            window = format_args!("{window:#x}"),
            "unmanaging X11 client"
        );
        self.update_client_lists()?;
        if let Some(focused) = self.clients.focused() {
            self.focus(window_id(focused))?;
        } else {
            self.connection
                .set_input_focus(InputFocus::POINTER_ROOT, self.root, CURRENT_TIME)?;
            self.connection
                .delete_property(self.root, self.atoms._NET_ACTIVE_WINDOW)?;
        }
        Ok(())
    }

    fn focus(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        if !self.clients.focus(id) {
            return Ok(());
        }

        for client in self.clients.stacking() {
            let pixel = if client == id {
                self.border_pixels.active
            } else {
                self.border_pixels.inactive
            };
            self.connection.change_window_attributes(
                window_id(client),
                &ChangeWindowAttributesAux::new().border_pixel(pixel),
            )?;
        }
        self.connection
            .set_input_focus(InputFocus::POINTER_ROOT, window, CURRENT_TIME)?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_ACTIVE_WINDOW,
            AtomEnum::WINDOW,
            &[window],
        )?;
        if self.config.focus.raise_on_focus {
            self.clients.raise(id);
            self.connection.configure_window(
                window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
            self.update_client_lists()?;
        }
        Ok(())
    }

    fn update_client_lists(&self) -> Result<(), X11Error> {
        let managed = self
            .clients
            .management_order()
            .map(window_id)
            .collect::<Vec<_>>();
        let stacking = self.clients.stacking().map(window_id).collect::<Vec<_>>();
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_CLIENT_LIST,
            AtomEnum::WINDOW,
            &managed,
        )?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_CLIENT_LIST_STACKING,
            AtomEnum::WINDOW,
            &stacking,
        )?;
        Ok(())
    }

    fn handle_event(&mut self, event: Event) -> Result<(), X11Error> {
        match event {
            Event::MapRequest(event) => self.manage(event.window, true)?,
            Event::ConfigureRequest(event) => self.configure_request(&event)?,
            Event::DestroyNotify(event) => self.unmanage(event.window)?,
            Event::UnmapNotify(event) => self.unmanage(event.window)?,
            Event::ButtonPress(event) => self.button_press(&event)?,
            Event::MotionNotify(event) => self.pointer_motion(event.root_x, event.root_y)?,
            Event::ButtonRelease(_) => self.drag = None,
            Event::MappingNotify(_) => {
                debug!("X11 input mapping changed; refreshing pointer grabs");
                self.grab_mouse_actions()?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_ACTIVE_WINDOW
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.focus(event.window)?;
            }
            Event::Error(error) => warn!(?error, "non-fatal X11 protocol error"),
            _ => {}
        }
        Ok(())
    }

    fn configure_request(&mut self, event: &ConfigureRequestEvent) -> Result<(), X11Error> {
        let mut values = ConfigureWindowAux::new();
        if event.value_mask.contains(ConfigWindow::X) {
            values = values.x(i32::from(event.x));
        }
        if event.value_mask.contains(ConfigWindow::Y) {
            values = values.y(i32::from(event.y));
        }
        if event.value_mask.contains(ConfigWindow::WIDTH) {
            values = values.width(u32::from(event.width).max(1));
        }
        if event.value_mask.contains(ConfigWindow::HEIGHT) {
            values = values.height(u32::from(event.height).max(1));
        }
        if event.value_mask.contains(ConfigWindow::BORDER_WIDTH) {
            values = values.border_width(u32::from(event.border_width));
        }
        if event.value_mask.contains(ConfigWindow::SIBLING) {
            values = values.sibling(event.sibling);
        }
        if event.value_mask.contains(ConfigWindow::STACK_MODE) {
            values = values.stack_mode(event.stack_mode);
        }
        self.connection.configure_window(event.window, &values)?;

        if self.clients.contains(client_id(event.window)) {
            let geometry = self.connection.get_geometry(event.window)?.reply()?;
            self.clients.set_geometry(
                client_id(event.window),
                Geometry::new(
                    i32::from(geometry.x),
                    i32::from(geometry.y),
                    u32::from(geometry.width),
                    u32::from(geometry.height),
                ),
            );
        }
        Ok(())
    }

    fn button_press(&mut self, event: &ButtonPressEvent) -> Result<(), X11Error> {
        let window = if event.child != NONE {
            event.child
        } else {
            event.event
        };
        let id = client_id(window);
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        self.focus(window)?;

        let modifier = u16::from(self.modifier_mask());
        if u16::from(event.state) & modifier == 0 {
            return Ok(());
        }
        let kind = if event.detail == self.config.mouse.move_button {
            DragKind::Move
        } else if event.detail == self.config.mouse.resize_button {
            DragKind::Resize
        } else {
            return Ok(());
        };
        self.drag = Some(Drag {
            window,
            kind,
            pointer_x: event.root_x,
            pointer_y: event.root_y,
            initial: client.geometry,
        });
        Ok(())
    }

    fn pointer_motion(&mut self, root_x: i16, root_y: i16) -> Result<(), X11Error> {
        let Some(drag) = self.drag else {
            return Ok(());
        };
        let dx = i32::from(root_x) - i32::from(drag.pointer_x);
        let dy = i32::from(root_y) - i32::from(drag.pointer_y);
        let geometry = match drag.kind {
            DragKind::Move => Geometry::new(
                drag.initial.x.saturating_add(dx),
                drag.initial.y.saturating_add(dy),
                drag.initial.width,
                drag.initial.height,
            ),
            DragKind::Resize => Geometry::new(
                drag.initial.x,
                drag.initial.y,
                resize_dimension(drag.initial.width, dx),
                resize_dimension(drag.initial.height, dy),
            ),
        };
        self.connection.configure_window(
            drag.window,
            &ConfigureWindowAux::new()
                .x(geometry.x)
                .y(geometry.y)
                .width(geometry.width)
                .height(geometry.height),
        )?;
        self.clients.set_geometry(client_id(drag.window), geometry);
        Ok(())
    }
}

impl Drop for WindowManager {
    fn drop(&mut self) {
        let _ = self
            .connection
            .ungrab_button(ButtonIndex::ANY, self.root, ModMask::ANY);
        let _ = self
            .connection
            .delete_property(self.root, self.atoms._NET_SUPPORTING_WM_CHECK);
        let _ = self
            .connection
            .delete_property(self.root, self.atoms._NET_SUPPORTED);
        let _ = self
            .connection
            .delete_property(self.root, self.atoms._NET_ACTIVE_WINDOW);
        let _ = self
            .connection
            .delete_property(self.root, self.atoms._NET_CLIENT_LIST);
        let _ = self
            .connection
            .delete_property(self.root, self.atoms._NET_CLIENT_LIST_STACKING);
        let _ = self.connection.destroy_window(self.support_window);
        let _ = self.connection.flush();
    }
}

#[derive(Clone, Copy)]
struct BorderPixels {
    active: u32,
    inactive: u32,
}

#[derive(Clone, Copy)]
struct Drag {
    window: Window,
    kind: DragKind,
    pointer_x: i16,
    pointer_y: i16,
    initial: Geometry,
}

#[derive(Clone, Copy)]
enum DragKind {
    Move,
    Resize,
}

fn client_id(window: Window) -> ClientId {
    ClientId::new(u64::from(window))
}

fn window_id(client: ClientId) -> Window {
    u32::try_from(client.raw()).expect("X11 window identifiers are always 32-bit")
}

fn resize_dimension(initial: u32, delta: i32) -> u32 {
    let value = i64::from(initial).saturating_add(i64::from(delta));
    u32::try_from(value.max(1)).unwrap_or(u32::MAX)
}

fn allocate_color(
    connection: &RustConnection,
    colormap: u32,
    color: RgbColor,
) -> Result<u32, X11Error> {
    let pixel = color.pixel();
    let red = u16::try_from((pixel >> 16) & 0xff).expect("masked channel") * 257;
    let green = u16::try_from((pixel >> 8) & 0xff).expect("masked channel") * 257;
    let blue = u16::try_from(pixel & 0xff).expect("masked channel") * 257;
    Ok(connection
        .alloc_color(colormap, red, green, blue)?
        .reply()?
        .pixel)
}

/// Failures encountered while owning or serving an X11 display.
#[derive(Debug, Error)]
pub enum X11Error {
    /// The display connection could not be established.
    #[error("could not connect to the X11 display")]
    Connect(#[from] ConnectError),
    /// The selected screen index was absent from the X11 setup.
    #[error("X11 server did not advertise screen {0}")]
    InvalidScreen(usize),
    /// Another manager already selected substructure redirection.
    #[error("could not claim the X11 root window (is another window manager running?): {0}")]
    RootClaim(ReplyError),
    /// The X11 connection failed after setup.
    #[error("X11 connection failed")]
    Connection(#[from] ConnectionError),
    /// An X11 request returned an error.
    #[error("X11 request failed")]
    Reply(#[from] ReplyError),
    /// An X11 request or resource allocation failed.
    #[error("X11 request or resource allocation failed")]
    ReplyOrId(#[from] ReplyOrIdError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizing_clamps_at_one_pixel() {
        assert_eq!(resize_dimension(10, -50), 1);
        assert_eq!(resize_dimension(10, 5), 15);
    }
}
