//! X11 window-manager backend.

use std::{
    collections::BTreeMap,
    process::{Command, Stdio},
};

use nobox_config::{Action, Config, KeyboardModifier, MouseModifier, RgbColor, ThemeConfig};
use nobox_core::{
    AspectRange, AspectRatio, Client, ClientDecorations, ClientId, ClientLayer, ClientPolicy,
    ClientRole, ClientSet, DecorationExtents, EdgeReservation, EdgeReservations, Geometry, Gravity,
    Size, SizeHints, TransientTarget, WorkspaceAssignment, WorkspaceCorner, WorkspaceDirection,
    WorkspaceId, WorkspaceLayout, WorkspaceOrientation,
};
use thiserror::Error;
use tracing::{debug, info, warn};
use x11rb::{
    COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE,
    connection::Connection,
    errors::{ConnectError, ConnectionError, ReplyError, ReplyOrIdError},
    properties::{WmHints, WmHintsState, WmSizeHints},
    protocol::{
        ErrorKind, Event,
        xproto::{
            AtomEnum, ButtonIndex, ButtonPressEvent, CONFIGURE_NOTIFY_EVENT, ChangeGCAux,
            ChangeWindowAttributesAux, ClientMessageEvent, ConfigWindow, ConfigureNotifyEvent,
            ConfigureRequestEvent, ConfigureWindowAux, ConnectionExt as _, CreateGCAux,
            CreateWindowAux, EventMask, Font, Gcontext, Grab, GrabMode, GrabStatus, InputFocus,
            KeyPressEvent, MapState, ModMask, SetMode, StackMode, UnmapNotifyEvent, Window,
            WindowClass,
        },
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        UTF8_STRING,
        MANAGER,
        WM_DELETE_WINDOW,
        WM_CHANGE_STATE,
        WM_PROTOCOLS,
        WM_STATE,
        WM_TAKE_FOCUS,
        WM_TRANSIENT_FOR,
        _MOTIF_WM_HINTS,
        _NOBOX_CONTROL,
        _NOBOX_TIMESTAMP,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST,
        _NET_CLIENT_LIST_STACKING,
        _NET_CURRENT_DESKTOP,
        _NET_DESKTOP_GEOMETRY,
        _NET_DESKTOP_LAYOUT,
        _NET_DESKTOP_NAMES,
        _NET_DESKTOP_VIEWPORT,
        _NET_FRAME_EXTENTS,
        _NET_NUMBER_OF_DESKTOPS,
        _NET_REQUEST_FRAME_EXTENTS,
        _NET_RESTACK_WINDOW,
        _NET_SUPPORTED,
        _NET_SUPPORTING_WM_CHECK,
        _NET_WORKAREA,
        _NET_WM_NAME,
        _NET_WM_DESKTOP,
        _NET_WM_STATE,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_BELOW,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MODAL,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_COMBO,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_WINDOW_TYPE_DIALOG,
        _NET_WM_WINDOW_TYPE_DND,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_DROPDOWN_MENU,
        _NET_WM_WINDOW_TYPE_MENU,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _NET_WM_WINDOW_TYPE_NOTIFICATION,
        _NET_WM_WINDOW_TYPE_POPUP_MENU,
        _NET_WM_WINDOW_TYPE_SPLASH,
        _NET_WM_WINDOW_TYPE_TOOLBAR,
        _NET_WM_WINDOW_TYPE_TOOLTIP,
        _NET_WM_WINDOW_TYPE_UTILITY,
        _NET_WM_STRUT,
        _NET_WM_STRUT_PARTIAL,
    }
}

fn root_events() -> EventMask {
    EventMask::SUBSTRUCTURE_REDIRECT
        | EventMask::SUBSTRUCTURE_NOTIFY
        | EventMask::STRUCTURE_NOTIFY
        | EventMask::PROPERTY_CHANGE
        | EventMask::BUTTON_PRESS
        | EventMask::BUTTON_RELEASE
        | EventMask::KEY_PRESS
}

fn client_events() -> EventMask {
    EventMask::STRUCTURE_NOTIFY
        | EventMask::PROPERTY_CHANGE
        | EventMask::FOCUS_CHANGE
        | EventMask::BUTTON_PRESS
}

const WM_STATE_NORMAL: u32 = 1;
const WM_STATE_ICONIC: u32 = 3;
const MOTIF_FLAG_FUNCTIONS: u32 = 1 << 0;
const MOTIF_FLAG_DECORATIONS: u32 = 1 << 1;
const MOTIF_FUNCTION_ALL: u32 = 1 << 0;
const MOTIF_FUNCTION_RESIZE: u32 = 1 << 1;
const MOTIF_FUNCTION_MOVE: u32 = 1 << 2;
const MOTIF_DECORATION_ALL: u32 = 1 << 0;
const MOTIF_DECORATION_BORDER: u32 = 1 << 1;
const MOTIF_DECORATION_HANDLE: u32 = 1 << 2;
const MOTIF_DECORATION_TITLE: u32 = 1 << 3;
const CONTROL_RELOAD: u32 = 1;
const CONTROL_SHUTDOWN: u32 = 2;

/// A separate X11 connection used to wake and control a running [`WindowManager`].
pub struct ControlSender {
    connection: RustConnection,
    window: Window,
    atom: u32,
}

impl ControlSender {
    /// Requests an in-place configuration reload.
    ///
    /// # Errors
    ///
    /// Returns an error when the control event cannot be delivered.
    pub fn reload(&self) -> Result<(), X11Error> {
        self.send(CONTROL_RELOAD)
    }

    /// Requests a clean window-manager shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error when the control event cannot be delivered.
    pub fn shutdown(&self) -> Result<(), X11Error> {
        self.send(CONTROL_SHUTDOWN)
    }

    fn send(&self, request: u32) -> Result<(), X11Error> {
        let message = ClientMessageEvent::new(32, self.window, self.atom, [request, 0, 0, 0, 0]);
        self.connection
            .send_event(false, self.window, EventMask::NO_EVENT, message)?
            .check()?;
        self.connection.flush()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeRequest {
    Reload,
    Shutdown,
}

/// A running connection that owns the X11 window-manager selection.
pub struct WindowManager {
    connection: RustConnection,
    screen_index: usize,
    root: Window,
    support_window: Window,
    wm_selection: u32,
    desktop_layout_selection: u32,
    atoms: Atoms,
    config: Config,
    clients: ClientSet,
    titles: BTreeMap<ClientId, String>,
    struts: BTreeMap<ClientId, EdgeReservations>,
    work_areas: Vec<Geometry>,
    frames: BTreeMap<ClientId, Frame>,
    frame_parts: BTreeMap<Window, FramePart>,
    decoration_pixels: DecorationPixels,
    title_font: Font,
    title_gc: Gcontext,
    key_bindings: BTreeMap<(u8, u16), Action>,
    escape_keycodes: Vec<u8>,
    ignored_modifiers: u16,
    drag: Option<Drag>,
    expected_unmaps: BTreeMap<Window, u8>,
    last_timestamp: u32,
    running: bool,
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
        let screen_geometry = Geometry::new(
            0,
            0,
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        );

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
        let timestamp = server_timestamp(&connection, support_window, atoms._NOBOX_TIMESTAMP)?;

        let claim = connection.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(root_events()),
        )?;
        if let Err(error) = claim.check() {
            return Err(X11Error::RootClaim(error));
        }

        let selection_name = format!("WM_S{screen_index}");
        let wm_selection = connection
            .intern_atom(false, selection_name.as_bytes())?
            .reply()?
            .atom;
        let desktop_layout_selection_name = format!("_NET_DESKTOP_LAYOUT_S{screen_index}");
        let desktop_layout_selection = connection
            .intern_atom(false, desktop_layout_selection_name.as_bytes())?
            .reply()?
            .atom;
        connection
            .set_selection_owner(support_window, wm_selection, timestamp)?
            .check()?;
        let owner = connection.get_selection_owner(wm_selection)?.reply()?.owner;
        if owner != support_window {
            return Err(X11Error::SelectionClaim(selection_name));
        }

        let decoration_pixels = DecorationPixels::allocate(&connection, colormap, &config.theme)?;
        let title_font = connection.generate_id()?;
        connection.open_font(title_font, b"fixed")?.check()?;
        let title_gc = connection.generate_id()?;
        connection
            .create_gc(
                title_gc,
                root,
                &CreateGCAux::new()
                    .font(title_font)
                    .foreground(decoration_pixels.title_text),
            )?
            .check()?;

        let mut clients = ClientSet::default();
        clients.set_workspace_count(u32::try_from(config.workspaces.names.len()).unwrap_or(1));
        clients.set_workspace_layout(configured_workspace_layout(&config));
        let work_areas =
            vec![screen_geometry; usize::try_from(clients.workspace_count()).unwrap_or(1)];
        let mut wm = Self {
            connection,
            screen_index,
            root,
            support_window,
            wm_selection,
            desktop_layout_selection,
            atoms,
            config,
            clients,
            titles: BTreeMap::new(),
            struts: BTreeMap::new(),
            work_areas,
            frames: BTreeMap::new(),
            frame_parts: BTreeMap::new(),
            decoration_pixels,
            title_font,
            title_gc,
            key_bindings: BTreeMap::new(),
            escape_keycodes: Vec::new(),
            ignored_modifiers: u16::from(ModMask::LOCK),
            drag: None,
            expected_unmaps: BTreeMap::new(),
            last_timestamp: timestamp,
            running: true,
        };
        wm.refresh_workspace_layout()?;
        wm.publish_identity()?;
        wm.reload_input_bindings()?;
        wm.manage_existing_windows()?;
        wm.connection.flush()?;
        Ok(wm)
    }

    /// Opens a dedicated connection that can wake this manager's event loop.
    ///
    /// # Errors
    ///
    /// Returns an error when the display cannot be reached or initialized.
    pub fn control_sender(&self, display: Option<&str>) -> Result<ControlSender, X11Error> {
        let (connection, _) = x11rb::connect(display)?;
        let atom = connection
            .intern_atom(false, b"_NOBOX_CONTROL")?
            .reply()?
            .atom;
        Ok(ControlSender {
            connection,
            window: self.support_window,
            atom,
        })
    }

    /// Processes X11 events and runtime-control requests until clean shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error when communication with the X server fails.
    pub fn run<E>(
        mut self,
        mut load_config: impl FnMut() -> Result<Config, E>,
    ) -> Result<(), X11Error>
    where
        E: std::fmt::Display,
    {
        info!(
            display = ?self.connection.setup().vendor,
            screen = self.screen_index,
            root = format_args!("{:#x}", self.root),
            "nobox owns the X11 root window"
        );
        while self.running {
            let event = self.connection.wait_for_event()?;
            match self.runtime_request(&event) {
                Some(RuntimeRequest::Reload) => match load_config() {
                    Ok(config) => {
                        if let Err(error) = self.reload_config(config) {
                            warn!(%error, "could not apply reloaded configuration");
                        }
                    }
                    Err(error) => warn!(%error, "could not reload configuration"),
                },
                Some(RuntimeRequest::Shutdown) => self.running = false,
                None => {
                    if let Err(error) = self.handle_event(event) {
                        if error.is_vanished_window() {
                            debug!(%error, "ignored event for a vanished X11 window");
                        } else {
                            return Err(error);
                        }
                    }
                }
            }
            self.connection.flush()?;
        }
        info!("nobox X11 event loop stopped cleanly");
        Ok(())
    }

    fn runtime_request(&self, event: &Event) -> Option<RuntimeRequest> {
        let Event::ClientMessage(event) = event else {
            return None;
        };
        if event.window != self.support_window
            || event.type_ != self.atoms._NOBOX_CONTROL
            || event.format != 32
        {
            return None;
        }
        runtime_request_code(event.data.as_data32()[0])
    }

    fn publish_identity(&self) -> Result<(), X11Error> {
        let manager_announcement = ClientMessageEvent::new(
            32,
            self.root,
            self.atoms.MANAGER,
            [
                self.last_timestamp,
                self.wm_selection,
                self.support_window,
                0,
                0,
            ],
        );
        self.connection.send_event(
            false,
            self.root,
            EventMask::STRUCTURE_NOTIFY,
            manager_announcement,
        )?;
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
            self.atoms._NET_DESKTOP_GEOMETRY,
            self.atoms._NET_DESKTOP_LAYOUT,
            self.atoms._NET_DESKTOP_NAMES,
            self.atoms._NET_DESKTOP_VIEWPORT,
            self.atoms._NET_FRAME_EXTENTS,
            self.atoms._NET_NUMBER_OF_DESKTOPS,
            self.atoms._NET_REQUEST_FRAME_EXTENTS,
            self.atoms._NET_RESTACK_WINDOW,
            self.atoms._NET_SUPPORTING_WM_CHECK,
            self.atoms._NET_WORKAREA,
            self.atoms._NET_WM_NAME,
            self.atoms._NET_WM_DESKTOP,
            self.atoms._NET_WM_STATE,
            self.atoms._NET_WM_STATE_ABOVE,
            self.atoms._NET_WM_STATE_BELOW,
            self.atoms._NET_WM_STATE_FULLSCREEN,
            self.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
            self.atoms._NET_WM_STATE_MAXIMIZED_VERT,
            self.atoms._NET_WM_STATE_MODAL,
            self.atoms._NET_WM_WINDOW_TYPE,
            self.atoms._NET_WM_WINDOW_TYPE_COMBO,
            self.atoms._NET_WM_WINDOW_TYPE_DESKTOP,
            self.atoms._NET_WM_WINDOW_TYPE_DIALOG,
            self.atoms._NET_WM_WINDOW_TYPE_DND,
            self.atoms._NET_WM_WINDOW_TYPE_DOCK,
            self.atoms._NET_WM_WINDOW_TYPE_DROPDOWN_MENU,
            self.atoms._NET_WM_WINDOW_TYPE_MENU,
            self.atoms._NET_WM_WINDOW_TYPE_NORMAL,
            self.atoms._NET_WM_WINDOW_TYPE_NOTIFICATION,
            self.atoms._NET_WM_WINDOW_TYPE_POPUP_MENU,
            self.atoms._NET_WM_WINDOW_TYPE_SPLASH,
            self.atoms._NET_WM_WINDOW_TYPE_TOOLBAR,
            self.atoms._NET_WM_WINDOW_TYPE_TOOLTIP,
            self.atoms._NET_WM_WINDOW_TYPE_UTILITY,
            self.atoms._NET_WM_STRUT,
            self.atoms._NET_WM_STRUT_PARTIAL,
        ];
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_SUPPORTED,
            AtomEnum::ATOM,
            &supported,
        )?;
        self.publish_workspaces()?;
        self.update_client_lists()
    }

    fn reload_input_bindings(&mut self) -> Result<(), X11Error> {
        let minimum = self.connection.setup().min_keycode;
        let maximum = self.connection.setup().max_keycode;
        let count = maximum
            .checked_sub(minimum)
            .and_then(|value| value.checked_add(1))
            .ok_or(X11Error::InvalidKeyboardRange { minimum, maximum })?;
        let mapping = self
            .connection
            .get_keyboard_mapping(minimum, count)?
            .reply()?;

        let num_lock_keycodes = keycodes_for_raw_symbol(
            minimum,
            mapping.keysyms_per_keycode,
            &mapping.keysyms,
            xkeysym::key::Num_Lock,
        );
        let modifier_mapping = self.connection.get_modifier_mapping()?.reply()?;
        let keys_per_modifier = usize::from(modifier_mapping.keycodes_per_modifier());
        let num_lock_mask = if keys_per_modifier == 0 {
            0
        } else {
            modifier_mapping
                .keycodes
                .chunks(keys_per_modifier)
                .enumerate()
                .find(|(_, keycodes)| {
                    keycodes
                        .iter()
                        .any(|keycode| num_lock_keycodes.contains(keycode))
                })
                .and_then(|(index, _)| u32::try_from(index).ok())
                .and_then(|index| 1_u16.checked_shl(index))
                .unwrap_or(0)
        };
        self.ignored_modifiers = u16::from(ModMask::LOCK) | num_lock_mask;
        self.escape_keycodes = keycodes_for_named_symbol(
            minimum,
            mapping.keysyms_per_keycode,
            &mapping.keysyms,
            "Escape",
        );
        if self.escape_keycodes.is_empty() {
            return Err(X11Error::UnknownKeySymbol("Escape".to_owned()));
        }

        self.connection
            .ungrab_key(Grab::ANY, self.root, ModMask::ANY)?;
        self.key_bindings.clear();
        for binding in self.config.keyboard.bindings.clone() {
            let keycodes = keycodes_for_named_symbol(
                minimum,
                mapping.keysyms_per_keycode,
                &mapping.keysyms,
                binding.key.symbol(),
            );
            if keycodes.is_empty() {
                return Err(X11Error::UnknownKeySymbol(binding.key.symbol().to_owned()));
            }
            let modifiers = keyboard_modifier_mask(binding.key.modifiers());
            for keycode in keycodes {
                if self
                    .key_bindings
                    .insert((keycode, modifiers), binding.action.clone())
                    .is_some()
                {
                    return Err(X11Error::DuplicateKeyGrab { keycode, modifiers });
                }
                for locks in lock_combinations(self.ignored_modifiers) {
                    self.connection
                        .grab_key(
                            false,
                            self.root,
                            ModMask::from(modifiers | locks),
                            keycode,
                            GrabMode::ASYNC,
                            GrabMode::ASYNC,
                        )?
                        .check()?;
                }
            }
        }
        self.grab_mouse_actions()?;
        info!(
            bindings = self.key_bindings.len(),
            "loaded X11 key bindings"
        );
        Ok(())
    }

    fn grab_mouse_actions(&self) -> Result<(), X11Error> {
        self.connection
            .ungrab_button(ButtonIndex::ANY, self.root, ModMask::ANY)?;
        let modifier = u16::from(self.modifier_mask());
        for button in [
            self.config.mouse.move_button,
            self.config.mouse.resize_button,
        ] {
            for locks in lock_combinations(self.ignored_modifiers) {
                self.connection
                    .grab_button(
                        false,
                        self.root,
                        EventMask::BUTTON_PRESS
                            | EventMask::BUTTON_RELEASE
                            | EventMask::BUTTON_MOTION,
                        GrabMode::ASYNC,
                        GrabMode::ASYNC,
                        NONE,
                        NONE,
                        ButtonIndex::from(button),
                        ModMask::from(modifier | locks),
                    )?
                    .check()?;
            }
        }
        Ok(())
    }

    fn modifier_mask(&self) -> ModMask {
        match self.config.mouse.modifier {
            MouseModifier::Alt => ModMask::M1,
            MouseModifier::Super => ModMask::M4,
        }
    }

    fn reload_config(&mut self, config: Config) -> Result<(), X11Error> {
        if config == self.config {
            info!("configuration reload contained no changes");
            return Ok(());
        }
        self.cancel_drag(self.last_timestamp)?;
        let colormap = self.connection.setup().roots[self.screen_index].default_colormap;
        let new_pixels = DecorationPixels::allocate(&self.connection, colormap, &config.theme)?;
        let previous_config = std::mem::replace(&mut self.config, config);
        if let Err(error) = self.reload_input_bindings() {
            self.config = previous_config;
            self.reload_input_bindings()?;
            self.connection
                .free_colors(colormap, 0, &new_pixels.as_array())?;
            return Err(error);
        }

        let workspaces_changed = previous_config.workspaces != self.config.workspaces;
        if workspaces_changed {
            self.clients.set_workspace_count(
                u32::try_from(self.config.workspaces.names.len()).unwrap_or(1),
            );
            self.refresh_workspace_layout()?;
            for id in self.clients.management_order() {
                if let Some(client) = self.clients.get(id) {
                    self.publish_client_workspace(window_id(id), client.workspace)?;
                }
            }
            let _ = self.refresh_work_area()?;
            self.publish_workspaces()?;
            self.sync_workspace_visibility()?;
            self.restore_workspace_focus(self.last_timestamp)?;
        }

        let previous_pixels = std::mem::replace(&mut self.decoration_pixels, new_pixels);
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new().foreground(self.decoration_pixels.title_text),
        )?;
        let clients = self.clients.stacking().collect::<Vec<_>>();
        for id in clients.iter().copied() {
            let Some(policy) = self.clients.get(id).map(|client| client.policy) else {
                continue;
            };
            if let Err(error) = self.apply_frame_policy(id, policy) {
                if error.is_vanished_window() {
                    debug!(%error, "client vanished while applying reloaded frame policy");
                    continue;
                }
                return Err(error);
            }
        }
        for id in clients {
            if let Err(error) = self
                .refresh_frame_colors(id)
                .and_then(|()| self.draw_title(id))
            {
                if error.is_vanished_window() {
                    debug!(%error, "client vanished while redrawing reloaded theme");
                    continue;
                }
                return Err(error);
            }
        }
        self.connection
            .free_colors(colormap, 0, &previous_pixels.as_array())?;
        info!("reloaded configuration in place");
        Ok(())
    }

    fn refresh_frame_colors(&self, id: ClientId) -> Result<(), X11Error> {
        let Some(frame) = self.frames.get(&id).copied() else {
            return Ok(());
        };
        let (border, titlebar) = if self.clients.focused() == Some(id) {
            (
                self.decoration_pixels.active_border,
                self.decoration_pixels.active_titlebar,
            )
        } else {
            (
                self.decoration_pixels.inactive_border,
                self.decoration_pixels.inactive_titlebar,
            )
        };
        self.connection.change_window_attributes(
            frame.window,
            &ChangeWindowAttributesAux::new()
                .border_pixel(border)
                .background_pixel(titlebar),
        )?;
        for (button, pixel) in [
            (
                frame.minimize_button,
                self.decoration_pixels.minimize_button,
            ),
            (
                frame.maximize_button,
                self.decoration_pixels.maximize_button,
            ),
            (frame.close_button, self.decoration_pixels.close_button),
        ] {
            if let Some(button) = button {
                self.connection.change_window_attributes(
                    button,
                    &ChangeWindowAttributesAux::new().background_pixel(pixel),
                )?;
                self.connection.clear_area(false, button, 0, 0, 0, 0)?;
            }
        }
        Ok(())
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

    fn decoration_extents(&self, policy: ClientPolicy) -> DecorationExtents {
        policy.decorations.extents(
            self.config.theme.border_width,
            self.config.theme.titlebar_height,
        )
    }

    fn publish_frame_extents(
        &self,
        window: Window,
        extents: DecorationExtents,
    ) -> Result<(), X11Error> {
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms._NET_FRAME_EXTENTS,
            AtomEnum::CARDINAL,
            &[extents.left, extents.right, extents.top, extents.bottom],
        )?;
        Ok(())
    }

    fn read_title(&self, window: Window) -> Result<String, X11Error> {
        let modern = self
            .connection
            .get_property(
                false,
                window,
                self.atoms._NET_WM_NAME,
                self.atoms.UTF8_STRING,
                0,
                1024,
            )?
            .reply()?;
        if !modern.value.is_empty() {
            return Ok(String::from_utf8_lossy(&modern.value).into_owned());
        }
        let legacy = self
            .connection
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)?
            .reply()?;
        Ok(legacy.value.into_iter().map(char::from).collect())
    }

    fn refresh_title(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        let title = self.read_title(window)?;
        self.titles.insert(id, title.clone());
        let Some(frame) = self.frames.get(&id).copied() else {
            return Ok(());
        };
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            frame.window,
            self.atoms._NET_WM_NAME,
            self.atoms.UTF8_STRING,
            title.as_bytes(),
        )?;
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            frame.window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            &title_text_bytes(&title, usize::MAX),
        )?;
        self.draw_title(id)
    }

    fn draw_title(&self, id: ClientId) -> Result<(), X11Error> {
        let Some(frame) = self.frames.get(&id).copied() else {
            return Ok(());
        };
        let titlebar_height = frame.extents.top.saturating_sub(frame.extents.left);
        let Some(client) = self.clients.get(id) else {
            return Ok(());
        };
        if titlebar_height == 0 {
            return Ok(());
        }
        self.connection.clear_area(
            false,
            frame.window,
            0,
            0,
            x_dimension(client.geometry.width),
            x_dimension(titlebar_height),
        )?;
        let button_count = u32::from(frame.minimize_button.is_some())
            .saturating_add(u32::from(frame.maximize_button.is_some()))
            .saturating_add(u32::from(frame.close_button.is_some()));
        let button_size = titlebar_height.saturating_sub(8).max(1);
        let available = client
            .geometry
            .width
            .saturating_sub(button_count.saturating_mul(button_size.saturating_add(4)))
            .saturating_sub(12);
        let max_characters = usize::try_from(available / 8)
            .unwrap_or(usize::MAX)
            .min(255);
        let text = title_text_bytes(
            self.titles.get(&id).map_or("", String::as_str),
            max_characters,
        );
        if !text.is_empty() {
            let background = if self.clients.focused() == Some(id) {
                self.decoration_pixels.active_titlebar
            } else {
                self.decoration_pixels.inactive_titlebar
            };
            self.connection
                .change_gc(self.title_gc, &ChangeGCAux::new().background(background))?;
            self.connection.image_text8(
                frame.window,
                self.title_gc,
                6,
                clamp_i16_u32(titlebar_height / 2 + 5),
                &text,
            )?;
        }
        Ok(())
    }

    fn create_frame_button(
        &mut self,
        id: ClientId,
        frame: Window,
        content_width: u32,
        titlebar_height: u32,
        kind: FrameButtonKind,
        slot: u32,
    ) -> Result<Window, X11Error> {
        let button = self.connection.generate_id()?;
        let size = titlebar_height.saturating_sub(8).max(1).min(content_width);
        let x = content_width.saturating_sub(
            size.saturating_add(4)
                .saturating_mul(slot.saturating_add(1)),
        );
        let pixel = match kind {
            FrameButtonKind::Minimize => self.decoration_pixels.minimize_button,
            FrameButtonKind::Maximize => self.decoration_pixels.maximize_button,
            FrameButtonKind::Close => self.decoration_pixels.close_button,
        };
        self.connection.create_window(
            COPY_DEPTH_FROM_PARENT,
            button,
            frame,
            clamp_i16(i32::try_from(x).unwrap_or(i32::MAX)),
            4,
            x_dimension(size),
            x_dimension(size),
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(pixel)
                .event_mask(EventMask::BUTTON_PRESS | EventMask::EXPOSURE),
        )?;
        let name = match kind {
            FrameButtonKind::Minimize => b"nobox:minimize".as_slice(),
            FrameButtonKind::Maximize => b"nobox:maximize".as_slice(),
            FrameButtonKind::Close => b"nobox:close".as_slice(),
        };
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            button,
            self.atoms._NET_WM_NAME,
            self.atoms.UTF8_STRING,
            name,
        )?;
        self.frame_parts.insert(button, FramePart::Button(id, kind));
        Ok(button)
    }

    fn create_frame(
        &mut self,
        client: Window,
        content: Geometry,
        policy: ClientPolicy,
        original_border_width: u16,
        was_mapped: bool,
    ) -> Result<Frame, X11Error> {
        let id = client_id(client);
        let extents = self.decoration_extents(policy);
        let outer = extents.outer_geometry(content);
        let frame = self.connection.generate_id()?;
        let border_width = if policy.decorations.border {
            self.config.theme.border_width
        } else {
            0
        };
        let titlebar_height = if policy.decorations.titlebar {
            self.config.theme.titlebar_height
        } else {
            0
        };
        let frame_height = content.height.saturating_add(titlebar_height);
        self.connection.create_window(
            COPY_DEPTH_FROM_PARENT,
            frame,
            self.root,
            clamp_i16(outer.x),
            clamp_i16(outer.y),
            x_dimension(content.width),
            x_dimension(frame_height),
            x_u16(border_width),
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixel(self.decoration_pixels.inactive_titlebar)
                .border_pixel(self.decoration_pixels.inactive_border)
                .event_mask(
                    EventMask::SUBSTRUCTURE_REDIRECT
                        | EventMask::SUBSTRUCTURE_NOTIFY
                        | EventMask::BUTTON_PRESS
                        | EventMask::BUTTON_RELEASE
                        | EventMask::BUTTON_MOTION
                        | EventMask::EXPOSURE,
                ),
        )?;

        let close_button = if titlebar_height == 0 || !policy.decorations.close {
            None
        } else {
            Some(self.create_frame_button(
                id,
                frame,
                content.width,
                titlebar_height,
                FrameButtonKind::Close,
                0,
            )?)
        };
        let maximize_button = if titlebar_height == 0 || !policy.decorations.maximize {
            None
        } else {
            Some(self.create_frame_button(
                id,
                frame,
                content.width,
                titlebar_height,
                FrameButtonKind::Maximize,
                u32::from(close_button.is_some()),
            )?)
        };
        let minimize_button = if titlebar_height == 0 || !policy.decorations.minimize {
            None
        } else {
            Some(self.create_frame_button(
                id,
                frame,
                content.width,
                titlebar_height,
                FrameButtonKind::Minimize,
                u32::from(close_button.is_some()) + u32::from(maximize_button.is_some()),
            )?)
        };

        self.connection.change_window_attributes(
            client,
            &ChangeWindowAttributesAux::new().event_mask(client_events()),
        )?;
        self.connection.change_save_set(SetMode::INSERT, client)?;
        if was_mapped {
            self.expected_unmaps.insert(client, 2);
        }
        self.connection
            .reparent_window(client, frame, 0, clamp_i16_u32(titlebar_height))?;
        self.connection
            .configure_window(client, &ConfigureWindowAux::new().border_width(0))?;
        self.publish_frame_extents(client, extents)?;
        self.frame_parts.insert(frame, FramePart::Container(id));
        Ok(Frame {
            window: frame,
            minimize_button,
            maximize_button,
            close_button,
            extents,
            original_border_width,
        })
    }

    fn map_frame(&self, client: Window, frame: Frame) -> Result<(), X11Error> {
        if let Some(minimize_button) = frame.minimize_button {
            self.connection.map_window(minimize_button)?;
        }
        if let Some(maximize_button) = frame.maximize_button {
            self.connection.map_window(maximize_button)?;
        }
        if let Some(close_button) = frame.close_button {
            self.connection.map_window(close_button)?;
        }
        self.connection.map_window(client)?;
        self.connection.map_window(frame.window)?;
        Ok(())
    }

    fn frame_window(&self, id: ClientId) -> Window {
        self.frames
            .get(&id)
            .map_or_else(|| window_id(id), |frame| frame.window)
    }

    fn manage(&mut self, window: Window, map: bool) -> Result<(), X11Error> {
        let attributes = self.connection.get_window_attributes(window)?.reply()?;
        if attributes.override_redirect {
            if map {
                self.connection.map_window(window)?;
            }
            return Ok(());
        }
        if self.clients.contains(client_id(window)) {
            if map {
                self.restore(window)?;
                if self.config.focus.focus_new
                    && self
                        .clients
                        .get(client_id(window))
                        .is_some_and(|client| client.policy.capabilities.focusable)
                {
                    self.focus(window, self.last_timestamp)?;
                }
            }
            return Ok(());
        }

        let geometry = self.connection.get_geometry(window)?.reply()?;
        let initially_iconic = map
            && matches!(
                WmHints::get(&self.connection, window)?
                    .reply()?
                    .and_then(|hints| hints.initial_state),
                Some(WmHintsState::Iconic)
            );
        let normal_hints = self.read_normal_hints(window)?;
        let size_hints = normal_hints.size;
        let relationships = self.read_relationships(window)?;
        let initial_states = self.read_atom_list(window, self.atoms._NET_WM_STATE)?;
        let initially_maximized_horizontal =
            initial_states.contains(&self.atoms._NET_WM_STATE_MAXIMIZED_HORZ);
        let initially_maximized_vertical =
            initial_states.contains(&self.atoms._NET_WM_STATE_MAXIMIZED_VERT);
        let initially_fullscreen = initial_states.contains(&self.atoms._NET_WM_STATE_FULLSCREEN);
        let initial_layer = client_layer_from_states(
            &initial_states,
            self.atoms._NET_WM_STATE_ABOVE,
            self.atoms._NET_WM_STATE_BELOW,
        );
        let policy = self.read_client_policy(window, relationships.transient_for.is_some())?;
        let workspace =
            self.read_workspace_assignment(window, policy, relationships.transient_for)?;
        let titlebar_height = if policy.decorations.titlebar {
            self.config.theme.titlebar_height
        } else {
            0
        };
        let constrained = x_content_size(
            size_hints.constrain(Size::new(
                u32::from(geometry.width),
                u32::from(geometry.height),
            )),
            titlebar_height,
        );
        if constrained.width != u32::from(geometry.width)
            || constrained.height != u32::from(geometry.height)
        {
            self.connection.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .width(constrained.width)
                    .height(constrained.height),
            )?;
        }
        let id = client_id(window);
        let is_new = self.clients.manage(Client {
            id,
            geometry: Geometry::new(
                i32::from(geometry.x),
                i32::from(geometry.y),
                constrained.width,
                constrained.height,
            ),
            size_hints,
            gravity: normal_hints.gravity,
            policy,
            transient_for: relationships.transient_for,
            group: relationships.group,
            modal: relationships.modal,
            iconic: initially_iconic,
            workspace,
            layer: initial_layer,
            maximize: None,
            fullscreen: None,
        });

        let frame = self.create_frame(
            window,
            Geometry::new(
                i32::from(geometry.x),
                i32::from(geometry.y),
                constrained.width,
                constrained.height,
            ),
            policy,
            geometry.border_width,
            attributes.map_state != MapState::UNMAPPED,
        )?;
        self.frames.insert(id, frame);
        self.refresh_title(window)?;
        self.refresh_strut(window)?;
        self.publish_client_workspace(window, workspace)?;
        self.sync_layer_state(window, initial_layer)?;
        if initially_maximized_horizontal || initially_maximized_vertical {
            self.set_maximized(
                window,
                initially_maximized_horizontal,
                initially_maximized_vertical,
            )?;
        }
        if initially_fullscreen {
            self.set_fullscreen(window, true)?;
        }
        self.set_wm_state(
            window,
            if initially_iconic || !self.clients.is_visible(id) {
                WM_STATE_ICONIC
            } else {
                WM_STATE_NORMAL
            },
        )?;
        if !initially_iconic && self.clients.is_visible(id) {
            self.map_frame(window, frame)?;
            self.enforce_layers()?;
        }

        if is_new {
            info!(window = format_args!("{window:#x}"), "managing X11 client");
            self.update_client_lists()?;
        }
        if self.config.focus.focus_new
            && !initially_iconic
            && self.clients.is_visible(id)
            && policy.capabilities.focusable
        {
            self.focus(window, self.last_timestamp)?;
        }
        Ok(())
    }

    fn unmanage(&mut self, window: Window, withdrawn: bool) -> Result<(), X11Error> {
        let id = client_id(window);
        if self.drag.is_some_and(|drag| drag.window == window) {
            self.finish_drag(self.last_timestamp)?;
        }
        let was_focused = self.clients.focused() == Some(id);
        let geometry = self.clients.get(id).map(|client| client.geometry);
        if !self.clients.unmanage(id) {
            return Ok(());
        }
        self.titles.remove(&id);
        let removed_strut = self.struts.remove(&id).is_some();
        let mut client_exists = withdrawn;
        self.expected_unmaps.remove(&window);
        if let Some(frame) = self.frames.remove(&id) {
            self.frame_parts.remove(&frame.window);
            if let Some(minimize_button) = frame.minimize_button {
                self.frame_parts.remove(&minimize_button);
            }
            if let Some(maximize_button) = frame.maximize_button {
                self.frame_parts.remove(&maximize_button);
            }
            if let Some(close_button) = frame.close_button {
                self.frame_parts.remove(&close_button);
            }
            if withdrawn {
                client_exists = if let Some(geometry) = geometry {
                    window_request_succeeded(
                        self.connection
                            .reparent_window(
                                window,
                                self.root,
                                clamp_i16(geometry.x),
                                clamp_i16(geometry.y),
                            )?
                            .check(),
                    )?
                } else {
                    false
                };
                if client_exists {
                    client_exists = window_request_succeeded(
                        self.connection
                            .change_save_set(SetMode::DELETE, window)?
                            .check(),
                    )?;
                }
                if client_exists {
                    client_exists = window_request_succeeded(
                        self.connection
                            .configure_window(
                                window,
                                &ConfigureWindowAux::new()
                                    .border_width(u32::from(frame.original_border_width)),
                            )?
                            .check(),
                    )?;
                }
                if client_exists {
                    client_exists = window_request_succeeded(
                        self.connection
                            .delete_property(window, self.atoms._NET_FRAME_EXTENTS)?
                            .check(),
                    )?;
                }
            }
            self.connection.destroy_window(frame.window)?;
        }
        if withdrawn
            && client_exists
            && window_request_succeeded(
                self.connection
                    .delete_property(window, self.atoms.WM_STATE)?
                    .check(),
            )?
        {
            let _ = window_request_succeeded(
                self.connection
                    .delete_property(window, self.atoms._NET_WM_STATE)?
                    .check(),
            )?;
        }
        info!(
            window = format_args!("{window:#x}"),
            "unmanaging X11 client"
        );
        self.update_client_lists()?;
        if removed_strut {
            self.refresh_work_area()?;
        }
        if !was_focused {
            return Ok(());
        }
        if let Some(focused) = self.clients.focused() {
            if !self.focus(window_id(focused), self.last_timestamp)? {
                self.clear_x_focus(self.last_timestamp)?;
            }
        } else {
            self.clear_x_focus(self.last_timestamp)?;
        }
        Ok(())
    }

    fn unmap_notify(&mut self, event: &UnmapNotifyEvent) -> Result<(), X11Error> {
        if let Some(remaining) = self.expected_unmaps.get_mut(&event.window) {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                self.expected_unmaps.remove(&event.window);
            }
            return Ok(());
        }
        let attributes = match self.connection.get_window_attributes(event.window)?.reply() {
            Ok(attributes) => Some(attributes),
            Err(ReplyError::X11Error(_)) => None,
            Err(error) => return Err(error.into()),
        };
        if event.response_type & 0x80 != 0
            && attributes
                .as_ref()
                .is_some_and(|attributes| attributes.map_state != MapState::UNMAPPED)
        {
            debug!(
                window = format_args!("{:#x}", event.window),
                "ignoring synthetic unmap for a mapped client"
            );
            return Ok(());
        }
        self.unmanage(event.window, attributes.is_some())
    }

    fn focus(&mut self, window: Window, timestamp: u32) -> Result<bool, X11Error> {
        let requested = client_id(window);
        let Some(id) = self.clients.focus_target(requested) else {
            return Ok(false);
        };
        if self
            .clients
            .get(id)
            .is_none_or(|client| !client.policy.capabilities.focusable)
        {
            return Ok(false);
        }
        let window = window_id(id);

        let accepts_direct_focus = WmHints::get(&self.connection, window)?
            .reply()?
            .and_then(|hints| hints.input)
            .unwrap_or(true);
        let supports_take_focus = self.supports_protocol(window, self.atoms.WM_TAKE_FOCUS)?;
        let methods = focus_methods(accepts_direct_focus, supports_take_focus, timestamp);
        if !methods.direct && !methods.take_focus {
            debug!(
                window = format_args!("{window:#x}"),
                "client does not accept the available ICCCM focus methods"
            );
            return Ok(false);
        }
        self.clients.focus(id);

        if methods.direct {
            self.connection
                .set_input_focus(InputFocus::PARENT, window, timestamp)?;
        }
        if methods.take_focus {
            let message = ClientMessageEvent::new(
                32,
                window,
                self.atoms.WM_PROTOCOLS,
                [self.atoms.WM_TAKE_FOCUS, timestamp, 0, 0, 0],
            );
            self.connection
                .send_event(false, window, EventMask::NO_EVENT, message)?;
        }

        for client in self.clients.stacking() {
            let (border, titlebar) = if client == id {
                (
                    self.decoration_pixels.active_border,
                    self.decoration_pixels.active_titlebar,
                )
            } else {
                (
                    self.decoration_pixels.inactive_border,
                    self.decoration_pixels.inactive_titlebar,
                )
            };
            self.connection.change_window_attributes(
                self.frame_window(client),
                &ChangeWindowAttributesAux::new()
                    .border_pixel(border)
                    .background_pixel(titlebar),
            )?;
            self.draw_title(client)?;
        }
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_ACTIVE_WINDOW,
            AtomEnum::WINDOW,
            &[window],
        )?;
        if self.config.focus.raise_on_focus {
            self.raise_within_layer(id)?;
        }
        Ok(true)
    }

    fn clear_x_focus(&mut self, timestamp: u32) -> Result<(), X11Error> {
        self.clients.clear_focus();
        self.connection
            .set_input_focus(InputFocus::POINTER_ROOT, self.root, timestamp)?;
        self.connection
            .delete_property(self.root, self.atoms._NET_ACTIVE_WINDOW)?;
        Ok(())
    }

    fn set_wm_state(&self, window: Window, state: u32) -> Result<(), X11Error> {
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms.WM_STATE,
            self.atoms.WM_STATE,
            &[state, NONE],
        )?;
        Ok(())
    }

    fn iconify(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        if self.clients.get(id).is_none_or(|client| client.iconic) {
            return Ok(());
        }
        self.clients.set_iconic(id, true);
        self.connection.unmap_window(self.frame_window(id))?;
        self.set_wm_state(window, WM_STATE_ICONIC)?;
        if let Some(focused) = self.clients.focused() {
            if !self.focus(window_id(focused), self.last_timestamp)? {
                self.clear_x_focus(self.last_timestamp)?;
            }
        } else {
            self.clear_x_focus(self.last_timestamp)?;
        }
        Ok(())
    }

    fn restore(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        if self.clients.get(id).is_none_or(|client| !client.iconic) {
            return Ok(());
        }
        self.clients.set_iconic(id, false);
        if self.clients.is_visible(id) {
            if let Some(frame) = self.frames.get(&id).copied() {
                self.map_frame(window, frame)?;
            } else {
                self.connection.map_window(window)?;
            }
        }
        self.set_wm_state(window, WM_STATE_NORMAL)
    }

    fn switch_workspace(&mut self, workspace: WorkspaceId, timestamp: u32) -> Result<(), X11Error> {
        if workspace.index() >= self.clients.workspace_count()
            || workspace == self.clients.current_workspace()
        {
            return Ok(());
        }
        self.finish_drag(timestamp)?;
        self.clients.switch_workspace(workspace);
        self.reflow_maximized_clients()?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_CURRENT_DESKTOP,
            AtomEnum::CARDINAL,
            &[workspace.index()],
        )?;
        self.sync_workspace_visibility()?;
        self.restore_workspace_focus(timestamp)?;
        info!(workspace = workspace.index() + 1, "switched workspace");
        Ok(())
    }

    fn move_to_workspace(
        &mut self,
        id: ClientId,
        assignment: WorkspaceAssignment,
        timestamp: u32,
        follow: bool,
    ) -> Result<(), X11Error> {
        if self.drag.is_some_and(|drag| client_id(drag.window) == id) {
            self.finish_drag(timestamp)?;
        }
        let changed = self.clients.assign_workspace_family(id, assignment);
        if changed.is_empty() {
            return Ok(());
        }
        for member in changed {
            self.publish_client_workspace(window_id(member), assignment)?;
        }
        if !self.refresh_work_area()? {
            self.reflow_maximized_clients()?;
        }
        if follow
            && let WorkspaceAssignment::Workspace(workspace) = assignment
            && workspace != self.clients.current_workspace()
        {
            return self.switch_workspace(workspace, timestamp);
        }
        self.sync_workspace_visibility()?;
        self.restore_workspace_focus(timestamp)?;
        Ok(())
    }

    fn sync_workspace_visibility(&mut self) -> Result<(), X11Error> {
        for id in self.clients.stacking() {
            let Some(client) = self.clients.get(id).copied() else {
                continue;
            };
            let frame = self.frame_window(id);
            if !client.iconic && self.clients.is_visible(id) {
                if let Some(frame) = self.frames.get(&id).copied() {
                    self.map_frame(window_id(id), frame)?;
                } else {
                    self.connection.map_window(frame)?;
                }
                self.set_wm_state(window_id(id), WM_STATE_NORMAL)?;
            } else {
                self.connection.unmap_window(frame)?;
                self.set_wm_state(window_id(id), WM_STATE_ICONIC)?;
            }
        }
        self.enforce_layers()
    }

    fn restore_workspace_focus(&mut self, timestamp: u32) -> Result<(), X11Error> {
        if let Some(focused) = self.clients.focused()
            && self.focus(window_id(focused), timestamp)?
        {
            return Ok(());
        }
        self.clear_x_focus(timestamp)
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

    fn sync_stacking_from_server(&mut self) -> Result<(), X11Error> {
        let tree = self.connection.query_tree(self.root)?.reply()?;
        let observed =
            tree.children
                .into_iter()
                .filter_map(|window| match self.frame_parts.get(&window) {
                    Some(FramePart::Container(id)) => Some(*id),
                    _ => None,
                });
        self.clients.sync_stacking(observed);
        self.update_client_lists()
    }

    fn enforce_layers(&mut self) -> Result<(), X11Error> {
        for id in self.clients.policy_stacking() {
            self.connection.configure_window(
                self.frame_window(id),
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )?;
        }
        self.sync_stacking_from_server()
    }

    fn raise_within_layer(&mut self, id: ClientId) -> Result<(), X11Error> {
        if !self.clients.raise(id) {
            return Ok(());
        }
        self.enforce_layers()
    }

    fn net_restack_window(&mut self, event: &ClientMessageEvent) -> Result<(), X11Error> {
        if event.format != 32 {
            return Ok(());
        }
        let data = event.data.as_data32();
        let Some(stack_mode) = stack_mode(data[2]) else {
            return Ok(());
        };
        let id = client_id(event.window);
        let mut values = ConfigureWindowAux::new().stack_mode(stack_mode);
        if data[1] != NONE && data[1] != event.window {
            values = values.sibling(self.frame_window(client_id(data[1])));
        }
        self.connection
            .configure_window(self.frame_window(id), &values)?;
        self.sync_stacking_from_server()?;
        self.enforce_layers()
    }

    fn handle_event(&mut self, event: Event) -> Result<(), X11Error> {
        match event {
            Event::MapRequest(event) => self.manage(event.window, true)?,
            Event::ConfigureRequest(event) => self.configure_request(&event)?,
            Event::DestroyNotify(event) => self.unmanage(event.window, false)?,
            Event::UnmapNotify(event) => self.unmap_notify(&event)?,
            Event::ButtonPress(event) => self.button_press(&event)?,
            Event::KeyPress(event) => self.key_press(&event)?,
            Event::MotionNotify(event) => self.pointer_motion(event.root_x, event.root_y)?,
            Event::ButtonRelease(event)
                if self.drag.is_some_and(|drag| drag.button == event.detail) =>
            {
                self.finish_drag(event.time)?;
            }
            Event::Expose(event) => {
                if let Some(FramePart::Container(id)) = self.frame_parts.get(&event.window).copied()
                {
                    self.draw_title(id)?;
                }
            }
            Event::SelectionClear(event) if event.selection == self.wm_selection => {
                warn!("lost the ICCCM window-manager selection");
                self.running = false;
            }
            Event::PropertyNotify(event)
                if event.window == self.root && event.atom == self.atoms._NET_DESKTOP_LAYOUT =>
            {
                self.refresh_workspace_layout()?;
            }
            Event::PropertyNotify(event)
                if event.atom == u32::from(AtomEnum::WM_NORMAL_HINTS)
                    && self.clients.contains(client_id(event.window)) =>
            {
                let hints = self.read_normal_hints(event.window)?;
                self.clients
                    .set_size_hints(client_id(event.window), hints.size);
                self.clients
                    .set_gravity(client_id(event.window), hints.gravity);
            }
            Event::PropertyNotify(event)
                if (event.atom == self.atoms._NET_WM_NAME
                    || event.atom == u32::from(AtomEnum::WM_NAME))
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.refresh_title(event.window)?;
            }
            Event::PropertyNotify(event)
                if (event.atom == self.atoms._NET_WM_STRUT
                    || event.atom == self.atoms._NET_WM_STRUT_PARTIAL)
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.refresh_strut(event.window)?;
            }
            Event::PropertyNotify(event)
                if event.atom == self.atoms._NET_WM_DESKTOP
                    && self.clients.contains(client_id(event.window)) =>
            {
                let id = client_id(event.window);
                let Some(client) = self.clients.get(id).copied() else {
                    return Ok(());
                };
                let assignment = self.read_workspace_assignment(
                    event.window,
                    client.policy,
                    client.transient_for,
                )?;
                self.move_to_workspace(id, assignment, event.time, false)?;
            }
            Event::PropertyNotify(event)
                if (event.atom == self.atoms.WM_TRANSIENT_FOR
                    || event.atom == u32::from(AtomEnum::WM_HINTS)
                    || event.atom == self.atoms._NET_WM_STATE)
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.last_timestamp = event.time;
                self.refresh_relationships(event.window, event.time)?;
                if event.atom == self.atoms.WM_TRANSIENT_FOR {
                    self.refresh_client_policy(event.window)?;
                }
            }
            Event::PropertyNotify(event)
                if (event.atom == self.atoms._NET_WM_WINDOW_TYPE
                    || event.atom == self.atoms._MOTIF_WM_HINTS)
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.refresh_client_policy(event.window)?;
            }
            Event::MappingNotify(_) => {
                debug!("X11 input mapping changed; refreshing input grabs");
                self.reload_input_bindings()?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_ACTIVE_WINDOW
                    && self.clients.contains(client_id(event.window)) =>
            {
                if let Some(WorkspaceAssignment::Workspace(workspace)) = self
                    .clients
                    .get(client_id(event.window))
                    .map(|client| client.workspace)
                    && workspace != self.clients.current_workspace()
                {
                    self.switch_workspace(workspace, self.last_timestamp)?;
                }
                self.restore(event.window)?;
                let requested_timestamp = event.data.as_data32()[1];
                let timestamp = if requested_timestamp == CURRENT_TIME {
                    self.last_timestamp
                } else {
                    self.last_timestamp = requested_timestamp;
                    requested_timestamp
                };
                self.focus(event.window, timestamp)?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_CURRENT_DESKTOP && event.format == 32 =>
            {
                let data = event.data.as_data32();
                let timestamp = if data[1] == CURRENT_TIME {
                    self.last_timestamp
                } else {
                    self.last_timestamp = data[1];
                    data[1]
                };
                self.switch_workspace(WorkspaceId::new(data[0]), timestamp)?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_WM_DESKTOP
                    && event.format == 32
                    && self.clients.contains(client_id(event.window)) =>
            {
                if let Some(assignment) = workspace_assignment_from_ewmh(
                    event.data.as_data32()[0],
                    self.clients.workspace_count(),
                ) {
                    self.move_to_workspace(
                        client_id(event.window),
                        assignment,
                        self.last_timestamp,
                        false,
                    )?;
                }
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_WM_STATE
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.update_net_wm_state(&event)?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms.WM_CHANGE_STATE
                    && event.format == 32
                    && event.data.as_data32()[0] == WM_STATE_ICONIC
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.iconify(event.window)?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_RESTACK_WINDOW
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.net_restack_window(&event)?;
            }
            Event::ClientMessage(event) if event.type_ == self.atoms._NET_REQUEST_FRAME_EXTENTS => {
                let is_transient = self
                    .connection
                    .get_property(
                        false,
                        event.window,
                        self.atoms.WM_TRANSIENT_FOR,
                        AtomEnum::WINDOW,
                        0,
                        1,
                    )?
                    .reply()?
                    .value_len
                    > 0;
                let policy = self.read_client_policy(event.window, is_transient)?;
                self.publish_frame_extents(event.window, self.decoration_extents(policy))?;
            }
            Event::Error(error) => warn!(?error, "non-fatal X11 protocol error"),
            _ => {}
        }
        Ok(())
    }

    fn key_press(&mut self, event: &KeyPressEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        if self.drag.is_some() && self.escape_keycodes.contains(&event.detail) {
            self.cancel_drag(event.time)?;
            return Ok(());
        }
        let modifiers = u16::from(event.state) & 0xff & !self.ignored_modifiers;
        let Some(action) = self.key_bindings.get(&(event.detail, modifiers)).cloned() else {
            return Ok(());
        };
        match action {
            Action::Execute { command } => {
                match Command::new("/bin/sh")
                    .arg("-c")
                    .arg(&command)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    Ok(child) => info!(pid = child.id(), %command, "started key-binding command"),
                    Err(error) => warn!(%error, %command, "could not start key-binding command"),
                }
            }
            Action::Close => self.close_focused(event.time)?,
            Action::PreviousWorkspace => {
                let workspace = self
                    .clients
                    .workspace_in_direction(WorkspaceDirection::Previous);
                self.switch_workspace(workspace, event.time)?;
            }
            Action::NextWorkspace => {
                let workspace = self
                    .clients
                    .workspace_in_direction(WorkspaceDirection::Next);
                self.switch_workspace(workspace, event.time)?;
            }
            Action::WorkspaceLeft => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Left)?;
                self.switch_workspace(workspace, event.time)?;
            }
            Action::WorkspaceRight => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Right)?;
                self.switch_workspace(workspace, event.time)?;
            }
            Action::WorkspaceUp => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Up)?;
                self.switch_workspace(workspace, event.time)?;
            }
            Action::WorkspaceDown => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Down)?;
                self.switch_workspace(workspace, event.time)?;
            }
            Action::SwitchWorkspace { workspace } => {
                self.switch_workspace(WorkspaceId::new(workspace - 1), event.time)?;
            }
            Action::MoveToWorkspace { workspace, follow } => {
                if let Some(focused) = self.clients.focused() {
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(WorkspaceId::new(workspace - 1)),
                        event.time,
                        follow,
                    )?;
                }
            }
            Action::MoveToPreviousWorkspace { follow } => {
                if let Some(focused) = self.clients.focused() {
                    let workspace = self
                        .clients
                        .workspace_in_direction(WorkspaceDirection::Previous);
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        event.time,
                        follow,
                    )?;
                }
            }
            Action::MoveToNextWorkspace { follow } => {
                if let Some(focused) = self.clients.focused() {
                    let workspace = self
                        .clients
                        .workspace_in_direction(WorkspaceDirection::Next);
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        event.time,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceLeft { follow } => {
                if let Some(focused) = self.clients.focused() {
                    let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Left)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        event.time,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceRight { follow } => {
                if let Some(focused) = self.clients.focused() {
                    let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Right)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        event.time,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceUp { follow } => {
                if let Some(focused) = self.clients.focused() {
                    let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Up)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        event.time,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceDown { follow } => {
                if let Some(focused) = self.clients.focused() {
                    let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Down)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        event.time,
                        follow,
                    )?;
                }
            }
            Action::Exit => {
                self.finish_drag(event.time)?;
                self.running = false;
            }
        }
        Ok(())
    }

    fn close_focused(&self, timestamp: u32) -> Result<(), X11Error> {
        let Some(client) = self.clients.focused() else {
            return Ok(());
        };
        self.close_client(client, timestamp)
    }

    fn close_client(&self, client: ClientId, timestamp: u32) -> Result<(), X11Error> {
        if self
            .clients
            .get(client)
            .is_some_and(|client| !client.policy.capabilities.closable)
        {
            return Ok(());
        }
        let window = window_id(client);
        if self.supports_protocol(window, self.atoms.WM_DELETE_WINDOW)? {
            let message = ClientMessageEvent::new(
                32,
                window,
                self.atoms.WM_PROTOCOLS,
                [self.atoms.WM_DELETE_WINDOW, timestamp, 0, 0, 0],
            );
            self.connection
                .send_event(false, window, EventMask::NO_EVENT, message)?;
        } else {
            self.connection.kill_client(window)?;
        }
        Ok(())
    }

    fn supports_protocol(&self, window: Window, protocol: u32) -> Result<bool, X11Error> {
        let protocols = self
            .connection
            .get_property(
                false,
                window,
                self.atoms.WM_PROTOCOLS,
                AtomEnum::ATOM,
                0,
                u32::MAX,
            )?
            .reply()?;
        Ok(protocols
            .value32()
            .is_some_and(|mut atoms| atoms.any(|atom| atom == protocol)))
    }

    fn read_normal_hints(&self, window: Window) -> Result<NormalHints, X11Error> {
        let hints = WmSizeHints::get_normal_hints(&self.connection, window)?
            .reply()?
            .unwrap_or_default();
        Ok(NormalHints {
            size: SizeHints {
                minimum: positive_size(hints.min_size),
                maximum: positive_size(hints.max_size),
                base: nonnegative_size(hints.base_size),
                increment: positive_size(hints.size_increment),
                aspect: aspect_range(hints.aspect),
            },
            gravity: hints.win_gravity.map_or(Gravity::NorthWest, gravity),
        })
    }

    fn read_relationships(&self, window: Window) -> Result<Relationships, X11Error> {
        let transient = self
            .connection
            .get_property(
                false,
                window,
                self.atoms.WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?
            .value32()
            .and_then(|mut windows| windows.next());
        let transient_for = match transient {
            Some(parent) if parent == self.root => Some(TransientTarget::Group),
            Some(parent) if parent != window => Some(TransientTarget::Client(client_id(parent))),
            _ => None,
        };
        let group = WmHints::get(&self.connection, window)?
            .reply()?
            .and_then(|hints| hints.window_group)
            .map(client_id);
        let modal = self
            .read_atom_list(window, self.atoms._NET_WM_STATE)?
            .contains(&self.atoms._NET_WM_STATE_MODAL);
        Ok(Relationships {
            transient_for,
            group,
            modal,
        })
    }

    fn read_client_policy(
        &self,
        window: Window,
        is_transient: bool,
    ) -> Result<ClientPolicy, X11Error> {
        let role = self
            .read_atom_list(window, self.atoms._NET_WM_WINDOW_TYPE)?
            .into_iter()
            .find_map(|atom| self.client_role(atom))
            .unwrap_or(if is_transient {
                ClientRole::Dialog
            } else {
                ClientRole::Normal
            });
        let motif = self.read_motif_hints(window)?;
        let policy = apply_motif_hints(ClientPolicy::for_role(role), motif);
        debug!(
            window = format_args!("{window:#x}"),
            ?role,
            ?motif,
            "resolved X11 client policy"
        );
        Ok(policy)
    }

    fn client_role(&self, atom: u32) -> Option<ClientRole> {
        if atom == self.atoms._NET_WM_WINDOW_TYPE_NORMAL {
            Some(ClientRole::Normal)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_DIALOG {
            Some(ClientRole::Dialog)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_UTILITY {
            Some(ClientRole::Utility)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_TOOLBAR {
            Some(ClientRole::Toolbar)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_MENU {
            Some(ClientRole::Menu)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_SPLASH {
            Some(ClientRole::Splash)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_DESKTOP {
            Some(ClientRole::Desktop)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_DOCK {
            Some(ClientRole::Dock)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_DROPDOWN_MENU {
            Some(ClientRole::DropdownMenu)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_POPUP_MENU {
            Some(ClientRole::PopupMenu)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_TOOLTIP {
            Some(ClientRole::Tooltip)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_NOTIFICATION {
            Some(ClientRole::Notification)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_COMBO {
            Some(ClientRole::Combo)
        } else if atom == self.atoms._NET_WM_WINDOW_TYPE_DND {
            Some(ClientRole::DragAndDrop)
        } else {
            None
        }
    }

    fn read_motif_hints(&self, window: Window) -> Result<Option<MotifHints>, X11Error> {
        let reply = self
            .connection
            .get_property(
                false,
                window,
                self.atoms._MOTIF_WM_HINTS,
                self.atoms._MOTIF_WM_HINTS,
                0,
                5,
            )?
            .reply()?;
        let Some(mut values) = reply.value32() else {
            return Ok(None);
        };
        let Some(flags) = values.next() else {
            return Ok(None);
        };
        let Some(functions) = values.next() else {
            return Ok(None);
        };
        let Some(decorations) = values.next() else {
            return Ok(None);
        };
        Ok(Some(MotifHints {
            flags,
            functions,
            decorations,
        }))
    }

    fn read_atom_list(&self, window: Window, property: u32) -> Result<Vec<u32>, X11Error> {
        let reply = self
            .connection
            .get_property(false, window, property, AtomEnum::ATOM, 0, u32::MAX)?
            .reply()?;
        Ok(reply
            .value32()
            .map_or_else(Vec::new, |atoms| atoms.collect()))
    }

    fn refresh_relationships(&mut self, window: Window, timestamp: u32) -> Result<(), X11Error> {
        let relationships = self.read_relationships(window)?;
        let inherited_workspace = match relationships.transient_for {
            Some(TransientTarget::Client(parent)) => {
                self.clients.get(parent).map(|client| client.workspace)
            }
            Some(TransientTarget::Group) | None => None,
        };
        let changed = self.clients.set_relationships(
            client_id(window),
            relationships.transient_for,
            relationships.group,
            relationships.modal,
        );
        if changed {
            if let Some(workspace) = inherited_workspace {
                self.move_to_workspace(client_id(window), workspace, timestamp, false)?;
            }
            self.enforce_layers()?;
        }
        self.redirect_modal_focus(timestamp)
    }

    fn refresh_client_policy(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        let Some(current) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let policy = self.read_client_policy(window, current.transient_for.is_some())?;
        if current.policy == policy {
            return Ok(());
        }
        if current.maximize.is_some() && !policy.capabilities.maximizable {
            self.set_maximized(window, false, false)?;
        }
        if current.fullscreen.is_some() && !policy.capabilities.fullscreenable {
            self.set_fullscreen(window, false)?;
        }
        self.clients.set_policy(id, policy);
        self.apply_frame_policy(id, policy)?;
        if self.clients.focused() == Some(id) && !policy.capabilities.focusable {
            self.clear_x_focus(self.last_timestamp)?;
        }
        self.enforce_layers()
    }

    fn redirect_modal_focus(&mut self, timestamp: u32) -> Result<(), X11Error> {
        let Some(focused) = self.clients.focused() else {
            return Ok(());
        };
        if self.clients.focus_target(focused) != Some(focused) {
            self.focus(window_id(focused), timestamp)?;
        }
        Ok(())
    }

    fn screen_geometry(&self) -> Geometry {
        let screen = &self.connection.setup().roots[self.screen_index];
        Geometry::new(
            0,
            0,
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        )
    }

    fn publish_workspaces(&self) -> Result<(), X11Error> {
        let count = self.clients.workspace_count();
        let screen = self.screen_geometry();
        let viewport = (0..count).flat_map(|_| [0, 0]).collect::<Vec<_>>();
        let names = self
            .config
            .workspaces
            .names
            .iter()
            .flat_map(|name| name.as_bytes().iter().copied().chain(std::iter::once(0)))
            .collect::<Vec<_>>();
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_NUMBER_OF_DESKTOPS,
            AtomEnum::CARDINAL,
            &[count],
        )?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_CURRENT_DESKTOP,
            AtomEnum::CARDINAL,
            &[self.clients.current_workspace().index()],
        )?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_DESKTOP_GEOMETRY,
            AtomEnum::CARDINAL,
            &[screen.width, screen.height],
        )?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_DESKTOP_VIEWPORT,
            AtomEnum::CARDINAL,
            &viewport,
        )?;
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_DESKTOP_NAMES,
            self.atoms.UTF8_STRING,
            &names,
        )?;
        self.publish_work_area()
    }

    fn refresh_workspace_layout(&mut self) -> Result<(), X11Error> {
        let owner = self
            .connection
            .get_selection_owner(self.desktop_layout_selection)?
            .reply()?
            .owner;
        let pager_layout = if owner == NONE {
            None
        } else {
            let values = self.read_cardinals(self.root, self.atoms._NET_DESKTOP_LAYOUT)?;
            workspace_layout_from_ewmh(&values, self.clients.workspace_count())
        };
        let source = if pager_layout.is_some() {
            "pager"
        } else {
            "configuration"
        };
        let layout = pager_layout.unwrap_or_else(|| configured_workspace_layout(&self.config));
        if self.clients.set_workspace_layout(layout) {
            info!(
                source,
                columns = layout.columns(),
                rows = layout.rows(),
                "updated workspace layout"
            );
        }
        Ok(())
    }

    fn workspace_in_grid_direction(
        &mut self,
        direction: WorkspaceDirection,
    ) -> Result<WorkspaceId, X11Error> {
        self.refresh_workspace_layout()?;
        Ok(self
            .clients
            .workspace_in_grid_direction(direction, self.config.workspaces.wrap))
    }

    fn publish_work_area(&self) -> Result<(), X11Error> {
        let work_areas = self
            .work_areas
            .iter()
            .flat_map(|work_area| {
                [
                    u32::try_from(work_area.x).unwrap_or(0),
                    u32::try_from(work_area.y).unwrap_or(0),
                    work_area.width,
                    work_area.height,
                ]
            })
            .collect::<Vec<_>>();
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_WORKAREA,
            AtomEnum::CARDINAL,
            &work_areas,
        )?;
        Ok(())
    }

    fn read_cardinals(&self, window: Window, property: u32) -> Result<Vec<u32>, X11Error> {
        let reply = self
            .connection
            .get_property(false, window, property, AtomEnum::CARDINAL, 0, 12)?
            .reply()?;
        Ok(reply
            .value32()
            .map_or_else(Vec::new, |values| values.collect()))
    }

    fn read_workspace_assignment(
        &self,
        window: Window,
        policy: ClientPolicy,
        transient_for: Option<TransientTarget>,
    ) -> Result<WorkspaceAssignment, X11Error> {
        if let Some(TransientTarget::Client(parent)) = transient_for
            && let Some(parent) = self.clients.get(parent)
        {
            return Ok(parent.workspace);
        }
        if let Some(workspace) = self
            .read_cardinals(window, self.atoms._NET_WM_DESKTOP)?
            .first()
            .copied()
            && let Some(assignment) =
                workspace_assignment_from_ewmh(workspace, self.clients.workspace_count())
        {
            return Ok(assignment);
        }
        if matches!(policy.role, ClientRole::Desktop | ClientRole::Dock) {
            return Ok(WorkspaceAssignment::All);
        }
        Ok(WorkspaceAssignment::Workspace(
            self.clients.current_workspace(),
        ))
    }

    fn publish_client_workspace(
        &self,
        window: Window,
        assignment: WorkspaceAssignment,
    ) -> Result<(), X11Error> {
        let workspace = match assignment {
            WorkspaceAssignment::Workspace(workspace) => workspace.index(),
            WorkspaceAssignment::All => u32::MAX,
        };
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms._NET_WM_DESKTOP,
            AtomEnum::CARDINAL,
            &[workspace],
        )?;
        Ok(())
    }

    fn read_strut(&self, window: Window) -> Result<Option<EdgeReservations>, X11Error> {
        let partial = self.read_cardinals(window, self.atoms._NET_WM_STRUT_PARTIAL)?;
        if let [
            left,
            right,
            top,
            bottom,
            left_start,
            left_end,
            right_start,
            right_end,
            top_start,
            top_end,
            bottom_start,
            bottom_end,
        ] = partial.as_slice()
        {
            return Ok(Some(edge_reservations(
                [*left, *right, *top, *bottom],
                [
                    (*left_start, *left_end),
                    (*right_start, *right_end),
                    (*top_start, *top_end),
                    (*bottom_start, *bottom_end),
                ],
            )));
        }
        let legacy = self.read_cardinals(window, self.atoms._NET_WM_STRUT)?;
        let [left, right, top, bottom] = legacy.as_slice() else {
            return Ok(None);
        };
        let screen = self.screen_geometry();
        let horizontal_end = screen.width.saturating_sub(1);
        let vertical_end = screen.height.saturating_sub(1);
        Ok(Some(edge_reservations(
            [*left, *right, *top, *bottom],
            [
                (0, vertical_end),
                (0, vertical_end),
                (0, horizontal_end),
                (0, horizontal_end),
            ],
        )))
    }

    fn refresh_strut(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        let previous = self.struts.get(&id).copied();
        let current = self
            .read_strut(window)?
            .filter(|strut| edge_reservations_are_nonempty(*strut));
        if previous == current {
            return Ok(());
        }
        if let Some(current) = current {
            self.struts.insert(id, current);
        } else {
            self.struts.remove(&id);
        }
        self.refresh_work_area().map(|_| ())
    }

    fn refresh_work_area(&mut self) -> Result<bool, X11Error> {
        let screen = self.screen_geometry();
        let work_areas = (0..self.clients.workspace_count())
            .map(|index| {
                let workspace = WorkspaceId::new(index);
                screen.work_area(self.struts.iter().filter_map(|(id, reservation)| {
                    self.clients
                        .get(*id)
                        .filter(|client| client.workspace.is_visible_on(workspace))
                        .map(|_| *reservation)
                }))
            })
            .collect::<Vec<_>>();
        if work_areas == self.work_areas {
            return Ok(false);
        }
        self.work_areas = work_areas;
        self.publish_work_area()?;
        self.reflow_maximized_clients()?;
        info!(
            workspaces = self.work_areas.len(),
            reservations = self.struts.len(),
            "updated X11 work areas"
        );
        Ok(true)
    }

    fn reflow_maximized_clients(&mut self) -> Result<(), X11Error> {
        let maximized = self
            .clients
            .stacking()
            .filter_map(|id| {
                self.clients
                    .get(id)
                    .and_then(|client| client.maximize.map(|state| (id, state)))
            })
            .collect::<Vec<_>>();
        for (id, state) in maximized {
            self.set_maximized(window_id(id), state.horizontal, state.vertical)?;
        }
        Ok(())
    }

    fn available_geometry(&self, id: ClientId) -> Geometry {
        let workspace = self.clients.get(id).map_or_else(
            || self.clients.current_workspace(),
            |client| match client.workspace {
                WorkspaceAssignment::Workspace(workspace) => workspace,
                WorkspaceAssignment::All => self.clients.current_workspace(),
            },
        );
        let work_area = usize::try_from(workspace.index())
            .ok()
            .and_then(|index| self.work_areas.get(index))
            .copied()
            .unwrap_or_else(|| self.screen_geometry());
        let extents = self
            .frames
            .get(&id)
            .map_or_else(DecorationExtents::default, |frame| frame.extents);
        Geometry::new(
            add_root_offset(work_area.x, extents.left),
            add_root_offset(work_area.y, extents.top),
            work_area
                .width
                .saturating_sub(extents.left)
                .saturating_sub(extents.right),
            work_area
                .height
                .saturating_sub(extents.top)
                .saturating_sub(extents.bottom),
        )
    }

    fn set_maximized(
        &mut self,
        window: Window,
        horizontal: bool,
        vertical: bool,
    ) -> Result<(), X11Error> {
        let id = client_id(window);
        let available = self.available_geometry(id);
        let geometry = self
            .clients
            .set_maximized(id, horizontal, vertical, available);
        let actual = self.clients.get(id).and_then(|client| client.maximize);
        let actual_horizontal = actual.is_some_and(|state| state.horizontal);
        let actual_vertical = actual.is_some_and(|state| state.vertical);
        if let Some(geometry) = geometry {
            self.configure_decorated_client(id, geometry)?;
            self.draw_title(id)?;
        }
        self.sync_maximized_state(window, actual_horizontal, actual_vertical)
    }

    fn sync_maximized_state(
        &self,
        window: Window,
        horizontal: bool,
        vertical: bool,
    ) -> Result<(), X11Error> {
        let mut states = self.read_atom_list(window, self.atoms._NET_WM_STATE)?;
        states.retain(|state| {
            *state != self.atoms._NET_WM_STATE_MAXIMIZED_HORZ
                && *state != self.atoms._NET_WM_STATE_MAXIMIZED_VERT
        });
        if horizontal {
            states.push(self.atoms._NET_WM_STATE_MAXIMIZED_HORZ);
        }
        if vertical {
            states.push(self.atoms._NET_WM_STATE_MAXIMIZED_VERT);
        }
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms._NET_WM_STATE,
            AtomEnum::ATOM,
            &states,
        )?;
        Ok(())
    }

    fn toggle_full_maximize(&mut self, id: ClientId) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let is_full = client
            .maximize
            .is_some_and(|state| state.horizontal && state.vertical);
        self.set_maximized(window_id(id), !is_full, !is_full)
    }

    fn set_fullscreen(&mut self, window: Window, fullscreen: bool) -> Result<(), X11Error> {
        let id = client_id(window);
        let previous = self
            .clients
            .get(id)
            .is_some_and(|client| client.fullscreen.is_some());
        let geometry = self
            .clients
            .set_fullscreen(id, fullscreen, self.screen_geometry());
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let actual = client.fullscreen.is_some();
        if previous != actual {
            self.apply_frame_policy(id, client.policy)?;
            self.enforce_layers()?;
        } else if let Some(geometry) = geometry {
            self.configure_decorated_client(id, geometry)?;
        }
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_FULLSCREEN, actual)
    }

    fn set_client_layer(&mut self, window: Window, layer: ClientLayer) -> Result<(), X11Error> {
        let id = client_id(window);
        if self.clients.set_layer(id, layer) {
            self.sync_layer_state(window, layer)?;
            self.enforce_layers()?;
        }
        Ok(())
    }

    fn sync_boolean_state(&self, window: Window, atom: u32, enabled: bool) -> Result<(), X11Error> {
        let mut states = self.read_atom_list(window, self.atoms._NET_WM_STATE)?;
        states.retain(|state| *state != atom);
        if enabled {
            states.push(atom);
        }
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms._NET_WM_STATE,
            AtomEnum::ATOM,
            &states,
        )?;
        Ok(())
    }

    fn sync_layer_state(&self, window: Window, layer: ClientLayer) -> Result<(), X11Error> {
        let mut states = self.read_atom_list(window, self.atoms._NET_WM_STATE)?;
        states.retain(|state| {
            *state != self.atoms._NET_WM_STATE_ABOVE && *state != self.atoms._NET_WM_STATE_BELOW
        });
        match layer {
            ClientLayer::Below => states.push(self.atoms._NET_WM_STATE_BELOW),
            ClientLayer::Normal => {}
            ClientLayer::Above => states.push(self.atoms._NET_WM_STATE_ABOVE),
        }
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms._NET_WM_STATE,
            AtomEnum::ATOM,
            &states,
        )?;
        Ok(())
    }

    fn update_net_wm_state(&mut self, event: &ClientMessageEvent) -> Result<(), X11Error> {
        if event.format != 32 {
            return Ok(());
        }
        let data = event.data.as_data32();
        let id = client_id(event.window);
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let requested = [data[1], data[2]];
        if requested.contains(&self.atoms._NET_WM_STATE_MODAL)
            && let Some(modal) = ewmh_state_action(client.modal, data[0])
        {
            let mut states = self.read_atom_list(event.window, self.atoms._NET_WM_STATE)?;
            states.retain(|state| *state != self.atoms._NET_WM_STATE_MODAL);
            if modal {
                states.push(self.atoms._NET_WM_STATE_MODAL);
            }
            self.connection.change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                event.window,
                self.atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                &states,
            )?;
            self.clients.set_modal(id, modal);
            if modal {
                self.redirect_modal_focus(self.last_timestamp)?;
            }
        }

        let mut layer = client.layer;
        for (index, state) in requested.into_iter().enumerate() {
            if index == 1 && state == requested[0] {
                continue;
            }
            if state == self.atoms._NET_WM_STATE_ABOVE {
                let current = layer == ClientLayer::Above;
                if let Some(enabled) = ewmh_state_action(current, data[0]) {
                    layer = if enabled {
                        ClientLayer::Above
                    } else if current {
                        ClientLayer::Normal
                    } else {
                        layer
                    };
                }
            } else if state == self.atoms._NET_WM_STATE_BELOW {
                let current = layer == ClientLayer::Below;
                if let Some(enabled) = ewmh_state_action(current, data[0]) {
                    layer = if enabled {
                        ClientLayer::Below
                    } else if current {
                        ClientLayer::Normal
                    } else {
                        layer
                    };
                }
            }
        }
        if layer != client.layer {
            self.set_client_layer(event.window, layer)?;
        }

        let current_fullscreen = client.fullscreen.is_some();
        if requested.contains(&self.atoms._NET_WM_STATE_FULLSCREEN)
            && let Some(fullscreen) = ewmh_state_action(current_fullscreen, data[0])
            && fullscreen != current_fullscreen
        {
            self.set_fullscreen(event.window, fullscreen)?;
        }

        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let current_horizontal = client.maximize.is_some_and(|state| state.horizontal);
        let current_vertical = client.maximize.is_some_and(|state| state.vertical);
        let horizontal = if requested.contains(&self.atoms._NET_WM_STATE_MAXIMIZED_HORZ) {
            ewmh_state_action(current_horizontal, data[0]).unwrap_or(current_horizontal)
        } else {
            current_horizontal
        };
        let vertical = if requested.contains(&self.atoms._NET_WM_STATE_MAXIMIZED_VERT) {
            ewmh_state_action(current_vertical, data[0]).unwrap_or(current_vertical)
        } else {
            current_vertical
        };
        if horizontal != current_horizontal || vertical != current_vertical {
            self.set_maximized(event.window, horizontal, vertical)?;
        }
        Ok(())
    }

    fn apply_frame_policy(&mut self, id: ClientId, policy: ClientPolicy) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let geometry = client.geometry;
        let Some(previous) = self.frames.get(&id).copied() else {
            return Ok(());
        };
        let extents = if client.fullscreen.is_some() {
            DecorationExtents::default()
        } else {
            self.decoration_extents(policy)
        };
        let titlebar_height = extents.top.saturating_sub(extents.left);
        let wants_close = titlebar_height > 0 && policy.decorations.close;
        let close_button = match (previous.close_button, wants_close) {
            (Some(button), false) => {
                self.frame_parts.remove(&button);
                self.connection.destroy_window(button)?;
                None
            }
            (None, true) => {
                let button = self.create_frame_button(
                    id,
                    previous.window,
                    geometry.width,
                    titlebar_height,
                    FrameButtonKind::Close,
                    0,
                )?;
                self.connection.map_window(button)?;
                Some(button)
            }
            (button, _) => button,
        };
        let wants_maximize = titlebar_height > 0 && policy.decorations.maximize;
        let maximize_button = match (previous.maximize_button, wants_maximize) {
            (Some(button), false) => {
                self.frame_parts.remove(&button);
                self.connection.destroy_window(button)?;
                None
            }
            (None, true) => {
                let button = self.create_frame_button(
                    id,
                    previous.window,
                    geometry.width,
                    titlebar_height,
                    FrameButtonKind::Maximize,
                    u32::from(close_button.is_some()),
                )?;
                self.connection.map_window(button)?;
                Some(button)
            }
            (button, _) => button,
        };
        let wants_minimize = titlebar_height > 0 && policy.decorations.minimize;
        let minimize_button = match (previous.minimize_button, wants_minimize) {
            (Some(button), false) => {
                self.frame_parts.remove(&button);
                self.connection.destroy_window(button)?;
                None
            }
            (None, true) => {
                let button = self.create_frame_button(
                    id,
                    previous.window,
                    geometry.width,
                    titlebar_height,
                    FrameButtonKind::Minimize,
                    u32::from(close_button.is_some()) + u32::from(maximize_button.is_some()),
                )?;
                self.connection.map_window(button)?;
                Some(button)
            }
            (button, _) => button,
        };
        if let Some(frame) = self.frames.get_mut(&id) {
            frame.extents = extents;
            frame.minimize_button = minimize_button;
            frame.maximize_button = maximize_button;
            frame.close_button = close_button;
        }
        self.connection.configure_window(
            previous.window,
            &ConfigureWindowAux::new().border_width(extents.left),
        )?;
        if client.fullscreen.is_some() {
            self.configure_decorated_client(id, geometry)?;
        } else if let Some(maximize) = client.maximize {
            self.set_maximized(window_id(id), maximize.horizontal, maximize.vertical)?;
        } else {
            let constrained =
                x_content_size(Size::new(geometry.width, geometry.height), titlebar_height);
            let geometry = Geometry::new(
                geometry.x,
                geometry.y,
                constrained.width,
                constrained.height,
            );
            self.clients.set_geometry(id, geometry);
            self.configure_decorated_client(id, geometry)?;
            self.draw_title(id)?;
        }
        self.publish_frame_extents(window_id(id), extents)
    }

    fn configure_decorated_client(&self, id: ClientId, geometry: Geometry) -> Result<(), X11Error> {
        let client = window_id(id);
        let Some(frame) = self.frames.get(&id).copied() else {
            self.connection.configure_window(
                client,
                &ConfigureWindowAux::new()
                    .x(geometry.x)
                    .y(geometry.y)
                    .width(geometry.width)
                    .height(geometry.height),
            )?;
            return Ok(());
        };
        let outer = frame.extents.outer_geometry(geometry);
        let titlebar_height = frame.extents.top.saturating_sub(frame.extents.left);
        self.connection.configure_window(
            frame.window,
            &ConfigureWindowAux::new()
                .x(outer.x)
                .y(outer.y)
                .width(geometry.width)
                .height(geometry.height.saturating_add(titlebar_height)),
        )?;
        self.connection.configure_window(
            client,
            &ConfigureWindowAux::new()
                .x(0)
                .y(i32::try_from(titlebar_height).unwrap_or(i32::MAX))
                .width(geometry.width)
                .height(geometry.height)
                .border_width(0),
        )?;
        if let Some(close_button) = frame.close_button {
            let size = titlebar_height.saturating_sub(8).max(1).min(geometry.width);
            self.connection.configure_window(
                close_button,
                &ConfigureWindowAux::new()
                    .x(button_x(geometry.width, size, 0))
                    .width(size)
                    .height(size),
            )?;
        }
        if let Some(maximize_button) = frame.maximize_button {
            let size = titlebar_height.saturating_sub(8).max(1).min(geometry.width);
            self.connection.configure_window(
                maximize_button,
                &ConfigureWindowAux::new()
                    .x(button_x(
                        geometry.width,
                        size,
                        u32::from(frame.close_button.is_some()),
                    ))
                    .width(size)
                    .height(size),
            )?;
        }
        if let Some(minimize_button) = frame.minimize_button {
            let size = titlebar_height.saturating_sub(8).max(1).min(geometry.width);
            self.connection.configure_window(
                minimize_button,
                &ConfigureWindowAux::new()
                    .x(button_x(
                        geometry.width,
                        size,
                        u32::from(frame.close_button.is_some())
                            + u32::from(frame.maximize_button.is_some()),
                    ))
                    .width(size)
                    .height(size),
            )?;
        }
        let notify = ConfigureNotifyEvent {
            response_type: CONFIGURE_NOTIFY_EVENT,
            sequence: 0,
            event: client,
            window: client,
            above_sibling: NONE,
            x: clamp_i16(geometry.x),
            y: clamp_i16(geometry.y),
            width: x_dimension(geometry.width),
            height: x_dimension(geometry.height),
            border_width: 0,
            override_redirect: false,
        };
        self.connection
            .send_event(false, client, EventMask::STRUCTURE_NOTIFY, notify)?;
        Ok(())
    }

    fn configure_request(&mut self, event: &ConfigureRequestEvent) -> Result<(), X11Error> {
        let id = client_id(event.window);
        let managed = self.clients.get(id).copied();
        if let Some(client) = managed {
            let requested = Size::new(
                if event.value_mask.contains(ConfigWindow::WIDTH) {
                    u32::from(event.width)
                } else {
                    client.geometry.width
                },
                if event.value_mask.contains(ConfigWindow::HEIGHT) {
                    u32::from(event.height)
                } else {
                    client.geometry.height
                },
            );
            let constrained = x_content_size(
                client.size_hints.constrain(requested),
                self.frames.get(&id).map_or(0, |frame| {
                    frame.extents.top.saturating_sub(frame.extents.left)
                }),
            );
            let final_size = Size {
                width: if client.fullscreen.is_some()
                    || client.maximize.is_some_and(|state| state.horizontal)
                {
                    client.geometry.width
                } else if event.value_mask.contains(ConfigWindow::WIDTH) {
                    constrained.width
                } else {
                    client.geometry.width
                },
                height: if client.fullscreen.is_some()
                    || client.maximize.is_some_and(|state| state.vertical)
                {
                    client.geometry.height
                } else if event.value_mask.contains(ConfigWindow::HEIGHT) {
                    constrained.height
                } else {
                    client.geometry.height
                },
            };
            let x_was_requested = event.value_mask.contains(ConfigWindow::X);
            let y_was_requested = event.value_mask.contains(ConfigWindow::Y);
            let (gravity_x, gravity_y) = client.gravity.adjust_resize(
                client.geometry,
                final_size,
                x_was_requested,
                y_was_requested,
            );
            let final_x = if client.fullscreen.is_some()
                || client.maximize.is_some_and(|state| state.horizontal)
            {
                client.geometry.x
            } else if x_was_requested {
                i32::from(event.x)
            } else {
                gravity_x
            };
            let final_y = if client.fullscreen.is_some()
                || client.maximize.is_some_and(|state| state.vertical)
            {
                client.geometry.y
            } else if y_was_requested {
                i32::from(event.y)
            } else {
                gravity_y
            };
            let geometry = Geometry::new(final_x, final_y, final_size.width, final_size.height);
            self.configure_decorated_client(id, geometry)?;
            self.clients.set_geometry(id, geometry);

            if event.value_mask.contains(ConfigWindow::STACK_MODE) {
                let mut values = ConfigureWindowAux::new().stack_mode(event.stack_mode);
                if event.value_mask.contains(ConfigWindow::SIBLING) {
                    values = values.sibling(self.frame_window(client_id(event.sibling)));
                }
                self.connection
                    .configure_window(self.frame_window(id), &values)?;
                self.sync_stacking_from_server()?;
                self.enforce_layers()?;
            }
            return Ok(());
        }

        let mut values = ConfigureWindowAux::from_configure_request(event);
        if event.value_mask.contains(ConfigWindow::WIDTH) && event.width == 0 {
            values = values.width(1);
        }
        if event.value_mask.contains(ConfigWindow::HEIGHT) && event.height == 0 {
            values = values.height(1);
        }
        self.connection.configure_window(event.window, &values)?;
        Ok(())
    }

    fn button_press(&mut self, event: &ButtonPressEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        for candidate in [event.child, event.event] {
            if let Some(FramePart::Button(id, kind)) = self.frame_parts.get(&candidate).copied()
                && event.detail == u8::from(ButtonIndex::M1)
            {
                match kind {
                    FrameButtonKind::Minimize => self.iconify(window_id(id))?,
                    FrameButtonKind::Maximize => self.toggle_full_maximize(id)?,
                    FrameButtonKind::Close => self.close_client(id, event.time)?,
                }
                return Ok(());
            }
        }
        let id = [event.child, event.event]
            .into_iter()
            .find_map(|candidate| match self.frame_parts.get(&candidate) {
                Some(FramePart::Container(id)) => Some(*id),
                _ if self.clients.contains(client_id(candidate)) => Some(client_id(candidate)),
                _ => None,
            });
        let Some(id) = id else {
            return Ok(());
        };
        let window = window_id(id);
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        self.focus(window, event.time)?;

        let modifier = u16::from(self.modifier_mask());
        if u16::from(event.state) & modifier == 0 {
            return Ok(());
        }
        let kind = if event.detail == self.config.mouse.move_button {
            if !client.policy.capabilities.movable
                || client.maximize.is_some()
                || client.fullscreen.is_some()
            {
                return Ok(());
            }
            DragKind::Move
        } else if event.detail == self.config.mouse.resize_button {
            if !client.policy.capabilities.resizable
                || client.maximize.is_some()
                || client.fullscreen.is_some()
            {
                return Ok(());
            }
            DragKind::Resize
        } else {
            return Ok(());
        };
        let keyboard_status = self
            .connection
            .grab_keyboard(
                false,
                self.root,
                event.time,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )?
            .reply()?
            .status;
        if keyboard_status != GrabStatus::SUCCESS {
            warn!(
                status = u8::from(keyboard_status),
                "could not grab keyboard for cancellable pointer operation"
            );
            return Ok(());
        }
        self.drag = Some(Drag {
            window,
            kind,
            button: event.detail,
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
        let id = client_id(drag.window);
        let bounds = self.available_geometry(id);
        let resistance = self.config.mouse.edge_resistance;
        let geometry = match drag.kind {
            DragKind::Move => Geometry::new(
                drag.initial.x.saturating_add(dx),
                drag.initial.y.saturating_add(dy),
                drag.initial.width,
                drag.initial.height,
            )
            .snap_movement(bounds, resistance),
            DragKind::Resize => {
                let snapped = Geometry::new(
                    drag.initial.x,
                    drag.initial.y,
                    resize_dimension(drag.initial.width, dx),
                    resize_dimension(drag.initial.height, dy),
                )
                .snap_resize(bounds, resistance);
                let requested = Size::new(snapped.width, snapped.height);
                let constrained = self
                    .clients
                    .get(client_id(drag.window))
                    .map_or(requested, |client| client.size_hints.constrain(requested));
                let titlebar_height = self.frames.get(&client_id(drag.window)).map_or(0, |frame| {
                    frame.extents.top.saturating_sub(frame.extents.left)
                });
                let constrained = x_content_size(constrained, titlebar_height);
                Geometry::new(
                    drag.initial.x,
                    drag.initial.y,
                    constrained.width,
                    constrained.height,
                )
            }
        };
        self.configure_decorated_client(id, geometry)?;
        self.clients.set_geometry(id, geometry);
        Ok(())
    }

    fn finish_drag(&mut self, timestamp: u32) -> Result<(), X11Error> {
        if self.drag.take().is_some() {
            self.connection.ungrab_keyboard(timestamp)?;
        }
        Ok(())
    }

    fn cancel_drag(&mut self, timestamp: u32) -> Result<(), X11Error> {
        let Some(drag) = self.drag.take() else {
            return Ok(());
        };
        let id = client_id(drag.window);
        self.configure_decorated_client(id, drag.initial)?;
        self.clients.set_geometry(id, drag.initial);
        self.connection.ungrab_keyboard(timestamp)?;
        Ok(())
    }
}

impl Drop for WindowManager {
    fn drop(&mut self) {
        let _ = self
            .connection
            .set_selection_owner(NONE, self.wm_selection, self.last_timestamp);
        let _ = self
            .connection
            .ungrab_key(Grab::ANY, self.root, ModMask::ANY);
        let _ = self
            .connection
            .ungrab_button(ButtonIndex::ANY, self.root, ModMask::ANY);
        let _ = self.connection.ungrab_keyboard(CURRENT_TIME);
        let colormap = self.connection.setup().roots[self.screen_index].default_colormap;
        let _ = self
            .connection
            .free_colors(colormap, 0, &self.decoration_pixels.as_array());
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
        let _ = self
            .connection
            .delete_property(self.root, self.atoms._NET_WORKAREA);
        let _ = self.connection.free_gc(self.title_gc);
        let _ = self.connection.close_font(self.title_font);
        let _ = self.connection.destroy_window(self.support_window);
        let _ = self.connection.flush();
    }
}

#[derive(Clone, Copy)]
struct DecorationPixels {
    active_border: u32,
    inactive_border: u32,
    active_titlebar: u32,
    inactive_titlebar: u32,
    title_text: u32,
    minimize_button: u32,
    maximize_button: u32,
    close_button: u32,
}

impl DecorationPixels {
    fn allocate(
        connection: &RustConnection,
        colormap: u32,
        theme: &ThemeConfig,
    ) -> Result<Self, X11Error> {
        let colors = [
            theme.active_border,
            theme.inactive_border,
            theme.active_titlebar,
            theme.inactive_titlebar,
            theme.title_text,
            theme.minimize_button,
            theme.maximize_button,
            theme.close_button,
        ];
        let mut pixels = [0; 8];
        for (index, color) in colors.into_iter().enumerate() {
            match allocate_color(connection, colormap, color) {
                Ok(pixel) => pixels[index] = pixel,
                Err(error) => {
                    if index > 0 {
                        connection.free_colors(colormap, 0, &pixels[..index])?;
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self {
            active_border: pixels[0],
            inactive_border: pixels[1],
            active_titlebar: pixels[2],
            inactive_titlebar: pixels[3],
            title_text: pixels[4],
            minimize_button: pixels[5],
            maximize_button: pixels[6],
            close_button: pixels[7],
        })
    }

    const fn as_array(self) -> [u32; 8] {
        [
            self.active_border,
            self.inactive_border,
            self.active_titlebar,
            self.inactive_titlebar,
            self.title_text,
            self.minimize_button,
            self.maximize_button,
            self.close_button,
        ]
    }
}

#[derive(Clone, Copy)]
struct Frame {
    window: Window,
    minimize_button: Option<Window>,
    maximize_button: Option<Window>,
    close_button: Option<Window>,
    extents: DecorationExtents,
    original_border_width: u16,
}

#[derive(Clone, Copy)]
enum FramePart {
    Container(ClientId),
    Button(ClientId, FrameButtonKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrameButtonKind {
    Minimize,
    Maximize,
    Close,
}

#[derive(Clone, Copy, Debug)]
struct NormalHints {
    size: SizeHints,
    gravity: Gravity,
}

#[derive(Clone, Copy, Debug)]
struct Relationships {
    transient_for: Option<TransientTarget>,
    group: Option<ClientId>,
    modal: bool,
}

#[derive(Clone, Copy, Debug)]
struct MotifHints {
    flags: u32,
    functions: u32,
    decorations: u32,
}

#[derive(Clone, Copy)]
struct Drag {
    window: Window,
    kind: DragKind,
    button: u8,
    pointer_x: i16,
    pointer_y: i16,
    initial: Geometry,
}

#[derive(Clone, Copy)]
enum DragKind {
    Move,
    Resize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FocusMethods {
    direct: bool,
    take_focus: bool,
}

fn client_id(window: Window) -> ClientId {
    ClientId::new(u64::from(window))
}

fn window_id(client: ClientId) -> Window {
    u32::try_from(client.raw()).expect("X11 window identifiers are always 32-bit")
}

fn edge_reservations(depths: [u32; 4], spans: [(u32, u32); 4]) -> EdgeReservations {
    let reservation = |index: usize| EdgeReservation {
        depth: depths[index],
        start: i32::try_from(spans[index].0).unwrap_or(i32::MAX),
        end: i32::try_from(spans[index].1).unwrap_or(i32::MAX),
    };
    EdgeReservations {
        left: reservation(0),
        right: reservation(1),
        top: reservation(2),
        bottom: reservation(3),
    }
}

fn runtime_request_code(request: u32) -> Option<RuntimeRequest> {
    match request {
        CONTROL_RELOAD => Some(RuntimeRequest::Reload),
        CONTROL_SHUTDOWN => Some(RuntimeRequest::Shutdown),
        _ => None,
    }
}

fn workspace_assignment_from_ewmh(
    desktop: u32,
    workspace_count: u32,
) -> Option<WorkspaceAssignment> {
    if desktop == u32::MAX {
        Some(WorkspaceAssignment::All)
    } else if desktop < workspace_count {
        Some(WorkspaceAssignment::Workspace(WorkspaceId::new(desktop)))
    } else {
        None
    }
}

fn configured_workspace_layout(config: &Config) -> WorkspaceLayout {
    let count = u32::try_from(config.workspaces.names.len()).unwrap_or(1);
    let (columns, rows) = if config.workspaces.columns == 0 {
        (count, 1)
    } else {
        (config.workspaces.columns, 0)
    };
    WorkspaceLayout::new(
        count,
        columns,
        rows,
        WorkspaceOrientation::Horizontal,
        WorkspaceCorner::TopLeft,
    )
    .unwrap_or_else(|| WorkspaceLayout::one_row(count))
}

fn workspace_layout_from_ewmh(values: &[u32], count: u32) -> Option<WorkspaceLayout> {
    let [orientation, columns, rows, rest @ ..] = values else {
        return None;
    };
    let orientation = match *orientation {
        0 => WorkspaceOrientation::Horizontal,
        1 => WorkspaceOrientation::Vertical,
        _ => return None,
    };
    let corner = match rest.first().copied().unwrap_or(0) {
        0 => WorkspaceCorner::TopLeft,
        1 => WorkspaceCorner::TopRight,
        2 => WorkspaceCorner::BottomRight,
        3 => WorkspaceCorner::BottomLeft,
        _ => return None,
    };
    WorkspaceLayout::new(count, *columns, *rows, orientation, corner)
}

fn edge_reservations_are_nonempty(reservations: EdgeReservations) -> bool {
    reservations.left.depth > 0
        || reservations.right.depth > 0
        || reservations.top.depth > 0
        || reservations.bottom.depth > 0
}

fn add_root_offset(coordinate: i32, offset: u32) -> i32 {
    i32::try_from(i64::from(coordinate).saturating_add(i64::from(offset))).unwrap_or(i32::MAX)
}

fn apply_motif_hints(mut policy: ClientPolicy, hints: Option<MotifHints>) -> ClientPolicy {
    let Some(hints) = hints else {
        return policy;
    };
    if hints.flags & MOTIF_FLAG_DECORATIONS != 0
        && hints.decorations & MOTIF_DECORATION_ALL == 0
        && hints.decorations & (MOTIF_DECORATION_HANDLE | MOTIF_DECORATION_TITLE) == 0
    {
        policy.decorations = ClientDecorations {
            border: hints.decorations & MOTIF_DECORATION_BORDER != 0 && policy.decorations.border,
            titlebar: false,
            minimize: false,
            maximize: false,
            close: false,
        };
    }
    if hints.flags & MOTIF_FLAG_FUNCTIONS != 0 && hints.functions & MOTIF_FUNCTION_ALL == 0 {
        if hints.functions & MOTIF_FUNCTION_RESIZE == 0 {
            policy.capabilities.resizable = false;
        }
        if hints.functions & MOTIF_FUNCTION_MOVE == 0 {
            policy.capabilities.movable = false;
        }
    }
    if !policy.capabilities.resizable || !policy.capabilities.movable {
        policy.capabilities.maximizable = false;
        policy.decorations.maximize = false;
    }
    policy
}

fn ewmh_state_action(current: bool, action: u32) -> Option<bool> {
    match action {
        0 => Some(false),
        1 => Some(true),
        2 => Some(!current),
        _ => None,
    }
}

fn client_layer_from_states(states: &[u32], above: u32, below: u32) -> ClientLayer {
    if states.contains(&above) {
        ClientLayer::Above
    } else if states.contains(&below) {
        ClientLayer::Below
    } else {
        ClientLayer::Normal
    }
}

fn clamp_i16(value: i32) -> i16 {
    i16::try_from(value).unwrap_or(if value.is_negative() {
        i16::MIN
    } else {
        i16::MAX
    })
}

fn clamp_i16_u32(value: u32) -> i16 {
    i16::try_from(value).unwrap_or(i16::MAX)
}

fn x_dimension(value: u32) -> u16 {
    u16::try_from(value.max(1)).unwrap_or(u16::MAX)
}

fn x_u16(value: u32) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn button_x(content_width: u32, button_size: u32, slot: u32) -> i32 {
    i32::try_from(
        content_width.saturating_sub(
            button_size
                .saturating_add(4)
                .saturating_mul(slot.saturating_add(1)),
        ),
    )
    .unwrap_or(i32::MAX)
}

fn title_text_bytes(title: &str, limit: usize) -> Vec<u8> {
    title
        .chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .map(|character| u8::try_from(u32::from(character)).unwrap_or(b'?'))
        .collect()
}

fn x_content_size(size: Size, titlebar_height: u32) -> Size {
    let maximum_height = u32::from(u16::MAX)
        .saturating_sub(titlebar_height.min(u32::from(u16::MAX) - 1))
        .max(1);
    Size::new(
        size.width.min(u32::from(u16::MAX)),
        size.height.min(maximum_height),
    )
}

fn window_request_succeeded(result: Result<(), ReplyError>) -> Result<bool, X11Error> {
    match result {
        Ok(()) => Ok(true),
        Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Window => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn stack_mode(value: u32) -> Option<StackMode> {
    if value == u32::from(StackMode::ABOVE) {
        Some(StackMode::ABOVE)
    } else if value == u32::from(StackMode::BELOW) {
        Some(StackMode::BELOW)
    } else if value == u32::from(StackMode::TOP_IF) {
        Some(StackMode::TOP_IF)
    } else if value == u32::from(StackMode::BOTTOM_IF) {
        Some(StackMode::BOTTOM_IF)
    } else if value == u32::from(StackMode::OPPOSITE) {
        Some(StackMode::OPPOSITE)
    } else {
        None
    }
}

fn resize_dimension(initial: u32, delta: i32) -> u32 {
    let value = i64::from(initial).saturating_add(i64::from(delta));
    u32::try_from(value.max(1)).unwrap_or(u32::MAX)
}

fn positive_size(value: Option<(i32, i32)>) -> Option<Size> {
    let (width, height) = value?;
    let width = u32::try_from(width).ok().filter(|value| *value > 0)?;
    let height = u32::try_from(height).ok().filter(|value| *value > 0)?;
    Some(Size::new(width, height))
}

fn nonnegative_size(value: Option<(i32, i32)>) -> Option<Size> {
    let (width, height) = value?;
    Some(Size {
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
    })
}

fn gravity(value: x11rb::protocol::xproto::Gravity) -> Gravity {
    use x11rb::protocol::xproto::Gravity as XGravity;

    if value == XGravity::NORTH {
        Gravity::North
    } else if value == XGravity::NORTH_EAST {
        Gravity::NorthEast
    } else if value == XGravity::WEST {
        Gravity::West
    } else if value == XGravity::CENTER {
        Gravity::Center
    } else if value == XGravity::EAST {
        Gravity::East
    } else if value == XGravity::SOUTH_WEST {
        Gravity::SouthWest
    } else if value == XGravity::SOUTH {
        Gravity::South
    } else if value == XGravity::SOUTH_EAST {
        Gravity::SouthEast
    } else if value == XGravity::STATIC {
        Gravity::Static
    } else if value == XGravity::BIT_FORGET {
        Gravity::Forget
    } else {
        Gravity::NorthWest
    }
}

fn aspect_range(
    value: Option<(
        x11rb::properties::AspectRatio,
        x11rb::properties::AspectRatio,
    )>,
) -> Option<AspectRange> {
    let (minimum, maximum) = value?;
    let minimum = AspectRatio::new(
        u32::try_from(minimum.numerator).ok()?,
        u32::try_from(minimum.denominator).ok()?,
    )?;
    let maximum = AspectRatio::new(
        u32::try_from(maximum.numerator).ok()?,
        u32::try_from(maximum.denominator).ok()?,
    )?;
    AspectRange::new(minimum, maximum)
}

fn focus_methods(
    accepts_direct_focus: bool,
    supports_take_focus: bool,
    timestamp: u32,
) -> FocusMethods {
    FocusMethods {
        direct: accepts_direct_focus,
        take_focus: supports_take_focus && timestamp != CURRENT_TIME,
    }
}

fn keyboard_modifier_mask(modifiers: &[KeyboardModifier]) -> u16 {
    modifiers.iter().fold(0, |mask, modifier| {
        mask | u16::from(match modifier {
            KeyboardModifier::Control => ModMask::CONTROL,
            KeyboardModifier::Alt => ModMask::M1,
            KeyboardModifier::Shift => ModMask::SHIFT,
            KeyboardModifier::Super => ModMask::M4,
        })
    })
}

fn lock_combinations(ignored_modifiers: u16) -> Vec<u16> {
    let caps_lock = u16::from(ModMask::LOCK);
    let other_locks = ignored_modifiers & !caps_lock;
    let mut combinations = vec![0, caps_lock, other_locks, caps_lock | other_locks];
    combinations.sort_unstable();
    combinations.dedup();
    combinations
}

fn keycodes_for_named_symbol(
    minimum: u8,
    keysyms_per_keycode: u8,
    keysyms: &[u32],
    name: &str,
) -> Vec<u8> {
    keycodes_matching(minimum, keysyms_per_keycode, keysyms, |raw| {
        xkeysym::Keysym::new(raw).name().is_some_and(|candidate| {
            candidate == name
                || candidate.strip_prefix("XK_") == Some(name)
                || candidate.strip_prefix("XF86XK_") == Some(name)
        })
    })
}

fn keycodes_for_raw_symbol(
    minimum: u8,
    keysyms_per_keycode: u8,
    keysyms: &[u32],
    symbol: u32,
) -> Vec<u8> {
    keycodes_matching(minimum, keysyms_per_keycode, keysyms, |raw| raw == symbol)
}

fn keycodes_matching(
    minimum: u8,
    keysyms_per_keycode: u8,
    keysyms: &[u32],
    predicate: impl Fn(u32) -> bool,
) -> Vec<u8> {
    let width = usize::from(keysyms_per_keycode);
    if width == 0 {
        return Vec::new();
    }
    keysyms
        .chunks(width)
        .enumerate()
        .filter(|(_, symbols)| symbols.iter().copied().any(&predicate))
        .filter_map(|(offset, _)| {
            u8::try_from(offset)
                .ok()
                .and_then(|offset| minimum.checked_add(offset))
        })
        .collect()
}

fn server_timestamp(
    connection: &RustConnection,
    support_window: Window,
    timestamp_atom: u32,
) -> Result<u32, X11Error> {
    connection.change_property8(
        x11rb::protocol::xproto::PropMode::APPEND,
        support_window,
        timestamp_atom,
        AtomEnum::INTEGER,
        &[0],
    )?;
    connection.flush()?;
    loop {
        match connection.wait_for_event()? {
            Event::PropertyNotify(event)
                if event.window == support_window && event.atom == timestamp_atom =>
            {
                return Ok(event.time);
            }
            Event::Error(error) => warn!(?error, "X11 error while obtaining server timestamp"),
            _ => {}
        }
    }
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
    /// The X server advertised an impossible keycode interval.
    #[error("X11 server advertised invalid keycode range {minimum}..={maximum}")]
    InvalidKeyboardRange {
        /// Minimum keycode.
        minimum: u8,
        /// Maximum keycode.
        maximum: u8,
    },
    /// A configured X11 keysym name was absent from the active keyboard map.
    #[error("X11 keyboard map has no symbol named {0:?}")]
    UnknownKeySymbol(String),
    /// Two configured symbols resolved to the same physical grab.
    #[error("multiple bindings resolve to keycode {keycode} with modifiers {modifiers:#x}")]
    DuplicateKeyGrab {
        /// Conflicting X11 keycode.
        keycode: u8,
        /// Conflicting normalized modifier mask.
        modifiers: u16,
    },
    /// The ICCCM selection did not report nobox as its owner after acquisition.
    #[error("could not acquire ICCCM window-manager selection {0}")]
    SelectionClaim(String),
}

impl X11Error {
    fn is_vanished_window(&self) -> bool {
        match self {
            Self::Reply(ReplyError::X11Error(error))
            | Self::ReplyOrId(ReplyOrIdError::X11Error(error)) => {
                error.error_kind == ErrorKind::Window
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizing_clamps_at_one_pixel() {
        assert_eq!(resize_dimension(10, -50), 1);
        assert_eq!(resize_dimension(10, 5), 15);
    }

    #[test]
    fn lock_combinations_are_unique_without_num_lock() {
        assert_eq!(
            lock_combinations(u16::from(ModMask::LOCK)),
            [0, u16::from(ModMask::LOCK)]
        );
    }

    #[test]
    fn keycode_lookup_checks_every_keyboard_column() {
        let mapping = [xkeysym::key::a, xkeysym::key::A, 0, xkeysym::key::Return];
        assert_eq!(keycodes_for_named_symbol(8, 2, &mapping, "A"), [8]);
        assert_eq!(keycodes_for_named_symbol(8, 2, &mapping, "Return"), [9]);
    }

    #[test]
    fn icccm_focus_methods_respect_input_hint_and_timestamp() {
        assert_eq!(
            focus_methods(true, false, CURRENT_TIME),
            FocusMethods {
                direct: true,
                take_focus: false,
            }
        );
        assert_eq!(
            focus_methods(false, true, 42),
            FocusMethods {
                direct: false,
                take_focus: true,
            }
        );
        assert_eq!(
            focus_methods(false, true, CURRENT_TIME),
            FocusMethods {
                direct: false,
                take_focus: false,
            }
        );
    }

    #[test]
    fn stack_modes_reject_unknown_protocol_values() {
        assert_eq!(stack_mode(0), Some(StackMode::ABOVE));
        assert_eq!(stack_mode(4), Some(StackMode::OPPOSITE));
        assert_eq!(stack_mode(5), None);
        assert_eq!(stack_mode(u32::MAX), None);
    }

    #[test]
    fn framed_content_is_clamped_to_x11_dimensions() {
        assert_eq!(
            x_content_size(Size::new(u32::MAX, u32::MAX), 24),
            Size::new(u32::from(u16::MAX), u32::from(u16::MAX) - 24)
        );
    }

    #[test]
    fn motif_hints_remove_titlebar_but_can_retain_border() {
        let undecorated = apply_motif_hints(
            ClientPolicy::for_role(ClientRole::Normal),
            Some(MotifHints {
                flags: MOTIF_FLAG_DECORATIONS,
                functions: 0,
                decorations: 0,
            }),
        );
        assert_eq!(
            undecorated.decorations.extents(2, 24),
            DecorationExtents::default()
        );

        let border_only = apply_motif_hints(
            ClientPolicy::for_role(ClientRole::Normal),
            Some(MotifHints {
                flags: MOTIF_FLAG_DECORATIONS,
                functions: 0,
                decorations: MOTIF_DECORATION_BORDER,
            }),
        );
        assert_eq!(
            border_only.decorations.extents(2, 24),
            DecorationExtents::new(2, 2, 2, 2)
        );
    }

    #[test]
    fn motif_function_hints_limit_interactive_operations() {
        let policy = apply_motif_hints(
            ClientPolicy::for_role(ClientRole::Normal),
            Some(MotifHints {
                flags: MOTIF_FLAG_FUNCTIONS,
                functions: MOTIF_FUNCTION_MOVE,
                decorations: 0,
            }),
        );
        assert!(policy.capabilities.movable);
        assert!(!policy.capabilities.resizable);
        assert!(!policy.capabilities.maximizable);
        assert!(!policy.decorations.maximize);
    }

    #[test]
    fn title_text_is_bounded_and_safe_for_the_core_x11_font() {
        assert_eq!(title_text_bytes("nobox\nrocks", 8), b"noboxroc");
        assert_eq!(title_text_bytes("blåbær", usize::MAX), b"bl\xe5b\xe6r");
        assert_eq!(title_text_bytes("snowman ☃", usize::MAX), b"snowman ?");
    }

    #[test]
    fn frame_buttons_are_laid_out_from_the_right_edge() {
        assert_eq!(button_x(400, 16, 0), 380);
        assert_eq!(button_x(400, 16, 1), 360);
    }

    #[test]
    fn ewmh_state_actions_add_remove_and_toggle() {
        assert_eq!(ewmh_state_action(false, 0), Some(false));
        assert_eq!(ewmh_state_action(false, 1), Some(true));
        assert_eq!(ewmh_state_action(false, 2), Some(true));
        assert_eq!(ewmh_state_action(true, 2), Some(false));
        assert_eq!(ewmh_state_action(false, 3), None);
    }

    #[test]
    fn ewmh_layer_state_is_mutually_exclusive() {
        assert_eq!(client_layer_from_states(&[], 10, 20), ClientLayer::Normal);
        assert_eq!(client_layer_from_states(&[20], 10, 20), ClientLayer::Below);
        assert_eq!(client_layer_from_states(&[10], 10, 20), ClientLayer::Above);
        assert_eq!(
            client_layer_from_states(&[20, 10], 10, 20),
            ClientLayer::Above
        );
    }

    #[test]
    fn runtime_control_codes_are_typed_and_unknown_codes_are_ignored() {
        assert_eq!(
            runtime_request_code(CONTROL_RELOAD),
            Some(RuntimeRequest::Reload)
        );
        assert_eq!(
            runtime_request_code(CONTROL_SHUTDOWN),
            Some(RuntimeRequest::Shutdown)
        );
        assert_eq!(runtime_request_code(0), None);
        assert_eq!(runtime_request_code(u32::MAX), None);
    }

    #[test]
    fn ewmh_desktops_translate_to_core_workspace_assignments() {
        assert_eq!(
            workspace_assignment_from_ewmh(1, 4),
            Some(WorkspaceAssignment::Workspace(WorkspaceId::new(1)))
        );
        assert_eq!(
            workspace_assignment_from_ewmh(u32::MAX, 4),
            Some(WorkspaceAssignment::All)
        );
        assert_eq!(workspace_assignment_from_ewmh(4, 4), None);
    }

    #[test]
    fn ewmh_desktop_layout_accepts_legacy_and_current_forms() {
        let legacy = workspace_layout_from_ewmh(&[0, 2, 0], 4).unwrap();
        assert_eq!((legacy.columns(), legacy.rows()), (2, 2));
        assert_eq!(
            legacy.neighbor(WorkspaceId::new(0), WorkspaceDirection::Down, false),
            WorkspaceId::new(2)
        );

        let vertical_top_right = workspace_layout_from_ewmh(&[1, 2, 2, 1], 4).unwrap();
        assert_eq!(
            vertical_top_right.neighbor(WorkspaceId::new(0), WorkspaceDirection::Left, false),
            WorkspaceId::new(2)
        );
        assert!(workspace_layout_from_ewmh(&[2, 2, 2, 0], 4).is_none());
        assert!(workspace_layout_from_ewmh(&[0, 0, 0, 0], 4).is_none());
    }

    #[test]
    fn x11_strut_order_translates_to_protocol_neutral_edges() {
        let reservations = edge_reservations([10, 20, 30, 40], [(1, 2), (3, 4), (5, 6), (7, 8)]);
        assert_eq!(reservations.left.depth, 10);
        assert_eq!((reservations.right.start, reservations.right.end), (3, 4));
        assert_eq!(reservations.top.depth, 30);
        assert_eq!((reservations.bottom.start, reservations.bottom.end), (7, 8));
        assert!(edge_reservations_are_nonempty(reservations));
        assert!(!edge_reservations_are_nonempty(EdgeReservations::default()));
    }
}
