//! X11 window-manager backend.

mod session;

pub use session::{SessionError, SessionRestore, SessionSnapshot};

use std::{
    collections::{BTreeMap, BTreeSet},
    process::{Command, Stdio},
    sync::mpsc::{self, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use nobox_config::{
    Action, ApplicationIdentity, ApplicationKind, ApplicationLayer, ApplicationSettings, Config,
    KeyboardModifier, MenuDefinition, MenuEntry, MenuSource, MouseContext, MouseModifier,
    MouseTrigger, RgbColor, ThemeConfig,
};
use nobox_core::{
    AspectRange, AspectRatio, Client, ClientDecorations, ClientId, ClientLayer, ClientPolicy,
    ClientPresentation, ClientRole, ClientSet, DecorationExtents, DecorationOverride,
    EdgeReservation, EdgeReservations, Geometry, Gravity, Output, OutputCoverage, OutputId,
    OutputSet, Size, SizeHints, TransientTarget, WorkspaceAssignment, WorkspaceCorner,
    WorkspaceDirection, WorkspaceId, WorkspaceLayout, WorkspaceOrientation, centered_placement,
    smart_placement,
};
use thiserror::Error;
use tracing::{debug, info, warn};
use x11rb::{
    COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE,
    connection::{Connection, RequestConnection},
    errors::{ConnectError, ConnectionError, ReplyError, ReplyOrIdError},
    properties::{WmHints, WmHintsState, WmSizeHints},
    protocol::{
        ErrorKind, Event,
        randr::{ConnectionExt as _, NotifyMask},
        shape::{ConnectionExt as _, SK, SO},
        sync::{
            self, Alarm, ConnectionExt as _, Counter, CreateAlarmAux, Int64 as SyncInt64, TESTTYPE,
            VALUETYPE,
        },
        xproto::{
            AtomEnum, ButtonIndex, ButtonPressEvent, ButtonReleaseEvent, CONFIGURE_NOTIFY_EVENT,
            ChangeGCAux, ChangeWindowAttributesAux, ClientMessageEvent, ClipOrdering, Colormap,
            ColormapNotifyEvent, ConfigWindow, ConfigureNotifyEvent, ConfigureRequestEvent,
            ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, EnterNotifyEvent,
            EventMask, FocusInEvent, Font, Gcontext, Grab, GrabMode, GrabStatus, InputFocus,
            KeyPressEvent, KeyReleaseEvent, MapState, ModMask, MotionNotifyEvent, NotifyDetail,
            NotifyMode, Rectangle, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent,
            SelectionRequestEvent, SetMode, StackMode, UnmapNotifyEvent, Window, WindowClass,
        },
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        UTF8_STRING,
        ATOM_PAIR,
        MANAGER,
        MULTIPLE,
        TARGETS,
        TIMESTAMP,
        SM_CLIENT_ID,
        WM_DELETE_WINDOW,
        WM_CHANGE_STATE,
        WM_CLIENT_LEADER,
        WM_COLORMAP_WINDOWS,
        WM_PROTOCOLS,
        WM_STATE,
        WM_TAKE_FOCUS,
        WM_TRANSIENT_FOR,
        WM_WINDOW_ROLE,
        _MOTIF_WM_HINTS,
        _NOBOX_CONTROL,
        _NOBOX_FOCUS_SWITCHER,
        _NOBOX_MENU,
        _NOBOX_MENU_SELECTION,
        _NOBOX_TIMESTAMP,
        _NET_ACTIVE_WINDOW,
        _NET_CLOSE_WINDOW,
        _NET_CLIENT_LIST,
        _NET_CLIENT_LIST_STACKING,
        _NET_CURRENT_DESKTOP,
        _NET_DESKTOP_GEOMETRY,
        _NET_DESKTOP_LAYOUT,
        _NET_DESKTOP_NAMES,
        _NET_DESKTOP_VIEWPORT,
        _NET_FRAME_EXTENTS,
        _NET_MOVERESIZE_WINDOW,
        _NET_WM_FULLSCREEN_MONITORS,
        _NET_WM_MOVERESIZE,
        _NET_NUMBER_OF_DESKTOPS,
        _NET_REQUEST_FRAME_EXTENTS,
        _NET_RESTACK_WINDOW,
        _NET_SHOWING_DESKTOP,
        _NET_SUPPORTED,
        _NET_SUPPORTING_WM_CHECK,
        _NET_WORKAREA,
        _NET_WM_ACTION_ABOVE,
        _NET_WM_ACTION_BELOW,
        _NET_WM_ACTION_CHANGE_DESKTOP,
        _NET_WM_ACTION_CLOSE,
        _NET_WM_ACTION_FULLSCREEN,
        _NET_WM_ACTION_MAXIMIZE_HORZ,
        _NET_WM_ACTION_MAXIMIZE_VERT,
        _NET_WM_ACTION_MINIMIZE,
        _NET_WM_ACTION_MOVE,
        _NET_WM_ACTION_RESIZE,
        _NET_WM_ACTION_SHADE,
        _NET_WM_ALLOWED_ACTIONS,
        _NET_WM_ICON,
        _NET_WM_NAME,
        _NET_WM_PING,
        _NET_WM_SYNC_REQUEST,
        _NET_WM_SYNC_REQUEST_COUNTER,
        _NET_WM_DESKTOP,
        _NET_WM_STATE,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_BELOW,
        _NET_WM_STATE_DEMANDS_ATTENTION,
        _NET_WM_STATE_FOCUSED,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MODAL,
        _NET_WM_STATE_SHADED,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_USER_TIME,
        _NET_WM_USER_TIME_WINDOW,
        _NET_WM_VISIBLE_NAME,
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
        | EventMask::KEY_RELEASE
}

fn client_events() -> EventMask {
    EventMask::STRUCTURE_NOTIFY
        | EventMask::PROPERTY_CHANGE
        | EventMask::COLOR_MAP_CHANGE
        | EventMask::FOCUS_CHANGE
        | EventMask::BUTTON_PRESS
        | EventMask::BUTTON_RELEASE
        | EventMask::ENTER_WINDOW
        | EventMask::BUTTON_MOTION
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
const CONTROL_KEY_CHAIN_TIMEOUT: u32 = 3;
const CONTROL_PING_TIMEOUT: u32 = 4;
const CONTROL_SYNC_RESIZE_TIMEOUT: u32 = 5;
const CLIENT_PING_TIMEOUT: Duration = Duration::from_secs(3);
const SYNC_RESIZE_TIMEOUT: Duration = Duration::from_secs(1);
const PREFERRED_CLIENT_ICON_SIZE: u32 = 32;
const MAX_CLIENT_ICON_DIMENSION: u32 = 256;
const MAX_CLIENT_ICON_PROPERTY_VALUES: u32 = 256 * 256 + 2;
const MAX_SELECTION_MULTIPLE_PAIRS: u32 = 64;
const MAX_CLIENT_COLORMAP_WINDOWS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClientColormapWindow {
    window: Window,
    colormap: Colormap,
}

/// A separate X11 connection used to wake and control a running [`WindowManager`].
pub struct ControlSender {
    connection: RustConnection,
    window: Window,
    atom: u32,
}

impl ControlSender {
    fn connect(display: Option<&str>, window: Window, atom: u32) -> Result<Self, X11Error> {
        let (connection, _) = x11rb::connect(display)?;
        Ok(Self {
            connection,
            window,
            atom,
        })
    }

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
        self.send_data(request, 0)
    }

    fn send_data(&self, request: u32, value: u32) -> Result<(), X11Error> {
        self.send_payload(request, value, 0)
    }

    fn send_payload(&self, request: u32, value: u32, extra: u32) -> Result<(), X11Error> {
        let message =
            ClientMessageEvent::new(32, self.window, self.atom, [request, value, extra, 0, 0]);
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
    KeyChainTimeout(u32),
    PingTimeout { client: ClientId, generation: u32 },
    SyncResizeTimeout { client: ClientId, generation: u32 },
}

enum RuntimeTimerCommand {
    ArmKeyChain {
        generation: u32,
        timeout: Duration,
    },
    CancelKeyChain,
    ArmPing {
        client: ClientId,
        generation: u32,
        timeout: Duration,
    },
    CancelPing(ClientId),
    ArmSyncResize {
        client: ClientId,
        generation: u32,
        timeout: Duration,
    },
    CancelSyncResize,
    Stop,
}

struct RuntimeTimer {
    commands: Sender<RuntimeTimerCommand>,
    thread: Option<JoinHandle<()>>,
}

impl RuntimeTimer {
    fn spawn(control: ControlSender) -> Result<Self, X11Error> {
        let (commands, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("nobox-runtime-timer".to_owned())
            .stack_size(64 * 1024)
            .spawn(move || {
                let mut key_chain = None;
                let mut pings: BTreeMap<ClientId, (u32, Instant)> = BTreeMap::new();
                let mut sync_resize = None;
                loop {
                    let now = Instant::now();
                    if let Some((generation, deadline)) = key_chain
                        && deadline <= now
                    {
                        key_chain = None;
                        if let Err(error) = control.send_data(CONTROL_KEY_CHAIN_TIMEOUT, generation)
                        {
                            warn!(%error, "could not deliver keyboard-chain timeout");
                            break;
                        }
                    }
                    let mut delivery_failed = false;
                    while let Some((client, generation)) =
                        pings.iter().find_map(|(client, (generation, deadline))| {
                            (*deadline <= now).then_some((*client, *generation))
                        })
                    {
                        pings.remove(&client);
                        if let Err(error) = control.send_payload(
                            CONTROL_PING_TIMEOUT,
                            window_id(client),
                            generation,
                        ) {
                            warn!(%error, "could not deliver client-ping timeout");
                            delivery_failed = true;
                            break;
                        }
                    }
                    if delivery_failed {
                        break;
                    }
                    if let Some((client, generation, deadline)) = sync_resize
                        && deadline <= now
                    {
                        sync_resize = None;
                        if let Err(error) = control.send_payload(
                            CONTROL_SYNC_RESIZE_TIMEOUT,
                            window_id(client),
                            generation,
                        ) {
                            warn!(%error, "could not deliver synchronized-resize timeout");
                            break;
                        }
                    }

                    let deadline = key_chain
                        .map(|(_, deadline)| deadline)
                        .into_iter()
                        .chain(pings.values().map(|(_, deadline)| *deadline))
                        .chain(sync_resize.map(|(_, _, deadline)| deadline))
                        .min();
                    let command = match deadline {
                        Some(deadline) => match receiver
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        {
                            Ok(command) => command,
                            Err(RecvTimeoutError::Timeout) => continue,
                            Err(RecvTimeoutError::Disconnected) => break,
                        },
                        None => match receiver.recv() {
                            Ok(command) => command,
                            Err(_) => break,
                        },
                    };
                    match command {
                        RuntimeTimerCommand::ArmKeyChain {
                            generation,
                            timeout,
                        } => key_chain = Some((generation, Instant::now() + timeout)),
                        RuntimeTimerCommand::CancelKeyChain => key_chain = None,
                        RuntimeTimerCommand::ArmPing {
                            client,
                            generation,
                            timeout,
                        } => {
                            pings.insert(client, (generation, Instant::now() + timeout));
                        }
                        RuntimeTimerCommand::CancelPing(client) => {
                            pings.remove(&client);
                        }
                        RuntimeTimerCommand::ArmSyncResize {
                            client,
                            generation,
                            timeout,
                        } => {
                            sync_resize = Some((client, generation, Instant::now() + timeout));
                        }
                        RuntimeTimerCommand::CancelSyncResize => sync_resize = None,
                        RuntimeTimerCommand::Stop => break,
                    }
                }
            })
            .map_err(X11Error::TimerThread)?;
        Ok(Self {
            commands,
            thread: Some(thread),
        })
    }

    fn arm_key_chain(&self, generation: u32, timeout: Duration) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::ArmKeyChain {
                generation,
                timeout,
            })
            .map_err(|_| X11Error::TimerChannel)
    }

    fn cancel_key_chain(&self) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::CancelKeyChain)
            .map_err(|_| X11Error::TimerChannel)
    }

    fn arm_ping(
        &self,
        client: ClientId,
        generation: u32,
        timeout: Duration,
    ) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::ArmPing {
                client,
                generation,
                timeout,
            })
            .map_err(|_| X11Error::TimerChannel)
    }

    fn cancel_ping(&self, client: ClientId) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::CancelPing(client))
            .map_err(|_| X11Error::TimerChannel)
    }

    fn arm_sync_resize(
        &self,
        client: ClientId,
        generation: u32,
        timeout: Duration,
    ) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::ArmSyncResize {
                client,
                generation,
                timeout,
            })
            .map_err(|_| X11Error::TimerChannel)
    }

    fn cancel_sync_resize(&self) -> Result<(), X11Error> {
        self.commands
            .send(RuntimeTimerCommand::CancelSyncResize)
            .map_err(|_| X11Error::TimerChannel)
    }

    fn stop(&mut self) {
        let _ = self.commands.send(RuntimeTimerCommand::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RuntimeTimer {
    fn drop(&mut self) {
        self.stop();
    }
}

type KeyInput = (u8, u16);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct KeyBindingNode {
    children: BTreeMap<KeyInput, KeyBindingNode>,
    actions: Vec<Action>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KeyChain {
    path: Vec<KeyInput>,
    generation: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingPing {
    timestamp: u32,
    generation: u32,
    timed_out: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FullscreenMonitorIndices {
    top: u32,
    bottom: u32,
    left: u32,
    right: u32,
}

impl FullscreenMonitorIndices {
    const fn from_message(data: [u32; 5]) -> Self {
        Self {
            top: data[0],
            bottom: data[1],
            left: data[2],
            right: data[3],
        }
    }

    const fn property(self) -> [u32; 4] {
        [self.top, self.bottom, self.left, self.right]
    }
}

/// A running connection that owns the X11 window-manager selection.
pub struct WindowManager {
    connection: RustConnection,
    screen_index: usize,
    root: Window,
    support_window: Window,
    wm_selection: u32,
    wm_selection_timestamp: u32,
    desktop_layout_selection: u32,
    atoms: Atoms,
    config: Config,
    clients: ClientSet,
    titles: BTreeMap<ClientId, String>,
    icons: BTreeMap<ClientId, ClientIcon>,
    struts: BTreeMap<ClientId, EdgeReservations>,
    work_areas: Vec<Geometry>,
    output_work_areas: BTreeMap<(OutputId, WorkspaceId), Geometry>,
    root_geometry: Geometry,
    outputs: OutputSet,
    fullscreen_monitors: BTreeMap<ClientId, FullscreenMonitorIndices>,
    randr_version: Option<(u32, u32)>,
    shape_version: Option<(u16, u16)>,
    sync_version: Option<(u8, u8)>,
    bounding_shaped: BTreeSet<ClientId>,
    input_shaped: BTreeSet<ClientId>,
    user_time_windows: BTreeMap<Window, ClientId>,
    client_user_time_windows: BTreeMap<ClientId, Window>,
    default_colormap: Colormap,
    client_colormaps: BTreeMap<ClientId, Vec<ClientColormapWindow>>,
    colormap_window_owners: BTreeMap<Window, BTreeSet<ClientId>>,
    active_colormaps: Vec<Colormap>,
    sync_counters: BTreeMap<ClientId, Counter>,
    sync_resize_generation: u32,
    session_restore: SessionRestore,
    session_identities: BTreeMap<ClientId, session::SessionIdentity>,
    session_stacking: BTreeMap<ClientId, u32>,
    frames: BTreeMap<ClientId, Frame>,
    frame_parts: BTreeMap<Window, FramePart>,
    decoration_pixels: DecorationPixels,
    title_font: Font,
    title_gc: Gcontext,
    focus_overlay: FocusOverlay,
    menu_overlay: MenuOverlay,
    menu_session: Option<MenuSession>,
    menu_keycodes: MenuKeycodes,
    key_bindings: KeyBindingNode,
    chain_quit_bindings: Vec<KeyInput>,
    key_chain: Option<KeyChain>,
    key_chain_generation: u32,
    runtime_timer: RuntimeTimer,
    pending_pings: BTreeMap<ClientId, PendingPing>,
    unresponsive_clients: BTreeSet<ClientId>,
    ping_generation: u32,
    modifier_keycodes: BTreeMap<u8, u16>,
    escape_keycodes: Vec<u8>,
    ignored_modifiers: u16,
    mouse_bindings: BTreeMap<MouseBindingKey, Vec<Action>>,
    mouse_gesture: Option<MouseGesture>,
    last_mouse_click: Option<MouseClick>,
    drag: Option<Drag>,
    focus_cycle: Option<FocusCycle>,
    published_focus: Option<ClientId>,
    expected_unmaps: BTreeMap<Window, u8>,
    last_timestamp: u32,
    last_user_time: u32,
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
        Self::connect_with_session(display, config, SessionRestore::default())
    }

    /// Connects to X11 and applies single-use saved-session candidates.
    ///
    /// # Errors
    ///
    /// Returns an error if the display cannot be reached, another manager owns
    /// the root, or X11 setup fails.
    pub fn connect_with_session(
        display: Option<&str>,
        config: Config,
        session_restore: SessionRestore,
    ) -> Result<Self, X11Error> {
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
        let randr_version = query_randr_version(&connection)?;
        let shape_version = query_shape_version(&connection)?;
        let sync_version = query_sync_version(&connection)?;
        let outputs = discover_outputs(&connection, root, screen_geometry, randr_version)?;
        if let Some(version) = randr_version {
            let notify_mask = if version_at_least(version, (1, 2)) {
                NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE | NotifyMask::OUTPUT_CHANGE
            } else {
                NotifyMask::SCREEN_CHANGE
            };
            connection.randr_select_input(root, notify_mask)?.check()?;
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
        let runtime_timer = RuntimeTimer::spawn(ControlSender::connect(
            display,
            support_window,
            atoms._NOBOX_CONTROL,
        )?)?;

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

        let focus_overlay_window = connection.generate_id()?;
        connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                focus_overlay_window,
                root,
                0,
                0,
                1,
                1,
                x_u16(config.theme.border_width.clamp(1, 8)),
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(decoration_pixels.inactive_titlebar)
                    .border_pixel(decoration_pixels.active_border)
                    .override_redirect(1_u32)
                    .save_under(1_u32)
                    .event_mask(EventMask::EXPOSURE),
            )?
            .check()?;
        connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            focus_overlay_window,
            atoms._NET_WM_NAME,
            atoms.UTF8_STRING,
            b"nobox:focus-switcher",
        )?;
        connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            focus_overlay_window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"nobox-focus-switcher\0nobox\0",
        )?;
        connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            focus_overlay_window,
            atoms._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[atoms._NET_WM_WINDOW_TYPE_NOTIFICATION],
        )?;

        let menu_overlay_window = connection.generate_id()?;
        connection
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                menu_overlay_window,
                root,
                0,
                0,
                1,
                1,
                x_u16(config.theme.border_width.clamp(1, 8)),
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new()
                    .background_pixel(decoration_pixels.inactive_titlebar)
                    .border_pixel(decoration_pixels.active_border)
                    .override_redirect(1_u32)
                    .save_under(1_u32)
                    .event_mask(EventMask::EXPOSURE),
            )?
            .check()?;
        connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            menu_overlay_window,
            atoms._NET_WM_NAME,
            atoms.UTF8_STRING,
            b"nobox:menu",
        )?;
        connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            menu_overlay_window,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"nobox-menu\0nobox\0",
        )?;
        connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            menu_overlay_window,
            atoms._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[atoms._NET_WM_WINDOW_TYPE_MENU],
        )?;

        let mut clients = ClientSet::default();
        clients.set_workspace_count(u32::try_from(config.workspaces.names.len()).unwrap_or(1));
        clients.set_workspace_layout(configured_workspace_layout(&config));
        if let Some(workspace) = session_restore
            .current_workspace()
            .filter(|workspace| *workspace < clients.workspace_count())
        {
            clients.switch_workspace(WorkspaceId::new(workspace));
        }
        let work_areas =
            vec![screen_geometry; usize::try_from(clients.workspace_count()).unwrap_or(1)];
        let mut output_work_areas = BTreeMap::new();
        for output in outputs.outputs() {
            for workspace in 0..clients.workspace_count() {
                output_work_areas.insert((output.id, WorkspaceId::new(workspace)), output.geometry);
            }
        }
        let mut wm = Self {
            connection,
            screen_index,
            root,
            support_window,
            wm_selection,
            wm_selection_timestamp: timestamp,
            desktop_layout_selection,
            atoms,
            config,
            clients,
            titles: BTreeMap::new(),
            icons: BTreeMap::new(),
            struts: BTreeMap::new(),
            work_areas,
            output_work_areas,
            root_geometry: screen_geometry,
            outputs,
            fullscreen_monitors: BTreeMap::new(),
            randr_version,
            shape_version,
            sync_version,
            bounding_shaped: BTreeSet::new(),
            input_shaped: BTreeSet::new(),
            user_time_windows: BTreeMap::new(),
            client_user_time_windows: BTreeMap::new(),
            default_colormap: colormap,
            client_colormaps: BTreeMap::new(),
            colormap_window_owners: BTreeMap::new(),
            active_colormaps: vec![colormap],
            sync_counters: BTreeMap::new(),
            sync_resize_generation: 0,
            session_restore,
            session_identities: BTreeMap::new(),
            session_stacking: BTreeMap::new(),
            frames: BTreeMap::new(),
            frame_parts: BTreeMap::new(),
            decoration_pixels,
            title_font,
            title_gc,
            focus_overlay: FocusOverlay {
                window: focus_overlay_window,
                width: 1,
                height: 1,
                mapped: false,
            },
            menu_overlay: MenuOverlay {
                window: menu_overlay_window,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                mapped: false,
            },
            menu_session: None,
            menu_keycodes: MenuKeycodes::default(),
            key_bindings: KeyBindingNode::default(),
            chain_quit_bindings: Vec::new(),
            key_chain: None,
            key_chain_generation: 0,
            runtime_timer,
            pending_pings: BTreeMap::new(),
            unresponsive_clients: BTreeSet::new(),
            ping_generation: 0,
            modifier_keycodes: BTreeMap::new(),
            escape_keycodes: Vec::new(),
            ignored_modifiers: u16::from(ModMask::LOCK),
            mouse_bindings: BTreeMap::new(),
            mouse_gesture: None,
            last_mouse_click: None,
            drag: None,
            focus_cycle: None,
            published_focus: None,
            expected_unmaps: BTreeMap::new(),
            last_timestamp: timestamp,
            last_user_time: CURRENT_TIME,
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
    ) -> Result<SessionSnapshot, X11Error>
    where
        E: std::fmt::Display,
    {
        info!(
            display = ?self.connection.setup().vendor,
            screen = self.screen_index,
            root = format_args!("{:#x}", self.root),
            "nobox owns the X11 root window"
        );
        info!(
            outputs = self.outputs.outputs().len(),
            randr = ?self.randr_version,
            shape = ?self.shape_version,
            sync = ?self.sync_version,
            "using X11 output topology"
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
                Some(RuntimeRequest::KeyChainTimeout(generation)) => {
                    if self
                        .key_chain
                        .as_ref()
                        .is_some_and(|chain| chain.generation == generation)
                    {
                        self.finish_key_chain()?;
                    }
                }
                Some(RuntimeRequest::PingTimeout { client, generation }) => {
                    self.client_ping_timeout(client, generation)?;
                }
                Some(RuntimeRequest::SyncResizeTimeout { client, generation }) => {
                    self.sync_resize_timeout(client, generation)?;
                }
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
        let snapshot = self.session_snapshot();
        info!("nobox X11 event loop stopped cleanly");
        Ok(snapshot)
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
        let data = event.data.as_data32();
        runtime_request(data[0], data[1], data[2])
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

        let mut supported = vec![
            self.atoms._NET_ACTIVE_WINDOW,
            self.atoms._NET_CLOSE_WINDOW,
            self.atoms._NET_CLIENT_LIST,
            self.atoms._NET_CLIENT_LIST_STACKING,
            self.atoms._NET_CURRENT_DESKTOP,
            self.atoms._NET_DESKTOP_GEOMETRY,
            self.atoms._NET_DESKTOP_LAYOUT,
            self.atoms._NET_DESKTOP_NAMES,
            self.atoms._NET_DESKTOP_VIEWPORT,
            self.atoms._NET_FRAME_EXTENTS,
            self.atoms._NET_MOVERESIZE_WINDOW,
            self.atoms._NET_WM_FULLSCREEN_MONITORS,
            self.atoms._NET_WM_MOVERESIZE,
            self.atoms._NET_NUMBER_OF_DESKTOPS,
            self.atoms._NET_REQUEST_FRAME_EXTENTS,
            self.atoms._NET_RESTACK_WINDOW,
            self.atoms._NET_SHOWING_DESKTOP,
            self.atoms._NET_SUPPORTING_WM_CHECK,
            self.atoms._NET_WORKAREA,
            self.atoms._NET_WM_ACTION_ABOVE,
            self.atoms._NET_WM_ACTION_BELOW,
            self.atoms._NET_WM_ACTION_CHANGE_DESKTOP,
            self.atoms._NET_WM_ACTION_CLOSE,
            self.atoms._NET_WM_ACTION_FULLSCREEN,
            self.atoms._NET_WM_ACTION_MAXIMIZE_HORZ,
            self.atoms._NET_WM_ACTION_MAXIMIZE_VERT,
            self.atoms._NET_WM_ACTION_MINIMIZE,
            self.atoms._NET_WM_ACTION_MOVE,
            self.atoms._NET_WM_ACTION_RESIZE,
            self.atoms._NET_WM_ACTION_SHADE,
            self.atoms._NET_WM_ALLOWED_ACTIONS,
            self.atoms._NET_WM_ICON,
            self.atoms._NET_WM_NAME,
            self.atoms._NET_WM_PING,
            self.atoms._NET_WM_DESKTOP,
            self.atoms._NET_WM_STATE,
            self.atoms._NET_WM_STATE_ABOVE,
            self.atoms._NET_WM_STATE_BELOW,
            self.atoms._NET_WM_STATE_DEMANDS_ATTENTION,
            self.atoms._NET_WM_STATE_FOCUSED,
            self.atoms._NET_WM_STATE_FULLSCREEN,
            self.atoms._NET_WM_STATE_HIDDEN,
            self.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
            self.atoms._NET_WM_STATE_MAXIMIZED_VERT,
            self.atoms._NET_WM_STATE_MODAL,
            self.atoms._NET_WM_STATE_SHADED,
            self.atoms._NET_WM_STATE_SKIP_PAGER,
            self.atoms._NET_WM_STATE_SKIP_TASKBAR,
            self.atoms._NET_WM_USER_TIME,
            self.atoms._NET_WM_USER_TIME_WINDOW,
            self.atoms._NET_WM_VISIBLE_NAME,
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
        if self.sync_version.is_some() {
            supported.extend([
                self.atoms._NET_WM_SYNC_REQUEST,
                self.atoms._NET_WM_SYNC_REQUEST_COUNTER,
            ]);
        }
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
            self.atoms._NET_SHOWING_DESKTOP,
            AtomEnum::CARDINAL,
            &[u32::from(self.clients.showing_desktop())],
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
        self.modifier_keycodes.clear();
        if keys_per_modifier > 0 {
            for (index, keycodes) in modifier_mapping
                .keycodes
                .chunks(keys_per_modifier)
                .enumerate()
            {
                let Some(mask) = u32::try_from(index)
                    .ok()
                    .and_then(|index| 1_u16.checked_shl(index))
                else {
                    continue;
                };
                for keycode in keycodes.iter().copied().filter(|keycode| *keycode != 0) {
                    self.modifier_keycodes
                        .entry(keycode)
                        .and_modify(|existing| *existing |= mask)
                        .or_insert(mask);
                }
            }
        }
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
        let menu_keys = |symbol| {
            keycodes_for_named_symbol(
                minimum,
                mapping.keysyms_per_keycode,
                &mapping.keysyms,
                symbol,
            )
        };
        let mut enter = menu_keys("Return");
        enter.extend(menu_keys("KP_Enter"));
        enter.sort_unstable();
        enter.dedup();
        let characters = mapping
            .keysyms
            .chunks(usize::from(mapping.keysyms_per_keycode).max(1))
            .enumerate()
            .filter_map(|(offset, symbols)| {
                let offset = u8::try_from(offset).ok()?;
                let keycode = minimum.checked_add(offset)?;
                let character = symbols
                    .iter()
                    .copied()
                    .find_map(|symbol| xkeysym::Keysym::new(symbol).key_char())?;
                Some((keycode, lowercase_character(character)))
            })
            .collect();
        self.menu_keycodes = MenuKeycodes {
            up: menu_keys("Up"),
            down: menu_keys("Down"),
            left: menu_keys("Left"),
            right: menu_keys("Right"),
            home: menu_keys("Home"),
            end: menu_keys("End"),
            enter,
            characters,
        };

        self.finish_focus_cycle(CURRENT_TIME)?;
        self.hide_menu(CURRENT_TIME)?;
        self.finish_key_chain()?;
        self.connection
            .ungrab_key(Grab::ANY, self.root, ModMask::ANY)?;
        let resolve_chord = |chord: &nobox_config::KeyChord| -> Result<Vec<KeyInput>, X11Error> {
            let keycodes = keycodes_for_named_symbol(
                minimum,
                mapping.keysyms_per_keycode,
                &mapping.keysyms,
                chord.symbol(),
            );
            if keycodes.is_empty() {
                return Err(X11Error::UnknownKeySymbol(chord.symbol().to_owned()));
            }
            let modifiers = keyboard_modifier_mask(chord.modifiers());
            Ok(keycodes
                .into_iter()
                .map(|keycode| (keycode, modifiers))
                .collect())
        };
        let mut key_bindings = KeyBindingNode::default();
        for binding in &self.config.keyboard.bindings {
            let sequence = binding
                .key
                .chords()
                .iter()
                .map(&resolve_chord)
                .collect::<Result<Vec<_>, _>>()?;
            insert_key_binding_variants(&mut key_bindings, &sequence, &binding.actions)?;
        }
        self.chain_quit_bindings = resolve_chord(&self.config.keyboard.chain_quit_key)?;
        self.key_bindings = key_bindings;
        self.grab_current_key_bindings()?;
        self.reload_mouse_bindings()?;
        info!(
            bindings = self.config.keyboard.bindings.len(),
            "loaded X11 key bindings"
        );
        Ok(())
    }

    fn wm_selection_request(&self, event: &SelectionRequestEvent) -> Result<(), X11Error> {
        if event.owner != self.support_window || event.selection != self.wm_selection {
            return Ok(());
        }
        let in_ownership_period =
            event.time == CURRENT_TIME || !x11_time_after(self.wm_selection_timestamp, event.time);
        let property = if event.property == NONE {
            event.target
        } else {
            event.property
        };
        let converted = in_ownership_period
            && if event.target == self.atoms.MULTIPLE {
                event.property != NONE
                    && self.convert_wm_selection_multiple(event.requestor, event.property)?
            } else {
                self.convert_wm_selection_target(event.requestor, event.target, property)?
            };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: event.time,
            requestor: event.requestor,
            selection: event.selection,
            target: event.target,
            property: if converted { property } else { NONE },
        };
        self.connection
            .send_event(false, event.requestor, EventMask::NO_EVENT, notify)?;
        Ok(())
    }

    fn convert_wm_selection_target(
        &self,
        requestor: Window,
        target: u32,
        property: u32,
    ) -> Result<bool, X11Error> {
        if property == NONE {
            return Ok(false);
        }
        if target == self.atoms.TARGETS {
            self.connection
                .change_property32(
                    x11rb::protocol::xproto::PropMode::REPLACE,
                    requestor,
                    property,
                    AtomEnum::ATOM,
                    &[
                        self.atoms.TARGETS,
                        self.atoms.MULTIPLE,
                        self.atoms.TIMESTAMP,
                    ],
                )?
                .check()?;
            Ok(true)
        } else if target == self.atoms.TIMESTAMP {
            self.connection
                .change_property32(
                    x11rb::protocol::xproto::PropMode::REPLACE,
                    requestor,
                    property,
                    AtomEnum::INTEGER,
                    &[self.wm_selection_timestamp],
                )?
                .check()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn convert_wm_selection_multiple(
        &self,
        requestor: Window,
        property: u32,
    ) -> Result<bool, X11Error> {
        let reply = self
            .connection
            .get_property(
                false,
                requestor,
                property,
                self.atoms.ATOM_PAIR,
                0,
                MAX_SELECTION_MULTIPLE_PAIRS * 2,
            )?
            .reply()?;
        if reply.type_ != self.atoms.ATOM_PAIR || reply.format != 32 || reply.bytes_after != 0 {
            return Ok(false);
        }
        let Some(values) = reply.value32() else {
            return Ok(false);
        };
        let mut pairs: Vec<u32> = values.collect();
        if pairs.len() % 2 != 0 {
            return Ok(false);
        }
        for pair in pairs.chunks_exact_mut(2) {
            let converted = pair[0] != self.atoms.MULTIPLE
                && pair[1] != NONE
                && self.convert_wm_selection_target(requestor, pair[0], pair[1])?;
            if !converted {
                pair[0] = NONE;
            }
        }
        self.connection
            .change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                requestor,
                property,
                self.atoms.ATOM_PAIR,
                &pairs,
            )?
            .check()?;
        Ok(true)
    }

    fn grab_current_key_bindings(&self) -> Result<(), X11Error> {
        self.connection
            .ungrab_key(Grab::ANY, self.root, ModMask::ANY)?;
        let node = self.current_key_node();
        for &(keycode, modifiers) in node.children.keys().chain(
            self.key_chain
                .iter()
                .flat_map(|_| self.chain_quit_bindings.iter()),
        ) {
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
        Ok(())
    }

    fn current_key_node(&self) -> &KeyBindingNode {
        let mut node = &self.key_bindings;
        if let Some(chain) = &self.key_chain {
            for input in &chain.path {
                let Some(next) = node.children.get(input) else {
                    return &self.key_bindings;
                };
                node = next;
            }
        }
        node
    }

    fn advance_key_chain(&mut self, input: KeyInput) -> Result<(), X11Error> {
        self.key_chain_generation = self.key_chain_generation.wrapping_add(1);
        let generation = self.key_chain_generation;
        if let Some(chain) = &mut self.key_chain {
            chain.path.push(input);
            chain.generation = generation;
        } else {
            self.key_chain = Some(KeyChain {
                path: vec![input],
                generation,
            });
        }
        self.grab_current_key_bindings()?;
        self.runtime_timer.arm_key_chain(
            generation,
            Duration::from_millis(u64::from(self.config.keyboard.chain_timeout_ms)),
        )?;
        debug!(generation, "entered X11 keyboard chain");
        Ok(())
    }

    fn finish_key_chain(&mut self) -> Result<(), X11Error> {
        if self.key_chain.take().is_some() {
            self.runtime_timer.cancel_key_chain()?;
            self.grab_current_key_bindings()?;
            debug!("finished X11 keyboard chain");
        }
        Ok(())
    }

    fn reload_mouse_bindings(&mut self) -> Result<(), X11Error> {
        let legacy_modifiers = u16::from(self.modifier_mask());
        let mut bindings = BTreeMap::new();
        for (button, action) in [
            (self.config.mouse.move_button, Action::Move),
            (self.config.mouse.resize_button, Action::Resize),
        ] {
            bindings.insert(
                MouseBindingKey {
                    context: MouseContext::Frame,
                    button,
                    modifiers: legacy_modifiers,
                    trigger: MouseTrigger::Drag,
                },
                vec![action],
            );
        }
        for binding in &self.config.mouse.bindings {
            bindings.insert(
                MouseBindingKey {
                    context: binding.context,
                    button: binding.button.button().number(),
                    modifiers: keyboard_modifier_mask(binding.button.modifiers()),
                    trigger: binding.trigger,
                },
                binding.actions.clone(),
            );
        }
        self.mouse_bindings = bindings;
        self.mouse_gesture = None;
        self.last_mouse_click = None;

        self.connection
            .ungrab_button(ButtonIndex::ANY, self.root, ModMask::ANY)?;
        let grabs = self
            .mouse_bindings
            .keys()
            .filter(|binding| binding.modifiers != 0)
            .map(|binding| (binding.button, binding.modifiers))
            .collect::<BTreeSet<_>>();
        for (button, modifiers) in grabs {
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
                        ModMask::from(modifiers | locks),
                    )?
                    .check()?;
            }
        }
        info!(
            bindings = self.mouse_bindings.len(),
            "loaded X11 mouse bindings"
        );
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
        self.hide_menu(self.last_timestamp)?;
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
        self.connection.change_window_attributes(
            self.focus_overlay.window,
            &ChangeWindowAttributesAux::new()
                .background_pixel(self.decoration_pixels.inactive_titlebar)
                .border_pixel(self.decoration_pixels.active_border),
        )?;
        self.connection.configure_window(
            self.focus_overlay.window,
            &ConfigureWindowAux::new().border_width(self.config.theme.border_width.clamp(1, 8)),
        )?;
        self.connection.change_window_attributes(
            self.menu_overlay.window,
            &ChangeWindowAttributesAux::new()
                .background_pixel(self.decoration_pixels.inactive_titlebar)
                .border_pixel(self.decoration_pixels.active_border),
        )?;
        self.connection.configure_window(
            self.menu_overlay.window,
            &ConfigureWindowAux::new().border_width(self.config.theme.border_width.clamp(1, 8)),
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
        let (border, titlebar) = if self.unresponsive_clients.contains(&id) {
            (
                self.decoration_pixels.urgent_border,
                self.decoration_pixels.urgent_titlebar,
            )
        } else if self.clients.focused() == Some(id) {
            (
                self.decoration_pixels.active_border,
                self.decoration_pixels.active_titlebar,
            )
        } else if self
            .clients
            .get(id)
            .is_some_and(|client| client.presentation.urgent)
        {
            (
                self.decoration_pixels.urgent_border,
                self.decoration_pixels.urgent_titlebar,
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

    fn read_cardinal_property(&self, window: Window, atom: u32) -> Result<Option<u32>, X11Error> {
        let reply = self
            .connection
            .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 1)?
            .reply()?;
        Ok(reply.value32().and_then(|mut values| values.next()))
    }

    fn read_window_property(&self, window: Window, atom: u32) -> Result<Option<Window>, X11Error> {
        let reply = self
            .connection
            .get_property(false, window, atom, AtomEnum::WINDOW, 0, 1)?
            .reply()?;
        Ok(reply
            .value32()
            .and_then(|mut values| values.next())
            .filter(|window| *window != NONE))
    }

    fn read_client_user_time(&self, window: Window) -> Result<Option<u32>, X11Error> {
        if let Some(timestamp) =
            self.read_cardinal_property(window, self.atoms._NET_WM_USER_TIME)?
        {
            return Ok(Some(timestamp));
        }
        let Some(time_window) =
            self.read_window_property(window, self.atoms._NET_WM_USER_TIME_WINDOW)?
        else {
            return Ok(None);
        };
        match self.read_cardinal_property(time_window, self.atoms._NET_WM_USER_TIME) {
            Ok(timestamp) => Ok(timestamp),
            Err(error) if error.is_vanished_window() => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn refresh_user_time_window(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        if let Some(previous) = self.client_user_time_windows.remove(&id)
            && self.user_time_windows.get(&previous) == Some(&id)
        {
            self.user_time_windows.remove(&previous);
        }
        let Some(time_window) =
            self.read_window_property(window, self.atoms._NET_WM_USER_TIME_WINDOW)?
        else {
            return Ok(());
        };
        let attributes = match self.connection.get_window_attributes(time_window)?.reply() {
            Ok(attributes) => attributes,
            Err(ReplyError::X11Error(_)) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        self.connection.change_window_attributes(
            time_window,
            &ChangeWindowAttributesAux::new()
                .event_mask(attributes.your_event_mask | EventMask::PROPERTY_CHANGE),
        )?;
        self.user_time_windows.insert(time_window, id);
        self.client_user_time_windows.insert(id, time_window);
        Ok(())
    }

    fn read_client_colormaps(&self, window: Window) -> Result<Vec<ClientColormapWindow>, X11Error> {
        let reply = self
            .connection
            .get_property(
                false,
                window,
                self.atoms.WM_COLORMAP_WINDOWS,
                AtomEnum::WINDOW,
                0,
                u32::try_from(MAX_CLIENT_COLORMAP_WINDOWS).expect("small colormap window limit"),
            )?
            .reply()?;
        let listed = if reply.type_ == u32::from(AtomEnum::WINDOW) && reply.format == 32 {
            reply
                .value32()
                .map_or_else(Vec::new, |windows| windows.collect())
        } else {
            Vec::new()
        };
        let windows = prioritized_colormap_windows(window, &listed);
        let mut colormaps = Vec::with_capacity(windows.len());
        for watched in windows {
            let geometry = match self.connection.get_geometry(watched)?.reply() {
                Ok(geometry) => geometry,
                Err(ReplyError::X11Error(error))
                    if error.error_kind == ErrorKind::Drawable
                        || error.error_kind == ErrorKind::Window =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if geometry.root != self.root {
                continue;
            }
            let attributes = match self.connection.get_window_attributes(watched)?.reply() {
                Ok(attributes) => attributes,
                Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Window => {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            colormaps.push(ClientColormapWindow {
                window: watched,
                colormap: attributes.colormap,
            });
        }
        Ok(colormaps)
    }

    fn set_colormap_watch(&self, window: Window, watch: bool) -> Result<bool, X11Error> {
        if !watch && self.clients.contains(client_id(window)) {
            return Ok(true);
        }
        let attributes = match self.connection.get_window_attributes(window)?.reply() {
            Ok(attributes) => attributes,
            Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Window => {
                return Ok(false);
            }
            Err(error) => return Err(error.into()),
        };
        let current = attributes.your_event_mask;
        let events = if watch {
            current | EventMask::COLOR_MAP_CHANGE
        } else {
            EventMask::from(u32::from(current) & !u32::from(EventMask::COLOR_MAP_CHANGE))
        };
        if events == current {
            return Ok(true);
        }
        window_request_succeeded(
            self.connection
                .change_window_attributes(
                    window,
                    &ChangeWindowAttributesAux::new().event_mask(events),
                )?
                .check(),
        )
    }

    fn add_colormap_owner(&mut self, window: Window, owner: ClientId) -> Result<(), X11Error> {
        let owners = self.colormap_window_owners.entry(window).or_default();
        if !owners.insert(owner) || owners.len() > 1 {
            return Ok(());
        }
        if !self.set_colormap_watch(window, true)? {
            self.colormap_window_owners.remove(&window);
        }
        Ok(())
    }

    fn remove_colormap_owner(&mut self, window: Window, owner: ClientId) -> Result<(), X11Error> {
        let remove_watch = if let Some(owners) = self.colormap_window_owners.get_mut(&window) {
            owners.remove(&owner);
            owners.is_empty()
        } else {
            false
        };
        if remove_watch {
            self.colormap_window_owners.remove(&window);
            let _ = self.set_colormap_watch(window, false)?;
        }
        Ok(())
    }

    fn refresh_client_colormaps(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        let colormaps = self.read_client_colormaps(window)?;
        let previous = self
            .client_colormaps
            .get(&id)
            .into_iter()
            .flatten()
            .map(|entry| entry.window)
            .collect::<BTreeSet<_>>();
        let current = colormaps
            .iter()
            .map(|entry| entry.window)
            .collect::<BTreeSet<_>>();
        for watched in current.difference(&previous).copied() {
            self.add_colormap_owner(watched, id)?;
        }
        for watched in previous.difference(&current).copied() {
            self.remove_colormap_owner(watched, id)?;
        }
        self.client_colormaps.insert(id, colormaps);
        if self.clients.focused() == Some(id) {
            self.sync_colormap_focus()?;
        }
        Ok(())
    }

    fn remove_client_colormaps(&mut self, id: ClientId) -> Result<(), X11Error> {
        let Some(colormaps) = self.client_colormaps.remove(&id) else {
            return Ok(());
        };
        for watched in colormaps.into_iter().map(|entry| entry.window) {
            self.remove_colormap_owner(watched, id)?;
        }
        Ok(())
    }

    fn colormap_notify(&mut self, event: &ColormapNotifyEvent) -> Result<(), X11Error> {
        if !event.new {
            return Ok(());
        }
        let owners = self
            .colormap_window_owners
            .get(&event.window)
            .cloned()
            .unwrap_or_default();
        let focused = self.clients.focused();
        let mut focused_changed = false;
        for owner in owners {
            let Some(colormaps) = self.client_colormaps.get_mut(&owner) else {
                continue;
            };
            for entry in colormaps
                .iter_mut()
                .filter(|entry| entry.window == event.window)
            {
                entry.colormap = event.colormap;
                focused_changed |= focused == Some(owner);
            }
        }
        if focused_changed {
            self.sync_colormap_focus()?;
        }
        Ok(())
    }

    fn sync_colormap_focus(&mut self) -> Result<(), X11Error> {
        let mut desired = Vec::new();
        let mut seen = BTreeSet::new();
        if let Some(focused) = self.clients.focused()
            && let Some(colormaps) = self.client_colormaps.get(&focused)
        {
            for entry in colormaps {
                if entry.colormap != NONE && seen.insert(entry.colormap) {
                    desired.push(entry.colormap);
                }
            }
        }
        if desired.is_empty() {
            desired.push(self.default_colormap);
            seen.insert(self.default_colormap);
        }

        for colormap in self.active_colormaps.iter().copied() {
            if !seen.contains(&colormap) {
                let _ = colormap_request_succeeded(
                    self.connection.uninstall_colormap(colormap)?.check(),
                )?;
            }
        }

        let mut active = Vec::with_capacity(desired.len());
        for colormap in desired.iter().rev().copied() {
            if colormap_request_succeeded(self.connection.install_colormap(colormap)?.check())? {
                active.push(colormap);
            }
        }
        active.reverse();
        self.active_colormaps = active;
        Ok(())
    }

    fn refresh_sync_counter(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        self.sync_counters.remove(&id);
        if self.sync_version.is_none()
            || !self.supports_protocol(window, self.atoms._NET_WM_SYNC_REQUEST)?
        {
            return Ok(());
        }
        let Some(counter) =
            self.read_cardinal_property(window, self.atoms._NET_WM_SYNC_REQUEST_COUNTER)?
        else {
            return Ok(());
        };
        if counter == NONE
            || !sync_request_succeeded(
                self.connection
                    .sync_set_counter(counter, sync_value(0))?
                    .check(),
            )?
        {
            return Ok(());
        }
        self.sync_counters.insert(id, counter);
        Ok(())
    }

    fn record_user_time(&mut self, timestamp: u32) {
        if timestamp != CURRENT_TIME
            && (self.last_user_time == CURRENT_TIME
                || x11_time_after(timestamp, self.last_user_time))
        {
            self.last_user_time = timestamp;
        }
    }

    fn focus_request_allowed(
        &self,
        id: ClientId,
        timestamp: Option<u32>,
        user_request: bool,
        forced: bool,
    ) -> bool {
        if user_request {
            return true;
        }
        if timestamp == Some(CURRENT_TIME) {
            return false;
        }
        if forced || !self.config.focus.prevent_focus_stealing {
            return true;
        }
        let Some(timestamp) = timestamp else {
            return true;
        };
        if self.last_user_time == CURRENT_TIME || !x11_time_after(self.last_user_time, timestamp) {
            return true;
        }
        self.clients
            .focused()
            .is_some_and(|focused| self.clients.clients_are_related(id, focused))
    }

    fn demand_attention(&mut self, id: ClientId) -> Result<(), X11Error> {
        let window = window_id(id);
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_DEMANDS_ATTENTION, true)?;
        self.refresh_client_presentation(window)
    }

    fn read_application_identity(
        &self,
        window: Window,
        role: ClientRole,
    ) -> Result<X11ApplicationIdentity, X11Error> {
        let class_reply = self
            .connection
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 2048)?
            .reply()?;
        let (name, class) = parse_wm_class(&class_reply.value);
        let role_reply = self
            .connection
            .get_property(
                false,
                window,
                self.atoms.WM_WINDOW_ROLE,
                AtomEnum::STRING,
                0,
                1024,
            )?
            .reply()?;
        let identity = X11ApplicationIdentity {
            name,
            class,
            role: x11_text(&role_reply.value),
            title: self.read_title(window)?,
            kind: application_kind(role),
        };
        debug!(
            window = format_args!("{window:#x}"),
            name = identity.name,
            class = identity.class,
            role = identity.role,
            title = identity.title,
            kind = ?identity.kind,
            "read X11 application identity"
        );
        Ok(identity)
    }

    fn read_session_identity(
        &self,
        window: Window,
        application: &X11ApplicationIdentity,
    ) -> Result<Option<session::SessionIdentity>, X11Error> {
        let leader = self
            .read_window_property(window, self.atoms.WM_CLIENT_LEADER)?
            .unwrap_or(window);
        let session_id = self.read_bounded_text_property(leader, self.atoms.SM_CLIENT_ID, 1024)?;
        let mut command = self.read_wm_command(leader)?;
        if command.is_empty() && leader != window {
            command = self.read_wm_command(window)?;
        }
        if session_id.is_none() && command.is_empty() {
            return Ok(None);
        }
        Ok(Some(session::SessionIdentity {
            session_id,
            command,
            instance: application.name.clone(),
            class: application.class.clone(),
            role: application.role.clone(),
            kind: application_kind_name(application.kind).to_owned(),
        }))
    }

    fn read_bounded_text_property(
        &self,
        window: Window,
        property: u32,
        maximum_bytes: u32,
    ) -> Result<Option<String>, X11Error> {
        let reply = self
            .connection
            .get_property(
                false,
                window,
                property,
                AtomEnum::STRING,
                0,
                maximum_bytes.div_ceil(4),
            )?
            .reply()?;
        if reply.bytes_after != 0 || reply.value.is_empty() || reply.value.contains(&0) {
            return Ok(None);
        }
        Ok(Some(x11_text(&reply.value)))
    }

    fn read_wm_command(&self, window: Window) -> Result<Vec<String>, X11Error> {
        let reply = self
            .connection
            .get_property(
                false,
                window,
                AtomEnum::WM_COMMAND,
                AtomEnum::STRING,
                0,
                1024,
            )?
            .reply()?;
        if reply.bytes_after != 0 {
            return Ok(Vec::new());
        }
        Ok(reply
            .value
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .take(64)
            .map(x11_text)
            .collect())
    }

    fn read_application_settings(
        &self,
        window: Window,
        role: ClientRole,
    ) -> Result<ApplicationSettings, X11Error> {
        if self.config.applications.is_empty() {
            return Ok(ApplicationSettings::default());
        }
        let identity = self.read_application_identity(window, role)?;
        Ok(self
            .config
            .application_settings(identity.as_application_identity()))
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
        self.sync_visible_title(id)?;
        self.draw_title(id)?;
        if self
            .focus_cycle
            .as_ref()
            .is_some_and(|cycle| cycle.candidates.contains(&id))
        {
            self.draw_focus_overlay()?;
        }
        Ok(())
    }

    fn read_client_icon(&self, window: Window) -> Result<Option<ClientIcon>, X11Error> {
        let reply = self
            .connection
            .get_property(
                false,
                window,
                self.atoms._NET_WM_ICON,
                AtomEnum::CARDINAL,
                0,
                MAX_CLIENT_ICON_PROPERTY_VALUES,
            )?
            .reply()?;
        let values = reply
            .value32()
            .map_or_else(Vec::new, |values| values.collect());
        Ok(parse_client_icon(&values, PREFERRED_CLIENT_ICON_SIZE))
    }

    fn refresh_client_icon(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        match self.read_client_icon(window)? {
            Some(icon) => {
                debug!(
                    window = format_args!("{window:#x}"),
                    width = icon.width,
                    height = icon.height,
                    pixels = icon.argb.len(),
                    "updated client icon metadata"
                );
                self.icons.insert(id, icon);
            }
            None => {
                self.icons.remove(&id);
                debug!(
                    window = format_args!("{window:#x}"),
                    "cleared client icon metadata"
                );
            }
        }
        Ok(())
    }

    fn draw_title(&self, id: ClientId) -> Result<(), X11Error> {
        let Some(frame) = self.frames.get(&id).copied() else {
            return Ok(());
        };
        let titlebar_height = frame.extents.top.saturating_sub(frame.extents.left);
        let Some(client) = self.clients.get(id) else {
            return Ok(());
        };
        let unresponsive = self.unresponsive_clients.contains(&id);
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
        let mut text = title_text_bytes(
            self.titles.get(&id).map_or("", String::as_str),
            if unresponsive {
                max_characters.saturating_sub(b" (Not Responding)".len())
            } else {
                max_characters
            },
        );
        if unresponsive {
            text.extend_from_slice(b" (Not Responding)");
            text.truncate(max_characters);
        }
        if !text.is_empty() {
            let background = if unresponsive {
                self.decoration_pixels.urgent_titlebar
            } else if self.clients.focused() == Some(id) {
                self.decoration_pixels.active_titlebar
            } else if client.presentation.urgent {
                self.decoration_pixels.urgent_titlebar
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

    fn sync_visible_title(&self, id: ClientId) -> Result<(), X11Error> {
        let window = window_id(id);
        if !self.unresponsive_clients.contains(&id) {
            self.connection
                .delete_property(window, self.atoms._NET_WM_VISIBLE_NAME)?;
            return Ok(());
        }
        let title = self.titles.get(&id).map_or("", String::as_str);
        let suffix = " (Not Responding)";
        let mut visible = Vec::with_capacity(title.len().saturating_add(suffix.len()));
        visible.extend_from_slice(title.as_bytes());
        visible.extend_from_slice(suffix.as_bytes());
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window,
            self.atoms._NET_WM_VISIBLE_NAME,
            self.atoms.UTF8_STRING,
            &visible,
        )?;
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
            &CreateWindowAux::new().background_pixel(pixel).event_mask(
                EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::BUTTON_MOTION
                    | EventMask::EXPOSURE,
            ),
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
                        | EventMask::ENTER_WINDOW
                        | EventMask::FOCUS_CHANGE
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
        if self
            .clients
            .get(client_id(client))
            .is_none_or(|managed| !managed.shaded)
        {
            self.connection.map_window(client)?;
        }
        self.connection.map_window(frame.window)?;
        Ok(())
    }

    fn initialize_client_shape(&mut self, window: Window) -> Result<(), X11Error> {
        let Some(version) = self.shape_version else {
            return Ok(());
        };
        self.connection.shape_select_input(window, true)?;
        let id = client_id(window);
        let extents = self.connection.shape_query_extents(window)?.reply()?;
        self.refresh_client_shape(window, SK::BOUNDING, extents.bounding_shaped)?;

        if shape_version_at_least(version, (1, 1)) {
            let geometry = self.clients.get(id).map(|client| client.geometry);
            let rectangles = self
                .connection
                .shape_get_rectangles(window, SK::INPUT)?
                .reply()?
                .rectangles;
            let input_shaped = !geometry.is_some_and(|geometry| {
                rectangles.as_slice().first().is_some_and(|rectangle| {
                    rectangles.len() == 1
                        && rectangle.x == 0
                        && rectangle.y == 0
                        && rectangle.width == x_dimension(geometry.width)
                        && rectangle.height == x_dimension(geometry.height)
                })
            });
            self.refresh_client_shape(window, SK::INPUT, input_shaped)?;
        }
        Ok(())
    }

    fn refresh_client_shape(
        &mut self,
        window: Window,
        kind: SK,
        shaped: bool,
    ) -> Result<(), X11Error> {
        let id = client_id(window);
        let tracked = if kind == SK::BOUNDING {
            &mut self.bounding_shaped
        } else if kind == SK::INPUT {
            &mut self.input_shaped
        } else {
            return Ok(());
        };
        if shaped {
            tracked.insert(id);
        } else {
            tracked.remove(&id);
        }
        self.apply_frame_shape(id, kind, shaped)
    }

    fn apply_frame_shape(&self, id: ClientId, kind: SK, shaped: bool) -> Result<(), X11Error> {
        let Some(frame) = self.frames.get(&id).copied() else {
            return Ok(());
        };
        if !shaped {
            self.connection
                .shape_mask(SO::SET, kind, frame.window, 0, 0, NONE)?;
            return Ok(());
        }
        let Some(client) = self.clients.get(id) else {
            return Ok(());
        };
        let titlebar_height = frame.extents.top.saturating_sub(frame.extents.left);
        self.connection.shape_combine(
            SO::SET,
            kind,
            kind,
            frame.window,
            0,
            clamp_i16_u32(titlebar_height),
            window_id(id),
        )?;
        if titlebar_height > 0 {
            self.connection.shape_rectangles(
                SO::UNION,
                kind,
                ClipOrdering::UNSORTED,
                frame.window,
                0,
                0,
                &[Rectangle {
                    x: 0,
                    y: 0,
                    width: x_dimension(client.geometry.width),
                    height: x_dimension(titlebar_height),
                }],
            )?;
        }
        Ok(())
    }

    fn frame_window(&self, id: ClientId) -> Window {
        self.frames
            .get(&id)
            .map_or_else(|| window_id(id), |frame| frame.window)
    }

    fn initial_placement(
        &self,
        content_size: Size,
        policy: ClientPolicy,
        transient_for: Option<TransientTarget>,
        assignment: WorkspaceAssignment,
    ) -> Geometry {
        let workspace = match assignment {
            WorkspaceAssignment::Workspace(workspace) => workspace,
            WorkspaceAssignment::All => self.clients.current_workspace(),
        };
        let anchor_client = match transient_for {
            Some(TransientTarget::Client(parent)) => self.clients.get(parent),
            _ => self
                .clients
                .focused()
                .and_then(|focused| self.clients.get(focused)),
        };
        let output = anchor_client.map_or_else(
            || self.outputs.primary(),
            |client| self.outputs.output_for(client.geometry),
        );
        let bounds = self
            .output_work_areas
            .get(&(output.id, workspace))
            .copied()
            .unwrap_or(output.geometry);
        let extents = self.decoration_extents(policy);
        let outer_size = {
            let outer = extents.outer_geometry(Geometry::new(
                0,
                0,
                content_size.width,
                content_size.height,
            ));
            Size::new(outer.width, outer.height)
        };
        let parent_anchor = match transient_for {
            Some(TransientTarget::Client(parent)) if policy.role == ClientRole::Dialog => {
                self.clients.get(parent).map(|parent| {
                    let parent_extents = self.frames.get(&parent.id).map_or_else(
                        || self.decoration_extents(parent.policy),
                        |frame| frame.extents,
                    );
                    parent_extents.outer_geometry(parent.geometry)
                })
            }
            Some(TransientTarget::Group) | Some(TransientTarget::Client(_)) | None => None,
        };
        let outer = if let Some(parent) = parent_anchor {
            centered_placement(outer_size, bounds, parent)
        } else if matches!(policy.role, ClientRole::Dialog | ClientRole::Splash) {
            centered_placement(outer_size, bounds, bounds)
        } else {
            let obstacles = self
                .clients
                .stacking()
                .filter_map(|id| self.clients.get(id))
                .filter(|client| {
                    !client.iconic
                        && client.workspace.is_visible_on(workspace)
                        && role_occupies_placement_space(client.policy.role)
                })
                .map(|client| {
                    let extents = self.frames.get(&client.id).map_or_else(
                        || self.decoration_extents(client.policy),
                        |frame| frame.extents,
                    );
                    extents.outer_geometry(client.geometry)
                })
                .collect::<Vec<_>>();
            smart_placement(
                outer_size,
                bounds,
                &obstacles,
                self.config.placement.center_free_space,
            )
        };
        Geometry::new(
            add_root_offset(outer.x, extents.left),
            add_root_offset(outer.y, extents.top),
            content_size.width,
            content_size.height,
        )
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
        let wm_hints = WmHints::get(&self.connection, window)?.reply()?;
        let mut initially_iconic = map
            && matches!(
                wm_hints.and_then(|hints| hints.initial_state),
                Some(WmHintsState::Iconic)
            );
        let normal_hints = self.read_normal_hints(window)?;
        let size_hints = normal_hints.size;
        let relationships = self.read_relationships(window)?;
        let user_time = self.read_client_user_time(window)?;
        let initial_states = self.read_atom_list(window, self.atoms._NET_WM_STATE)?;
        let mut initially_maximized_horizontal =
            initial_states.contains(&self.atoms._NET_WM_STATE_MAXIMIZED_HORZ);
        let mut initially_maximized_vertical =
            initial_states.contains(&self.atoms._NET_WM_STATE_MAXIMIZED_VERT);
        let mut initially_fullscreen =
            initial_states.contains(&self.atoms._NET_WM_STATE_FULLSCREEN);
        let mut initially_shaded = initial_states.contains(&self.atoms._NET_WM_STATE_SHADED);
        let mut presentation = ClientPresentation {
            skip_taskbar: initial_states.contains(&self.atoms._NET_WM_STATE_SKIP_TASKBAR),
            skip_pager: initial_states.contains(&self.atoms._NET_WM_STATE_SKIP_PAGER),
            urgent: wm_hints.is_some_and(|hints| hints.urgent)
                || initial_states.contains(&self.atoms._NET_WM_STATE_DEMANDS_ATTENTION),
        };
        let client_layer = client_layer_from_states(
            &initial_states,
            self.atoms._NET_WM_STATE_ABOVE,
            self.atoms._NET_WM_STATE_BELOW,
        );
        let client_policy =
            self.read_client_policy(window, relationships.transient_for.is_some())?;
        let application_identity = self.read_application_identity(window, client_policy.role)?;
        let application = self
            .config
            .application_settings(application_identity.as_application_identity());
        let session_identity = self.read_session_identity(window, &application_identity)?;
        let restored = session_identity
            .as_ref()
            .and_then(|identity| self.session_restore.take_match(identity));
        let policy = apply_size_capabilities(
            apply_application_decorations(client_policy, application.decorated),
            size_hints,
        );
        let mut initial_layer = application.layer.map_or(client_layer, application_layer);
        let rule_workspace = application.workspace.map(|workspace| {
            WorkspaceAssignment::Workspace(WorkspaceId::new(workspace.saturating_sub(1)))
        });
        let mut focus_new = application.focus.unwrap_or(self.config.focus.focus_new);
        let decoration_override = restored
            .as_ref()
            .map_or(DecorationOverride::Default, |saved| {
                session_decoration_override(saved.decoration_override)
            });
        let effective_policy = policy.with_decoration_override(decoration_override);
        let mut workspace =
            self.read_workspace_assignment(window, policy, relationships.transient_for)?;
        let titlebar_height = if effective_policy.decorations.titlebar {
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
        let requested_geometry = Geometry::new(
            i32::from(geometry.x),
            i32::from(geometry.y),
            constrained.width,
            constrained.height,
        );
        let placement_assignment = rule_workspace.unwrap_or(workspace);
        let mut content_geometry =
            if map && !normal_hints.positioned && role_occupies_placement_space(policy.role) {
                self.initial_placement(
                    constrained,
                    policy,
                    relationships.transient_for,
                    placement_assignment,
                )
            } else {
                requested_geometry
            };
        if let Some(saved) = &restored {
            let restored_size = x_content_size(
                size_hints.constrain(Size::new(saved.width, saved.height)),
                titlebar_height,
            );
            content_geometry =
                Geometry::new(saved.x, saved.y, restored_size.width, restored_size.height);
            workspace = saved
                .workspace
                .map_or(WorkspaceAssignment::All, |workspace| {
                    WorkspaceAssignment::Workspace(WorkspaceId::new(
                        workspace.min(self.clients.workspace_count().saturating_sub(1)),
                    ))
                });
            initially_iconic = saved.iconic;
            initially_shaded = saved.shaded;
            initially_fullscreen = saved.fullscreen;
            initially_maximized_horizontal = saved.maximized_horizontal;
            initially_maximized_vertical = saved.maximized_vertical;
            presentation.skip_taskbar = saved.skip_taskbar;
            presentation.skip_pager = saved.skip_pager;
            initial_layer = session_layer(saved.layer);
            focus_new = saved.focused;
        }
        let output_coverage = legacy_output_coverage(
            content_geometry,
            policy,
            initially_maximized_horizontal || initially_maximized_vertical,
            initially_fullscreen,
            &self.outputs,
            self.root_geometry,
        );
        if content_geometry
            != Geometry::new(
                i32::from(geometry.x),
                i32::from(geometry.y),
                u32::from(geometry.width),
                u32::from(geometry.height),
            )
        {
            self.connection.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .x(content_geometry.x)
                    .y(content_geometry.y)
                    .width(content_geometry.width)
                    .height(content_geometry.height),
            )?;
        }
        let id = client_id(window);
        if let Some(identity) = session_identity {
            self.session_identities.insert(id, identity);
        }
        if let Some(saved) = &restored {
            self.session_stacking.insert(id, saved.stacking_index);
        }
        let is_new = self.clients.manage(Client {
            id,
            geometry: content_geometry,
            size_hints,
            gravity: normal_hints.gravity,
            policy,
            natural_decorations: policy.decorations,
            decoration_override,
            presentation,
            transient_for: relationships.transient_for,
            group: relationships.group,
            modal: relationships.modal,
            iconic: initially_iconic,
            shaded: false,
            workspace,
            layer: initial_layer,
            maximize: None,
            fullscreen: None,
            output_coverage,
        });

        let frame = self.create_frame(
            window,
            content_geometry,
            effective_policy,
            geometry.border_width,
            attributes.map_state != MapState::UNMAPPED,
        )?;
        self.frames.insert(id, frame);
        self.initialize_client_shape(window)?;
        self.refresh_user_time_window(window)?;
        self.refresh_client_colormaps(window)?;
        self.refresh_sync_counter(window)?;
        self.refresh_frame_colors(id)?;
        self.refresh_title(window)?;
        self.refresh_client_icon(window)?;
        self.refresh_strut(window)?;
        if initially_shaded && !initially_fullscreen {
            self.set_shaded(window, true)?;
        }
        self.publish_client_workspace(window, workspace)?;
        self.sync_layer_state(window, initial_layer)?;
        self.sync_boolean_state(
            window,
            self.atoms._NET_WM_STATE_SKIP_TASKBAR,
            presentation.skip_taskbar,
        )?;
        self.sync_boolean_state(
            window,
            self.atoms._NET_WM_STATE_SKIP_PAGER,
            presentation.skip_pager,
        )?;
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
        self.publish_allowed_actions(id)?;
        self.set_wm_state(
            window,
            if initially_iconic || !self.clients.is_visible(id) {
                WM_STATE_ICONIC
            } else {
                WM_STATE_NORMAL
            },
        )?;
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_HIDDEN, initially_iconic)?;
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_FOCUSED, false)?;
        if restored.is_none()
            && let Some(workspace) = rule_workspace
        {
            self.move_to_workspace(id, workspace, self.last_timestamp, false)?;
        }
        if !initially_iconic && self.clients.is_visible(id) {
            self.map_frame(window, frame)?;
            self.enforce_layers()?;
        }

        if is_new {
            self.restore_session_stacking(id)?;
            info!(window = format_args!("{window:#x}"), "managing X11 client");
            self.update_client_lists()?;
        }
        let focus_candidate = focus_new
            && !initially_iconic
            && self.clients.is_visible(id)
            && policy.capabilities.focusable;
        if focus_candidate {
            if restored.as_ref().is_some_and(|saved| saved.focused)
                || self.focus_request_allowed(id, user_time, false, application.focus == Some(true))
            {
                self.focus(window, self.last_timestamp)?;
            } else {
                debug!(
                    window = format_args!("{window:#x}"),
                    ?user_time,
                    last_user_time = self.last_user_time,
                    "prevented newly mapped client from stealing focus"
                );
                self.demand_attention(id)?;
            }
        }
        Ok(())
    }

    fn unmanage(&mut self, window: Window, withdrawn: bool) -> Result<(), X11Error> {
        let id = client_id(window);
        if self.drag.is_some_and(|drag| drag.window == window) {
            self.finish_drag(self.last_timestamp)?;
        }
        if self
            .mouse_gesture
            .is_some_and(|gesture| gesture.target.client == Some(id))
        {
            self.mouse_gesture = None;
        }
        if self
            .last_mouse_click
            .is_some_and(|click| click.target.client == Some(id))
        {
            self.last_mouse_click = None;
        }
        let was_focused = self.clients.focused() == Some(id);
        let geometry = self.clients.get(id).map(|client| client.geometry);
        if !self.clients.unmanage(id) {
            return Ok(());
        }
        let had_fullscreen_monitors = self.fullscreen_monitors.remove(&id).is_some();
        if self.pending_pings.remove(&id).is_some() {
            self.runtime_timer.cancel_ping(id)?;
        }
        if self.unresponsive_clients.remove(&id) {
            let _ = window_request_succeeded(
                self.connection
                    .delete_property(window, self.atoms._NET_WM_VISIBLE_NAME)?
                    .check(),
            )?;
        }
        self.bounding_shaped.remove(&id);
        self.input_shaped.remove(&id);
        self.remove_client_colormaps(id)?;
        self.sync_counters.remove(&id);
        self.session_identities.remove(&id);
        self.session_stacking.remove(&id);
        if let Some(time_window) = self.client_user_time_windows.remove(&id)
            && self.user_time_windows.get(&time_window) == Some(&id)
        {
            self.user_time_windows.remove(&time_window);
        }
        self.titles.remove(&id);
        self.icons.remove(&id);
        self.remove_focus_cycle_candidate(id)?;
        if let Some(session) = &mut self.menu_session
            && session.target == Some(id)
        {
            session.target = None;
        }
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
                if client_exists {
                    client_exists = window_request_succeeded(
                        self.connection
                            .delete_property(window, self.atoms._NET_WM_ALLOWED_ACTIONS)?
                            .check(),
                    )?;
                }
            }
            self.connection.destroy_window(frame.window)?;
        }
        if withdrawn && client_exists && had_fullscreen_monitors {
            client_exists = window_request_succeeded(
                self.connection
                    .delete_property(window, self.atoms._NET_WM_FULLSCREEN_MONITORS)?
                    .check(),
            )?;
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

    fn remove_focus_cycle_candidate(&mut self, id: ClientId) -> Result<(), X11Error> {
        let Some(cycle) = &mut self.focus_cycle else {
            return Ok(());
        };
        let Some(removed) = cycle
            .candidates
            .iter()
            .position(|candidate| *candidate == id)
        else {
            return Ok(());
        };
        cycle.candidates.remove(removed);
        if cycle.original == Some(id) {
            cycle.original = None;
        }
        if cycle.candidates.is_empty() {
            return self.finish_focus_cycle(self.last_timestamp);
        }
        cycle.index = cycle.index.map(|index| {
            if removed < index {
                index - 1
            } else {
                index.min(cycle.candidates.len() - 1)
            }
        });
        self.update_focus_overlay()
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

    fn enter_notify(&mut self, event: &EnterNotifyEvent) -> Result<(), X11Error> {
        if !self.config.focus.follow_mouse
            || event.response_type & 0x80 != 0
            || event.mode != NotifyMode::NORMAL
            || self.drag.is_some()
            || self.focus_cycle.is_some()
            || self.menu_session.is_some()
        {
            return Ok(());
        }
        let id = if self.clients.contains(client_id(event.event)) {
            Some(client_id(event.event))
        } else {
            self.frame_parts.get(&event.event).map(|part| match *part {
                FramePart::Container(id) | FramePart::Button(id, _) => id,
            })
        };
        let Some(id) = id else {
            return Ok(());
        };
        self.last_timestamp = event.time;
        self.focus(window_id(id), event.time)?;
        Ok(())
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

        let shaded = self.clients.get(id).is_some_and(|client| client.shaded);
        let accepts_direct_focus = if shaded {
            true
        } else {
            WmHints::get(&self.connection, window)?
                .reply()?
                .and_then(|hints| hints.input)
                .unwrap_or(true)
        };
        let supports_take_focus =
            !shaded && self.supports_protocol(window, self.atoms.WM_TAKE_FOCUS)?;
        let methods = focus_methods(accepts_direct_focus, supports_take_focus, timestamp);
        if !methods.direct && !methods.take_focus {
            debug!(
                window = format_args!("{window:#x}"),
                "client does not accept the available ICCCM focus methods"
            );
            return Ok(false);
        }
        self.clients.focus(id);
        self.sync_focused_state()?;
        self.sync_colormap_focus()?;
        self.clear_demands_attention(window)?;

        if methods.direct {
            self.connection.set_input_focus(
                InputFocus::PARENT,
                if shaded {
                    self.frame_window(id)
                } else {
                    window
                },
                timestamp,
            )?;
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
            self.refresh_frame_colors(client)?;
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
        } else {
            self.enforce_output_coverage_layers()?;
        }
        Ok(true)
    }

    fn focus_in(&mut self, event: &FocusInEvent) -> Result<(), X11Error> {
        if !focus_mode_changes_ownership(event.mode) {
            return Ok(());
        }
        let Some(id) = self.focus_client_for_window(event.event)? else {
            return Ok(());
        };
        self.observe_focus(id)
    }

    fn focus_out(&mut self, event: &FocusInEvent) -> Result<(), X11Error> {
        if !focus_mode_changes_ownership(event.mode) || event.detail == NotifyDetail::INFERIOR {
            return Ok(());
        }
        self.reconcile_server_focus()
    }

    fn reconcile_server_focus(&mut self) -> Result<(), X11Error> {
        let focus = self.connection.get_input_focus()?.reply()?.focus;
        if let Some(id) = self.focus_client_for_window(focus)? {
            self.observe_focus(id)
        } else {
            self.clear_observed_focus()
        }
    }

    fn focus_client_for_window(&self, window: Window) -> Result<Option<ClientId>, X11Error> {
        if window == NONE || window == u32::from(InputFocus::POINTER_ROOT) || window == self.root {
            return Ok(None);
        }
        let mut current = window;
        for _ in 0..64 {
            let id = client_id(current);
            if self.clients.contains(id) {
                return Ok(Some(id));
            }
            if let Some(part) = self.frame_parts.get(&current) {
                return Ok(Some(match *part {
                    FramePart::Container(id) | FramePart::Button(id, _) => id,
                }));
            }
            let reply = match self.connection.query_tree(current)?.reply() {
                Ok(reply) => reply,
                Err(error) => {
                    let error = X11Error::from(error);
                    if error.is_vanished_window() {
                        return Ok(None);
                    }
                    return Err(error);
                }
            };
            if reply.parent == self.root || reply.parent == NONE || reply.parent == current {
                return Ok(None);
            }
            current = reply.parent;
        }
        debug!(
            window = format_args!("{window:#x}"),
            "discarded cyclic or excessively deep focus ancestry"
        );
        Ok(None)
    }

    fn observe_focus(&mut self, requested: ClientId) -> Result<(), X11Error> {
        let Some(target) = self.clients.focus_target(requested) else {
            return self.clear_observed_focus();
        };
        if target != requested {
            let _ = self.focus(window_id(target), self.last_timestamp)?;
            return Ok(());
        }
        let previous = self.clients.focused();
        if !self.clients.focus(target) {
            return self.clear_observed_focus();
        }
        self.sync_focused_state()?;
        self.sync_colormap_focus()?;
        self.clear_demands_attention(window_id(target))?;
        if previous != Some(target) {
            if let Some(previous) = previous
                && self.clients.contains(previous)
            {
                self.refresh_frame_colors(previous)?;
                self.draw_title(previous)?;
            }
            self.refresh_frame_colors(target)?;
            self.draw_title(target)?;
        }
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_ACTIVE_WINDOW,
            AtomEnum::WINDOW,
            &[window_id(target)],
        )?;
        if previous != Some(target) {
            self.enforce_output_coverage_layers()?;
        }
        Ok(())
    }

    fn clear_observed_focus(&mut self) -> Result<(), X11Error> {
        let previous = self.clients.focused();
        self.clients.clear_focus();
        self.sync_focused_state()?;
        self.sync_colormap_focus()?;
        if let Some(previous) = previous
            && self.clients.contains(previous)
        {
            self.refresh_frame_colors(previous)?;
            self.draw_title(previous)?;
        }
        self.connection
            .delete_property(self.root, self.atoms._NET_ACTIVE_WINDOW)?;
        if previous.is_some() {
            self.enforce_output_coverage_layers()?;
        }
        Ok(())
    }

    fn clear_x_focus(&mut self, timestamp: u32) -> Result<(), X11Error> {
        let had_focus = self.clients.focused().is_some();
        self.clients.clear_focus();
        self.sync_focused_state()?;
        self.sync_colormap_focus()?;
        self.connection
            .set_input_focus(InputFocus::POINTER_ROOT, self.root, timestamp)?;
        self.connection
            .delete_property(self.root, self.atoms._NET_ACTIVE_WINDOW)?;
        if had_focus {
            self.enforce_output_coverage_layers()?;
        }
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
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_HIDDEN, true)?;
        self.sync_focused_state()?;
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
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_HIDDEN, false)?;
        if self.clients.is_visible(id) {
            if let Some(frame) = self.frames.get(&id).copied() {
                self.map_frame(window, frame)?;
            } else {
                self.connection.map_window(window)?;
            }
        }
        self.set_wm_state(window, WM_STATE_NORMAL)
    }

    fn set_shaded(&mut self, window: Window, shaded: bool) -> Result<(), X11Error> {
        let id = client_id(window);
        if self.clients.get(id).is_none_or(|client| {
            client.shaded == shaded || (shaded && !client.operations().shadeable)
        }) {
            return Ok(());
        }
        if self.drag.is_some_and(|drag| client_id(drag.window) == id) {
            self.finish_drag(self.last_timestamp)?;
        }
        let map_state = self
            .connection
            .get_window_attributes(window)?
            .reply()?
            .map_state;
        if !self.clients.set_shaded(id, shaded) {
            return Ok(());
        }
        if let Some(geometry) = self.clients.get(id).map(|client| client.geometry) {
            self.configure_decorated_client(id, geometry)?;
        }
        if shaded {
            if map_state != MapState::UNMAPPED {
                self.expected_unmaps
                    .entry(window)
                    .and_modify(|count| *count = count.saturating_add(2))
                    .or_insert(2);
                self.connection.unmap_window(window)?;
            }
            if self.clients.focused() == Some(id) {
                self.connection.set_input_focus(
                    InputFocus::PARENT,
                    self.frame_window(id),
                    self.last_timestamp,
                )?;
            }
        } else if self
            .clients
            .get(id)
            .is_some_and(|client| !client.iconic && self.clients.is_visible(id))
        {
            self.connection.map_window(window)?;
            if self.clients.focused() == Some(id) {
                self.focus(window, self.last_timestamp)?;
            }
        }
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_SHADED, shaded)?;
        self.publish_allowed_actions(id)
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

    fn set_showing_desktop(&mut self, showing: bool, timestamp: u32) -> Result<(), X11Error> {
        if self.clients.showing_desktop() == showing {
            return Ok(());
        }
        self.finish_drag(timestamp)?;
        self.hide_menu(timestamp)?;
        self.clients.set_showing_desktop(showing);
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.root,
            self.atoms._NET_SHOWING_DESKTOP,
            AtomEnum::CARDINAL,
            &[u32::from(showing)],
        )?;
        self.sync_workspace_visibility()?;
        self.restore_workspace_focus(timestamp)?;
        info!(showing, "changed desktop visibility mode");
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
        for id in self.clients.policy_stacking(&self.outputs) {
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

    fn lower_within_layer(&mut self, id: ClientId) -> Result<(), X11Error> {
        if !self.clients.lower(id) {
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
        match &event {
            Event::ButtonPress(event) if event.response_type & 0x80 == 0 => {
                self.record_user_time(event.time);
            }
            Event::KeyPress(event) if event.response_type & 0x80 == 0 => {
                self.record_user_time(event.time);
            }
            _ => {}
        }
        match event {
            Event::MapRequest(event) => self.manage(event.window, true)?,
            Event::ConfigureRequest(event) => self.configure_request(&event)?,
            Event::DestroyNotify(event) => self.unmanage(event.window, false)?,
            Event::UnmapNotify(event) => self.unmap_notify(&event)?,
            Event::FocusIn(event) => self.focus_in(&event)?,
            Event::FocusOut(event) => self.focus_out(&event)?,
            Event::EnterNotify(event) => self.enter_notify(&event)?,
            Event::ButtonPress(event) if self.menu_session.is_some() => {
                self.menu_button_press(&event)?;
            }
            Event::ButtonPress(event) => self.button_press(&event)?,
            Event::KeyPress(event) if self.menu_session.is_some() => {
                self.menu_key_press(&event)?;
            }
            Event::KeyPress(event) => self.key_press(&event)?,
            Event::MotionNotify(event) if self.menu_session.is_some() => {
                self.menu_pointer_motion(event.root_x, event.root_y)?;
            }
            Event::MotionNotify(event) => self.button_motion(&event)?,
            Event::ButtonRelease(event) if self.menu_session.is_some() => {
                self.menu_button_release(&event)?;
            }
            Event::ButtonRelease(event) => self.button_release(&event)?,
            Event::KeyRelease(event) if self.menu_session.is_some() => {
                self.menu_key_release(&event)?;
            }
            Event::KeyRelease(event) => self.key_release(&event)?,
            Event::Expose(event) => {
                if let Some(FramePart::Container(id)) = self.frame_parts.get(&event.window).copied()
                {
                    self.draw_title(id)?;
                } else if event.window == self.focus_overlay.window {
                    self.draw_focus_overlay()?;
                } else if event.window == self.menu_overlay.window {
                    self.draw_menu_overlay()?;
                }
            }
            Event::SelectionClear(event) if event.selection == self.wm_selection => {
                warn!("lost the ICCCM window-manager selection");
                self.running = false;
            }
            Event::SelectionRequest(event) if event.selection == self.wm_selection => {
                self.wm_selection_request(&event)?;
            }
            Event::ShapeNotify(event)
                if self.shape_version.is_some()
                    && self.clients.contains(client_id(event.affected_window)) =>
            {
                self.last_timestamp = event.server_time;
                self.refresh_client_shape(event.affected_window, event.shape_kind, event.shaped)?;
            }
            Event::SyncAlarmNotify(event) if self.sync_version.is_some() => {
                self.sync_alarm_notify(&event)?;
            }
            Event::ColormapNotify(event) => self.colormap_notify(&event)?,
            Event::PropertyNotify(event)
                if event.window == self.root && event.atom == self.atoms._NET_DESKTOP_LAYOUT =>
            {
                self.refresh_workspace_layout()?;
            }
            Event::PropertyNotify(event)
                if event.atom == self.atoms.WM_COLORMAP_WINDOWS
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.refresh_client_colormaps(event.window)?;
            }
            Event::PropertyNotify(event)
                if (event.atom == self.atoms._NET_WM_SYNC_REQUEST_COUNTER
                    || event.atom == self.atoms.WM_PROTOCOLS)
                    && self.clients.contains(client_id(event.window)) =>
            {
                if self
                    .drag
                    .is_some_and(|drag| drag.window == event.window && drag.sync.is_some())
                {
                    self.finish_drag(event.time)?;
                }
                self.refresh_sync_counter(event.window)?;
            }
            Event::PropertyNotify(event)
                if event.atom == self.atoms._NET_WM_USER_TIME_WINDOW
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.refresh_user_time_window(event.window)?;
            }
            Event::PropertyNotify(event) if event.atom == self.atoms._NET_WM_USER_TIME => {
                let owner = if self.clients.contains(client_id(event.window)) {
                    Some(client_id(event.window))
                } else {
                    self.user_time_windows.get(&event.window).copied()
                };
                if owner.is_some()
                    && owner == self.clients.focused()
                    && let Some(timestamp) =
                        self.read_cardinal_property(event.window, self.atoms._NET_WM_USER_TIME)?
                    && !x11_time_after(timestamp, event.time)
                {
                    self.record_user_time(timestamp);
                }
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
                self.refresh_client_policy(event.window)?;
            }
            Event::PropertyNotify(event)
                if event.atom == self.atoms._NET_WM_ALLOWED_ACTIONS
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.publish_allowed_actions(client_id(event.window))?;
            }
            Event::PropertyNotify(event)
                if (event.atom == self.atoms._NET_WM_NAME
                    || event.atom == u32::from(AtomEnum::WM_NAME))
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.refresh_title(event.window)?;
            }
            Event::PropertyNotify(event)
                if event.atom == self.atoms._NET_WM_ICON
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.refresh_client_icon(event.window)?;
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
                if event.atom == u32::from(AtomEnum::WM_HINTS)
                    || event.atom == self.atoms._NET_WM_STATE
                {
                    self.refresh_client_presentation(event.window)?;
                }
                if event.atom == self.atoms._NET_WM_STATE {
                    self.sync_wm_owned_states(event.window)?;
                }
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
            Event::RandrNotify(_) | Event::RandrScreenChangeNotify(_)
                if self.randr_version.is_some() =>
            {
                self.refresh_outputs()?;
            }
            Event::ClientMessage(event) if event.type_ == self.atoms.WM_PROTOCOLS => {
                self.client_pong(&event)?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_WM_FULLSCREEN_MONITORS
                    && event.format == 32
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.net_wm_fullscreen_monitors(&event)?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_CLOSE_WINDOW
                    && event.format == 32
                    && self.clients.contains(client_id(event.window)) =>
            {
                let requested = event.data.as_data32()[0];
                let timestamp = if requested == CURRENT_TIME {
                    self.last_timestamp
                } else {
                    self.last_timestamp = requested;
                    requested
                };
                self.close_client(client_id(event.window), timestamp)?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_WM_MOVERESIZE && event.format == 32 =>
            {
                self.net_wm_moveresize(&event)?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_MOVERESIZE_WINDOW
                    && event.format == 32
                    && self.clients.contains(client_id(event.window)) =>
            {
                self.net_moveresize_window(&event)?;
            }
            Event::ClientMessage(event)
                if event.type_ == self.atoms._NET_ACTIVE_WINDOW
                    && event.format == 32
                    && self.clients.contains(client_id(event.window)) =>
            {
                let data = event.data.as_data32();
                let id = client_id(event.window);
                let requested_timestamp = data[1];
                if !self.focus_request_allowed(id, Some(requested_timestamp), data[0] == 2, false) {
                    debug!(
                        window = format_args!("{:#x}", event.window),
                        source = data[0],
                        requested_timestamp,
                        last_user_time = self.last_user_time,
                        "prevented application activation from stealing focus"
                    );
                    self.demand_attention(id)?;
                    return Ok(());
                }
                let timestamp = if requested_timestamp == CURRENT_TIME {
                    self.last_timestamp
                } else {
                    if data[0] == 2 {
                        self.record_user_time(requested_timestamp);
                    }
                    requested_timestamp
                };
                self.set_showing_desktop(false, timestamp)?;
                if let Some(WorkspaceAssignment::Workspace(workspace)) =
                    self.clients.get(id).map(|client| client.workspace)
                    && workspace != self.clients.current_workspace()
                {
                    self.switch_workspace(workspace, timestamp)?;
                }
                self.restore(event.window)?;
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
                if event.type_ == self.atoms._NET_SHOWING_DESKTOP && event.format == 32 =>
            {
                if let Some(showing) = showing_desktop_request(event.data.as_data32()[0]) {
                    self.set_showing_desktop(showing, self.last_timestamp)?;
                }
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
        if self.drag.is_some() {
            if self.escape_keycodes.contains(&event.detail) {
                return self.cancel_drag(event.time);
            }
            if self.menu_keycodes.enter.contains(&event.detail) {
                return self.finish_drag(event.time);
            }
            if let Some(direction) = self.keyboard_drag_direction(event.detail) {
                self.keyboard_drag(direction, u16::from(event.state), event.time)?;
            }
            return Ok(());
        }
        if self.focus_cycle.is_some() && self.escape_keycodes.contains(&event.detail) {
            self.cancel_focus_cycle(event.time)?;
            return Ok(());
        }
        let modifiers = u16::from(event.state) & 0xff & !self.ignored_modifiers;
        let input = (event.detail, modifiers);
        if self.key_chain.is_some() && self.chain_quit_bindings.contains(&input) {
            self.finish_key_chain()?;
            return Ok(());
        }
        let Some(node) = self.current_key_node().children.get(&input) else {
            return Ok(());
        };
        let actions = node.actions.clone();
        if !node.children.is_empty() {
            self.advance_key_chain(input)?;
            return Ok(());
        }
        self.finish_key_chain()?;
        for action in actions {
            self.run_action(action, None, modifiers, event.time, None)?;
        }
        Ok(())
    }

    fn run_action(
        &mut self,
        action: Action,
        target: Option<ClientId>,
        modifiers: u16,
        timestamp: u32,
        pointer: Option<PointerInvocation>,
    ) -> Result<(), X11Error> {
        if !matches!(&action, Action::NextWindow | Action::PreviousWindow) {
            self.finish_focus_cycle(timestamp)?;
        }
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
                    Ok(child) => info!(pid = child.id(), %command, "started binding command"),
                    Err(error) => warn!(%error, %command, "could not start binding command"),
                }
            }
            Action::ShowMenu { menu } => {
                self.show_menu(&menu, target, pointer, timestamp)?;
            }
            Action::Reconfigure => {
                self.request_reconfigure()?;
            }
            Action::Close => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.close_client(target, timestamp)?;
                }
            }
            Action::Focus => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.focus(window_id(target), timestamp)?;
                }
            }
            Action::Raise => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.raise_within_layer(target)?;
                }
            }
            Action::Lower => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.lower_within_layer(target)?;
                }
            }
            Action::Minimize => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.iconify(window_id(target))?;
                }
            }
            Action::ToggleMaximize => {
                if let Some(target) = target.or_else(|| self.clients.focused()) {
                    self.toggle_full_maximize(target)?;
                }
            }
            Action::ToggleFullscreen => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(client) = self.clients.get(target).copied()
                    && client.operations().fullscreenable
                {
                    self.set_fullscreen(window_id(target), client.fullscreen.is_none())?;
                }
            }
            Action::ToggleAlwaysOnTop => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(client) = self.clients.get(target).copied()
                    && client.operations().above
                {
                    let layer = if client.layer == ClientLayer::Above {
                        ClientLayer::Normal
                    } else {
                        ClientLayer::Above
                    };
                    self.set_client_layer(window_id(target), layer)?;
                }
            }
            Action::ToggleAlwaysOnBottom => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(client) = self.clients.get(target).copied()
                    && client.operations().below
                {
                    let layer = if client.layer == ClientLayer::Below {
                        ClientLayer::Normal
                    } else {
                        ClientLayer::Below
                    };
                    self.set_client_layer(window_id(target), layer)?;
                }
            }
            Action::ToggleDecorations => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && self
                        .clients
                        .get(target)
                        .is_some_and(|client| client.operations().decoratable)
                {
                    if self.clients.get(target).is_some_and(|client| client.shaded) {
                        self.set_shaded(window_id(target), false)?;
                    }
                    if let Some(policy) = self.clients.toggle_decorations(target) {
                        self.apply_frame_policy(target, policy)?;
                        self.publish_allowed_actions(target)?;
                    }
                }
            }
            Action::ToggleSticky => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(client) = self.clients.get(target)
                {
                    let assignment = if client.workspace == WorkspaceAssignment::All {
                        WorkspaceAssignment::Workspace(self.clients.current_workspace())
                    } else {
                        WorkspaceAssignment::All
                    };
                    self.move_to_workspace(target, assignment, timestamp, false)?;
                }
            }
            Action::ToggleShade => {
                if let Some(target) = target.or_else(|| self.clients.focused())
                    && let Some(client) = self.clients.get(target)
                {
                    self.set_shaded(window_id(target), !client.shaded)?;
                }
            }
            Action::ToggleShowDesktop => {
                self.set_showing_desktop(!self.clients.showing_desktop(), timestamp)?;
            }
            Action::Move => {
                if let (Some(target), Some(pointer)) =
                    (target.or_else(|| self.clients.focused()), pointer)
                {
                    self.start_drag(target, DragKind::Move, pointer, timestamp)?;
                }
            }
            Action::Resize => {
                if let (Some(target), Some(pointer)) =
                    (target.or_else(|| self.clients.focused()), pointer)
                {
                    let edges = self.resize_edges(target, pointer);
                    self.start_drag(target, DragKind::Resize(edges), pointer, timestamp)?;
                }
            }
            Action::NextWindow => {
                self.cycle_focus(FocusCycleDirection::Next, modifiers, timestamp)?;
            }
            Action::PreviousWindow => {
                self.cycle_focus(FocusCycleDirection::Previous, modifiers, timestamp)?;
            }
            Action::PreviousWorkspace => {
                let workspace = self
                    .clients
                    .workspace_in_direction(WorkspaceDirection::Previous);
                self.switch_workspace(workspace, timestamp)?;
            }
            Action::NextWorkspace => {
                let workspace = self
                    .clients
                    .workspace_in_direction(WorkspaceDirection::Next);
                self.switch_workspace(workspace, timestamp)?;
            }
            Action::WorkspaceLeft => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Left)?;
                self.switch_workspace(workspace, timestamp)?;
            }
            Action::WorkspaceRight => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Right)?;
                self.switch_workspace(workspace, timestamp)?;
            }
            Action::WorkspaceUp => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Up)?;
                self.switch_workspace(workspace, timestamp)?;
            }
            Action::WorkspaceDown => {
                let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Down)?;
                self.switch_workspace(workspace, timestamp)?;
            }
            Action::SwitchWorkspace { workspace } => {
                self.switch_workspace(WorkspaceId::new(workspace - 1), timestamp)?;
            }
            Action::MoveToWorkspace { workspace, follow } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(WorkspaceId::new(workspace - 1)),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::MoveToPreviousWorkspace { follow } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    let workspace = self
                        .clients
                        .workspace_in_direction(WorkspaceDirection::Previous);
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::MoveToNextWorkspace { follow } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    let workspace = self
                        .clients
                        .workspace_in_direction(WorkspaceDirection::Next);
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceLeft { follow } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Left)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceRight { follow } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Right)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceUp { follow } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Up)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::MoveToWorkspaceDown { follow } => {
                if let Some(focused) = target.or_else(|| self.clients.focused()) {
                    let workspace = self.workspace_in_grid_direction(WorkspaceDirection::Down)?;
                    self.move_to_workspace(
                        focused,
                        WorkspaceAssignment::Workspace(workspace),
                        timestamp,
                        follow,
                    )?;
                }
            }
            Action::Exit => {
                self.finish_drag(timestamp)?;
                self.running = false;
            }
        }
        Ok(())
    }

    fn request_reconfigure(&self) -> Result<(), X11Error> {
        let message = ClientMessageEvent::new(
            32,
            self.support_window,
            self.atoms._NOBOX_CONTROL,
            [CONTROL_RELOAD, 0, 0, 0, 0],
        );
        self.connection
            .send_event(false, self.support_window, EventMask::NO_EVENT, message)?
            .check()?;
        self.connection.flush()?;
        Ok(())
    }

    fn key_release(&mut self, event: &KeyReleaseEvent) -> Result<(), X11Error> {
        let Some(cycle) = self.focus_cycle.as_ref() else {
            return Ok(());
        };
        debug!(
            keycode = event.detail,
            state = u16::from(event.state),
            modifier_mask = self.modifier_keycodes.get(&event.detail).copied(),
            "observed key release during focus cycle"
        );
        if self
            .modifier_keycodes
            .get(&event.detail)
            .is_some_and(|mask| *mask & cycle.modifiers != 0)
        {
            debug!(keycode = event.detail, "finished modifier-held focus cycle");
            self.finish_focus_cycle(event.time)?;
        }
        Ok(())
    }

    fn finish_focus_cycle(&mut self, timestamp: u32) -> Result<(), X11Error> {
        let keyboard_grabbed = self
            .focus_cycle
            .take()
            .is_some_and(|cycle| cycle.keyboard_grabbed);
        self.hide_focus_overlay()?;
        if keyboard_grabbed {
            self.connection.ungrab_keyboard(timestamp)?;
        }
        Ok(())
    }

    fn cancel_focus_cycle(&mut self, timestamp: u32) -> Result<(), X11Error> {
        let original = self.focus_cycle.as_ref().and_then(|cycle| cycle.original);
        self.finish_focus_cycle(timestamp)?;
        if let Some(original) = original {
            self.focus(window_id(original), timestamp)?;
        }
        Ok(())
    }

    fn update_focus_overlay(&mut self) -> Result<(), X11Error> {
        if !self.config.switcher.enabled {
            return self.hide_focus_overlay();
        }
        let Some(cycle) = self.focus_cycle.as_ref() else {
            return self.hide_focus_overlay();
        };
        let Some(index) = cycle.index else {
            return self.hide_focus_overlay();
        };
        let Some(selected) = cycle.candidates.get(index).copied() else {
            return self.hide_focus_overlay();
        };
        if !cycle.keyboard_grabbed {
            return self.hide_focus_overlay();
        }
        let output = self.clients.get(selected).map_or_else(
            || self.outputs.primary(),
            |client| self.outputs.output_for(client.geometry),
        );
        let available_height = output.geometry.height.saturating_sub(40).max(1);
        let fitting_rows = (available_height / self.config.switcher.row_height).max(1);
        let rows = cycle.candidates.len().min(
            usize::try_from(self.config.switcher.max_rows.min(fitting_rows)).unwrap_or(usize::MAX),
        );
        let width = self
            .config
            .switcher
            .width
            .min(output.geometry.width.saturating_sub(40).max(1));
        let height = self
            .config
            .switcher
            .row_height
            .saturating_mul(u32::try_from(rows).unwrap_or(u32::MAX))
            .min(available_height)
            .max(1);
        let x = centered_axis(output.geometry.x, output.geometry.width, width);
        let y = centered_axis(output.geometry.y, output.geometry.height, height);
        self.connection.configure_window(
            self.focus_overlay.window,
            &ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .width(width)
                .height(height)
                .stack_mode(StackMode::ABOVE),
        )?;
        self.connection.change_window_attributes(
            self.focus_overlay.window,
            &ChangeWindowAttributesAux::new()
                .background_pixel(self.decoration_pixels.inactive_titlebar)
                .border_pixel(self.decoration_pixels.active_border),
        )?;
        self.focus_overlay.width = width;
        self.focus_overlay.height = height;
        let start = focus_cycle_visible_start(cycle.candidates.len(), index, rows);
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.focus_overlay.window,
            self.atoms._NOBOX_FOCUS_SWITCHER,
            AtomEnum::CARDINAL,
            &[
                window_id(selected),
                u32::try_from(index).unwrap_or(u32::MAX),
                u32::try_from(cycle.candidates.len()).unwrap_or(u32::MAX),
                u32::try_from(start).unwrap_or(u32::MAX),
            ],
        )?;
        if !self.focus_overlay.mapped {
            self.connection.map_window(self.focus_overlay.window)?;
            self.focus_overlay.mapped = true;
        }
        self.draw_focus_overlay()
    }

    fn draw_focus_overlay(&self) -> Result<(), X11Error> {
        if !self.focus_overlay.mapped {
            return Ok(());
        }
        let Some(cycle) = self.focus_cycle.as_ref() else {
            return Ok(());
        };
        let Some(selected) = cycle.index else {
            return Ok(());
        };
        let fitting_rows = (self.focus_overlay.height / self.config.switcher.row_height).max(1);
        let rows = cycle.candidates.len().min(
            usize::try_from(self.config.switcher.max_rows.min(fitting_rows)).unwrap_or(usize::MAX),
        );
        let start = focus_cycle_visible_start(cycle.candidates.len(), selected, rows);
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new().foreground(self.decoration_pixels.inactive_titlebar),
        )?;
        self.connection.poly_fill_rectangle(
            self.focus_overlay.window,
            self.title_gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: x_dimension(self.focus_overlay.width),
                height: x_dimension(self.focus_overlay.height),
            }],
        )?;
        let row_height = self.config.switcher.row_height;
        for (row, candidate) in cycle.candidates[start..start + rows].iter().enumerate() {
            let candidate_index = start + row;
            let background = if candidate_index == selected {
                self.decoration_pixels.active_titlebar
            } else {
                self.decoration_pixels.inactive_titlebar
            };
            let row_y = row_height.saturating_mul(u32::try_from(row).unwrap_or(u32::MAX));
            if candidate_index == selected {
                self.connection
                    .change_gc(self.title_gc, &ChangeGCAux::new().foreground(background))?;
                self.connection.poly_fill_rectangle(
                    self.focus_overlay.window,
                    self.title_gc,
                    &[Rectangle {
                        x: 0,
                        y: clamp_i16_u32(row_y),
                        width: x_dimension(self.focus_overlay.width),
                        height: x_dimension(row_height),
                    }],
                )?;
            }
            let title = self.titles.get(candidate).map_or("(untitled)", |title| {
                if title.is_empty() {
                    "(untitled)"
                } else {
                    title
                }
            });
            let limit = usize::try_from(self.focus_overlay.width.saturating_sub(24) / 8)
                .unwrap_or(usize::MAX)
                .min(255);
            let text = title_text_bytes(title, limit);
            self.connection.change_gc(
                self.title_gc,
                &ChangeGCAux::new()
                    .foreground(self.decoration_pixels.title_text)
                    .background(background),
            )?;
            self.connection.image_text8(
                self.focus_overlay.window,
                self.title_gc,
                12,
                clamp_i16_u32(row_y.saturating_add(row_height / 2).saturating_add(5)),
                &text,
            )?;
        }
        Ok(())
    }

    fn hide_focus_overlay(&mut self) -> Result<(), X11Error> {
        if self.focus_overlay.mapped {
            self.connection.unmap_window(self.focus_overlay.window)?;
            self.focus_overlay.mapped = false;
        }
        self.connection
            .delete_property(self.focus_overlay.window, self.atoms._NOBOX_FOCUS_SWITCHER)?;
        Ok(())
    }

    fn show_menu(
        &mut self,
        menu: &str,
        target: Option<ClientId>,
        pointer: Option<PointerInvocation>,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        self.hide_menu(timestamp)?;
        let target = target.or_else(|| self.clients.focused());
        let Some(runtime_menu) = self.resolve_menu(menu, target) else {
            warn!(menu, "ignored unknown menu action");
            return Ok(());
        };
        let Some(selected) = first_selectable_menu_entry(&runtime_menu.entries) else {
            return Ok(());
        };
        let (anchor_x, anchor_y, centered) = if let Some(pointer) = pointer {
            (i32::from(pointer.root_x), i32::from(pointer.root_y), false)
        } else {
            let output = target
                .or_else(|| self.clients.focused())
                .and_then(|id| self.clients.get(id))
                .map_or_else(
                    || self.outputs.primary(),
                    |client| self.outputs.output_for(client.geometry),
                );
            (
                centered_axis(output.geometry.x, output.geometry.width, 1),
                centered_axis(output.geometry.y, output.geometry.height, 1),
                true,
            )
        };
        let pointer_status = self
            .connection
            .grab_pointer(
                false,
                self.root,
                EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                NONE,
                NONE,
                timestamp,
            )?
            .reply()?
            .status;
        if pointer_status != GrabStatus::SUCCESS {
            warn!(
                ?pointer_status,
                menu, "could not retain pointer grab for menu"
            );
            return Ok(());
        }
        let keyboard_status = self
            .connection
            .grab_keyboard(
                false,
                self.root,
                timestamp,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )?
            .reply()?
            .status;
        if keyboard_status != GrabStatus::SUCCESS {
            self.connection.ungrab_pointer(timestamp)?;
            warn!(
                ?keyboard_status,
                menu, "could not retain keyboard grab for menu"
            );
            return Ok(());
        }
        self.mouse_gesture = None;
        self.last_mouse_click = None;
        self.menu_session = Some(MenuSession {
            menu: runtime_menu,
            parents: Vec::new(),
            selected,
            target,
            anchor_x,
            anchor_y,
            centered,
            opening_button: pointer.map(|pointer| pointer.button),
            pending_key: None,
            keyboard_grabbed: true,
            pointer_grabbed: true,
        });
        self.update_menu_overlay()
    }

    fn menu_definition(&self, id: &str) -> Option<&MenuDefinition> {
        self.config
            .menu
            .definitions
            .iter()
            .find(|definition| definition.id == id)
    }

    fn resolve_menu(&self, id: &str, target: Option<ClientId>) -> Option<RuntimeMenu> {
        let definition = self.menu_definition(id)?;
        match definition.source {
            MenuSource::Static => Some(RuntimeMenu {
                id: definition.id.clone(),
                title: definition.title.clone(),
                entries: definition
                    .entries
                    .iter()
                    .map(runtime_configured_entry)
                    .collect(),
            }),
            MenuSource::Client => self.resolve_client_menu(definition, target?),
            MenuSource::ClientWorkspaces => {
                self.resolve_client_workspaces_menu(definition, target?)
            }
            MenuSource::Windows => self.resolve_windows_menu(definition),
        }
    }

    fn resolve_client_menu(
        &self,
        definition: &MenuDefinition,
        target: ClientId,
    ) -> Option<RuntimeMenu> {
        let client = self.clients.get(target).copied()?;
        let operations = client.operations();
        let mut entries = Vec::with_capacity(14);
        if operations.workspace_movable {
            entries.push(runtime_submenu("_Send to workspace", "client-workspaces"));
        }
        if operations.minimizable {
            entries.push(runtime_action("Mi_nimize", Action::Minimize, target));
        }
        if operations.maximizable {
            entries.push(runtime_action(
                if client.maximize.is_some() {
                    "Unma_ximize"
                } else {
                    "Ma_ximize"
                },
                Action::ToggleMaximize,
                target,
            ));
        }
        if operations.shadeable {
            entries.push(runtime_action(
                if client.shaded { "Uns_hade" } else { "S_hade" },
                Action::ToggleShade,
                target,
            ));
        }
        if operations.fullscreenable {
            entries.push(runtime_action(
                if client.fullscreen.is_some() {
                    "Leave _fullscreen"
                } else {
                    "_Fullscreen"
                },
                Action::ToggleFullscreen,
                target,
            ));
        }
        if operations.above {
            entries.push(runtime_action(
                if client.layer == ClientLayer::Above {
                    "_Normal layer"
                } else {
                    "Always on _top"
                },
                Action::ToggleAlwaysOnTop,
                target,
            ));
        }
        if operations.below {
            entries.push(runtime_action(
                if client.layer == ClientLayer::Below {
                    "_Normal layer"
                } else {
                    "Always on _bottom"
                },
                Action::ToggleAlwaysOnBottom,
                target,
            ));
        }
        if operations.decoratable {
            entries.push(runtime_action(
                if client.policy.decorations.is_present() {
                    "Un_decorate"
                } else {
                    "_Decorate"
                },
                Action::ToggleDecorations,
                target,
            ));
        }
        entries.push(RuntimeMenuEntry::Separator { label: None });
        entries.push(runtime_action("_Raise", Action::Raise, target));
        entries.push(runtime_action("_Lower", Action::Lower, target));
        if operations.closable {
            entries.push(RuntimeMenuEntry::Separator { label: None });
            entries.push(runtime_action("_Close", Action::Close, target));
        }
        Some(RuntimeMenu {
            id: definition.id.clone(),
            title: self
                .titles
                .get(&target)
                .filter(|title| !title.is_empty())
                .cloned()
                .unwrap_or_else(|| definition.title.clone()),
            entries,
        })
    }

    fn resolve_client_workspaces_menu(
        &self,
        definition: &MenuDefinition,
        target: ClientId,
    ) -> Option<RuntimeMenu> {
        self.clients.get(target)?;
        let mut entries = Vec::with_capacity(self.config.workspaces.names.len().saturating_add(1));
        for (index, name) in self.config.workspaces.names.iter().enumerate() {
            let workspace = u32::try_from(index).ok()?.checked_add(1)?;
            entries.push(runtime_action(
                &workspace_menu_label(workspace, name),
                Action::MoveToWorkspace {
                    workspace,
                    follow: false,
                },
                target,
            ));
        }
        entries.push(RuntimeMenuEntry::Separator { label: None });
        entries.push(runtime_action(
            "_All workspaces",
            Action::ToggleSticky,
            target,
        ));
        Some(RuntimeMenu {
            id: definition.id.clone(),
            title: definition.title.clone(),
            entries,
        })
    }

    fn resolve_windows_menu(&self, definition: &MenuDefinition) -> Option<RuntimeMenu> {
        const MAX_DYNAMIC_ENTRIES: usize = 512;
        let clients = self.clients.management_order().collect::<Vec<_>>();
        let mut entries = Vec::with_capacity(
            clients
                .len()
                .saturating_add(self.config.workspaces.names.len())
                .min(MAX_DYNAMIC_ENTRIES),
        );
        for (workspace_index, workspace_name) in self.config.workspaces.names.iter().enumerate() {
            let workspace = WorkspaceId::new(u32::try_from(workspace_index).ok()?);
            let mut heading_added = false;
            for id in clients.iter().copied().filter(|id| {
                self.clients.get(*id).is_some_and(|client| {
                    !client.presentation.skip_taskbar
                        && client.workspace == WorkspaceAssignment::Workspace(workspace)
                })
            }) {
                let required = if heading_added { 1 } else { 2 };
                if entries.len().saturating_add(required) > MAX_DYNAMIC_ENTRIES {
                    break;
                }
                if !heading_added {
                    entries.push(RuntimeMenuEntry::Separator {
                        label: Some(workspace_name.clone()),
                    });
                    heading_added = true;
                }
                entries.push(runtime_client_activation(self.client_menu_title(id), id));
            }
        }
        let sticky = clients.into_iter().filter(|id| {
            self.clients.get(*id).is_some_and(|client| {
                !client.presentation.skip_taskbar && client.workspace == WorkspaceAssignment::All
            })
        });
        let mut sticky_heading = false;
        for id in sticky {
            let required = if sticky_heading { 1 } else { 2 };
            if entries.len().saturating_add(required) > MAX_DYNAMIC_ENTRIES {
                break;
            }
            if !sticky_heading {
                entries.push(RuntimeMenuEntry::Separator {
                    label: Some("All workspaces".to_owned()),
                });
                sticky_heading = true;
            }
            entries.push(runtime_client_activation(self.client_menu_title(id), id));
        }
        (!entries.is_empty()).then(|| RuntimeMenu {
            id: definition.id.clone(),
            title: definition.title.clone(),
            entries,
        })
    }

    fn client_menu_title(&self, id: ClientId) -> String {
        self.titles
            .get(&id)
            .filter(|title| !title.is_empty())
            .cloned()
            .unwrap_or_else(|| "(untitled)".to_owned())
    }

    fn update_menu_overlay(&mut self) -> Result<(), X11Error> {
        let Some(session) = self.menu_session.as_ref() else {
            return self.hide_menu(CURRENT_TIME);
        };
        let active = session.menu.id.clone();
        let selected = session.selected;
        let anchor_x = session.anchor_x;
        let anchor_y = session.anchor_y;
        let centered = session.centered;
        let entry_count = session.menu.entries.len();
        let output = self
            .outputs
            .output_for(Geometry::new(anchor_x, anchor_y, 1, 1));
        let available_height = output.geometry.height.saturating_sub(20).max(1);
        let fitting_rows = (available_height / self.config.menu.row_height)
            .saturating_sub(1)
            .max(1);
        let rows = entry_count.min(
            usize::try_from(self.config.menu.max_rows.min(fitting_rows)).unwrap_or(usize::MAX),
        );
        let width = self
            .config
            .menu
            .width
            .min(output.geometry.width.saturating_sub(20).max(1));
        let height = self
            .config
            .menu
            .row_height
            .saturating_mul(u32::try_from(rows.saturating_add(1)).unwrap_or(u32::MAX))
            .min(available_height)
            .max(1);
        let (x, y) = if centered {
            (
                centered_axis(output.geometry.x, output.geometry.width, width),
                centered_axis(output.geometry.y, output.geometry.height, height),
            )
        } else {
            (
                place_popup_axis(anchor_x, output.geometry.x, output.geometry.width, width),
                place_popup_axis(anchor_y, output.geometry.y, output.geometry.height, height),
            )
        };
        self.connection.configure_window(
            self.menu_overlay.window,
            &ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .width(width)
                .height(height)
                .stack_mode(StackMode::ABOVE),
        )?;
        self.menu_overlay.x = x;
        self.menu_overlay.y = y;
        self.menu_overlay.width = width;
        self.menu_overlay.height = height;
        let start = focus_cycle_visible_start(entry_count, selected, rows);
        self.connection.change_property8(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.menu_overlay.window,
            self.atoms._NOBOX_MENU,
            self.atoms.UTF8_STRING,
            active.as_bytes(),
        )?;
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            self.menu_overlay.window,
            self.atoms._NOBOX_MENU_SELECTION,
            AtomEnum::CARDINAL,
            &[
                u32::try_from(selected).unwrap_or(u32::MAX),
                u32::try_from(entry_count).unwrap_or(u32::MAX),
                u32::try_from(start).unwrap_or(u32::MAX),
            ],
        )?;
        if !self.menu_overlay.mapped {
            self.connection.map_window(self.menu_overlay.window)?;
            self.menu_overlay.mapped = true;
        }
        self.draw_menu_overlay()
    }

    fn draw_menu_overlay(&self) -> Result<(), X11Error> {
        if !self.menu_overlay.mapped {
            return Ok(());
        }
        let Some(session) = self.menu_session.as_ref() else {
            return Ok(());
        };
        let definition = &session.menu;
        let row_height = self.config.menu.row_height;
        let rows = definition.entries.len().min(
            usize::try_from(
                (self.menu_overlay.height / row_height)
                    .saturating_sub(1)
                    .min(self.config.menu.max_rows),
            )
            .unwrap_or(usize::MAX),
        );
        let start = focus_cycle_visible_start(definition.entries.len(), session.selected, rows);
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new().foreground(self.decoration_pixels.inactive_titlebar),
        )?;
        self.connection.poly_fill_rectangle(
            self.menu_overlay.window,
            self.title_gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: x_dimension(self.menu_overlay.width),
                height: x_dimension(self.menu_overlay.height),
            }],
        )?;
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new().foreground(self.decoration_pixels.active_titlebar),
        )?;
        self.connection.poly_fill_rectangle(
            self.menu_overlay.window,
            self.title_gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: x_dimension(self.menu_overlay.width),
                height: x_dimension(row_height),
            }],
        )?;
        self.draw_menu_text(
            &definition.title,
            12,
            0,
            self.decoration_pixels.active_titlebar,
        )?;

        for (row, entry) in definition.entries[start..start + rows].iter().enumerate() {
            let index = start + row;
            let y =
                row_height.saturating_mul(u32::try_from(row.saturating_add(1)).unwrap_or(u32::MAX));
            let selected = index == session.selected && menu_entry_is_selectable(entry);
            let background = if selected {
                self.decoration_pixels.active_titlebar
            } else {
                self.decoration_pixels.inactive_titlebar
            };
            if selected {
                self.connection
                    .change_gc(self.title_gc, &ChangeGCAux::new().foreground(background))?;
                self.connection.poly_fill_rectangle(
                    self.menu_overlay.window,
                    self.title_gc,
                    &[Rectangle {
                        x: 0,
                        y: clamp_i16_u32(y),
                        width: x_dimension(self.menu_overlay.width),
                        height: x_dimension(row_height),
                    }],
                )?;
            }
            match entry {
                RuntimeMenuEntry::Item { label, .. } => {
                    self.draw_menu_text(label, 12, y, background)?;
                }
                RuntimeMenuEntry::Submenu { label, .. } => {
                    let label = format!("{label}  >");
                    self.draw_menu_text(&label, 12, y, background)?;
                }
                RuntimeMenuEntry::Separator { label } => {
                    self.connection.change_gc(
                        self.title_gc,
                        &ChangeGCAux::new().foreground(self.decoration_pixels.inactive_border),
                    )?;
                    self.connection.poly_fill_rectangle(
                        self.menu_overlay.window,
                        self.title_gc,
                        &[Rectangle {
                            x: 8,
                            y: clamp_i16_u32(y.saturating_add(row_height / 2)),
                            width: x_dimension(self.menu_overlay.width.saturating_sub(16)),
                            height: 1,
                        }],
                    )?;
                    if let Some(label) = label {
                        self.draw_menu_text(label, 12, y, background)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn draw_menu_text(
        &self,
        text: &str,
        x: i16,
        row_y: u32,
        background: u32,
    ) -> Result<(), X11Error> {
        let limit = usize::try_from(self.menu_overlay.width.saturating_sub(24) / 8)
            .unwrap_or(usize::MAX)
            .min(255);
        self.connection.change_gc(
            self.title_gc,
            &ChangeGCAux::new()
                .foreground(self.decoration_pixels.title_text)
                .background(background),
        )?;
        self.connection.image_text8(
            self.menu_overlay.window,
            self.title_gc,
            x,
            clamp_i16_u32(
                row_y
                    .saturating_add(self.config.menu.row_height / 2)
                    .saturating_add(5),
            ),
            &title_text_bytes(text, limit),
        )?;
        Ok(())
    }

    fn menu_pointer_motion(&mut self, root_x: i16, root_y: i16) -> Result<(), X11Error> {
        let Some(index) = self.menu_entry_at(root_x, root_y) else {
            return Ok(());
        };
        let selectable = self
            .menu_session
            .as_ref()
            .and_then(|session| session.menu.entries.get(index))
            .is_some_and(menu_entry_is_selectable);
        if selectable
            && let Some(session) = &mut self.menu_session
            && session.selected != index
        {
            session.selected = index;
            session.pending_key = None;
            self.update_menu_overlay()?;
        }
        Ok(())
    }

    fn menu_button_press(&mut self, event: &ButtonPressEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        let Some(index) = self.menu_entry_at(event.root_x, event.root_y) else {
            return self.hide_menu(event.time);
        };
        if let Some(session) = &mut self.menu_session {
            session.opening_button = None;
            session.pending_key = None;
        }
        self.menu_pointer_motion(event.root_x, event.root_y)?;
        if matches!(event.detail, 4 | 5) {
            self.move_menu_selection(event.detail == 5)?;
        } else if self
            .menu_session
            .as_ref()
            .is_some_and(|session| session.selected != index)
        {
            self.draw_menu_overlay()?;
        }
        Ok(())
    }

    fn menu_button_release(&mut self, event: &ButtonReleaseEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        if self
            .menu_session
            .as_ref()
            .is_some_and(|session| session.opening_button == Some(event.detail))
        {
            if let Some(session) = &mut self.menu_session {
                session.opening_button = None;
            }
            let Some(index) = self.menu_entry_at(event.root_x, event.root_y) else {
                return Ok(());
            };
            return self.activate_menu_entry(
                index,
                mouse_modifier_mask(u16::from(event.state)),
                event.time,
                None,
            );
        }
        if matches!(event.detail, 4 | 5) {
            return Ok(());
        }
        let Some(index) = self.menu_entry_at(event.root_x, event.root_y) else {
            return self.hide_menu(event.time);
        };
        self.activate_menu_entry(
            index,
            mouse_modifier_mask(u16::from(event.state)),
            event.time,
            Some(PointerInvocation {
                target: MouseTarget {
                    window: self.root,
                    client: self
                        .menu_session
                        .as_ref()
                        .and_then(|session| session.target),
                    context: MouseContext::Root,
                },
                button: event.detail,
                root_x: event.root_x,
                root_y: event.root_y,
            }),
        )
    }

    fn menu_key_press(&mut self, event: &KeyPressEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        if self.escape_keycodes.contains(&event.detail) {
            return self.hide_menu(event.time);
        }
        if self.menu_keycodes.up.contains(&event.detail) {
            return self.move_menu_selection(false);
        }
        if self.menu_keycodes.down.contains(&event.detail) {
            return self.move_menu_selection(true);
        }
        if self.menu_keycodes.home.contains(&event.detail) {
            return self.select_menu_edge(false);
        }
        if self.menu_keycodes.end.contains(&event.detail) {
            return self.select_menu_edge(true);
        }
        if self.menu_keycodes.left.contains(&event.detail) {
            return self.leave_submenu();
        }
        if self.menu_keycodes.right.contains(&event.detail) {
            return self.enter_selected_submenu();
        }
        if self.menu_keycodes.enter.contains(&event.detail) {
            let (selected, entry) = match self.current_menu_entry() {
                Some((selected, entry)) => (selected, entry.clone()),
                None => return Ok(()),
            };
            match entry {
                RuntimeMenuEntry::Submenu { .. } => {
                    return self.activate_menu_entry(selected, 0, event.time, None);
                }
                RuntimeMenuEntry::Item { action, target, .. } => {
                    if let Some(session) = &mut self.menu_session {
                        session.pending_key = Some((event.detail, action, target));
                    }
                }
                RuntimeMenuEntry::Separator { .. } => {}
            }
            return Ok(());
        }
        if let Some(character) = self.menu_keycodes.characters.get(&event.detail).copied() {
            self.select_menu_accelerator(character, event)?;
        }
        Ok(())
    }

    fn menu_key_release(&mut self, event: &KeyReleaseEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        let pending = self
            .menu_session
            .as_mut()
            .and_then(|session| session.pending_key.take());
        let Some((keycode, action, target)) = pending else {
            return Ok(());
        };
        if keycode != event.detail {
            if let Some(session) = &mut self.menu_session {
                session.pending_key = Some((keycode, action, target));
            }
            return Ok(());
        }
        let target = target.or_else(|| {
            self.menu_session
                .as_ref()
                .and_then(|session| session.target)
        });
        self.hide_menu(event.time)?;
        self.execute_runtime_menu_action(action, target, 0, event.time, None)
    }

    fn select_menu_accelerator(
        &mut self,
        character: char,
        event: &KeyPressEvent,
    ) -> Result<(), X11Error> {
        let Some((selected, matches)) = self.menu_session.as_ref().and_then(|session| {
            accelerator_menu_entry(
                &session.menu.entries,
                session.selected,
                lowercase_character(character),
            )
        }) else {
            return Ok(());
        };
        if let Some(session) = &mut self.menu_session {
            session.selected = selected;
            session.pending_key = None;
        }
        self.update_menu_overlay()?;
        if matches != 1 {
            return Ok(());
        }
        let Some((_, entry)) = self
            .current_menu_entry()
            .map(|(index, entry)| (index, entry.clone()))
        else {
            return Ok(());
        };
        match entry {
            RuntimeMenuEntry::Submenu { menu, .. } => self.enter_submenu(selected, menu),
            RuntimeMenuEntry::Item { action, target, .. } => {
                if let Some(session) = &mut self.menu_session {
                    session.pending_key = Some((event.detail, action, target));
                }
                Ok(())
            }
            RuntimeMenuEntry::Separator { .. } => Ok(()),
        }
    }

    fn execute_runtime_menu_action(
        &mut self,
        action: RuntimeMenuAction,
        target: Option<ClientId>,
        modifiers: u16,
        timestamp: u32,
        pointer: Option<PointerInvocation>,
    ) -> Result<(), X11Error> {
        match action {
            RuntimeMenuAction::Configured(actions) => {
                for action in actions {
                    self.run_action(action, target, modifiers, timestamp, pointer)?;
                }
                Ok(())
            }
            RuntimeMenuAction::ActivateClient(id) => self.activate_client_from_menu(id, timestamp),
        }
    }

    fn activate_client_from_menu(&mut self, id: ClientId, timestamp: u32) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        self.set_showing_desktop(false, timestamp)?;
        if let WorkspaceAssignment::Workspace(workspace) = client.workspace
            && workspace != self.clients.current_workspace()
        {
            self.switch_workspace(workspace, timestamp)?;
        }
        if client.iconic {
            self.restore(window_id(id))?;
        }
        self.focus(window_id(id), timestamp)?;
        Ok(())
    }

    fn current_menu_entry(&self) -> Option<(usize, &RuntimeMenuEntry)> {
        let session = self.menu_session.as_ref()?;
        session
            .menu
            .entries
            .get(session.selected)
            .map(|entry| (session.selected, entry))
    }

    fn move_menu_selection(&mut self, forward: bool) -> Result<(), X11Error> {
        let next = {
            let Some(session) = self.menu_session.as_ref() else {
                return Ok(());
            };
            next_selectable_menu_entry(&session.menu.entries, session.selected, forward)
        };
        if let Some(next) = next
            && let Some(session) = &mut self.menu_session
        {
            session.selected = next;
            session.pending_key = None;
            self.update_menu_overlay()?;
        }
        Ok(())
    }

    fn select_menu_edge(&mut self, last: bool) -> Result<(), X11Error> {
        let selected = self.menu_session.as_ref().and_then(|session| {
            if last {
                last_selectable_menu_entry(&session.menu.entries)
            } else {
                first_selectable_menu_entry(&session.menu.entries)
            }
        });
        if let Some(selected) = selected
            && let Some(session) = &mut self.menu_session
        {
            session.selected = selected;
            session.pending_key = None;
            self.update_menu_overlay()?;
        }
        Ok(())
    }

    fn enter_selected_submenu(&mut self) -> Result<(), X11Error> {
        let Some((selected, RuntimeMenuEntry::Submenu { menu, .. })) = self.current_menu_entry()
        else {
            return Ok(());
        };
        let menu = menu.clone();
        self.enter_submenu(selected, menu)
    }

    fn enter_submenu(&mut self, selected: usize, menu: String) -> Result<(), X11Error> {
        let target = self
            .menu_session
            .as_ref()
            .and_then(|session| session.target);
        let Some(runtime_menu) = self.resolve_menu(&menu, target) else {
            return Ok(());
        };
        let Some(next) = first_selectable_menu_entry(&runtime_menu.entries) else {
            return Ok(());
        };
        if let Some(session) = &mut self.menu_session {
            let parent = std::mem::replace(&mut session.menu, runtime_menu);
            session.parents.push((parent, selected));
            session.selected = next;
            session.pending_key = None;
        }
        self.update_menu_overlay()
    }

    fn leave_submenu(&mut self) -> Result<(), X11Error> {
        let parent = self
            .menu_session
            .as_mut()
            .and_then(|session| session.parents.pop());
        if let Some((menu, selected)) = parent
            && let Some(session) = &mut self.menu_session
        {
            session.menu = menu;
            session.selected = selected;
            session.pending_key = None;
            self.update_menu_overlay()?;
        }
        Ok(())
    }

    fn activate_menu_entry(
        &mut self,
        index: usize,
        modifiers: u16,
        timestamp: u32,
        pointer: Option<PointerInvocation>,
    ) -> Result<(), X11Error> {
        let Some(entry) = self
            .menu_session
            .as_ref()
            .and_then(|session| session.menu.entries.get(index))
            .cloned()
        else {
            return Ok(());
        };
        match entry {
            RuntimeMenuEntry::Submenu { menu, .. } => self.enter_submenu(index, menu),
            RuntimeMenuEntry::Item { action, target, .. } => {
                let target = target.or_else(|| {
                    self.menu_session
                        .as_ref()
                        .and_then(|session| session.target)
                });
                self.hide_menu(timestamp)?;
                self.execute_runtime_menu_action(action, target, modifiers, timestamp, pointer)
            }
            RuntimeMenuEntry::Separator { .. } => Ok(()),
        }
    }

    fn menu_entry_at(&self, root_x: i16, root_y: i16) -> Option<usize> {
        let session = self.menu_session.as_ref()?;
        let definition = &session.menu;
        let x = i32::from(root_x).checked_sub(self.menu_overlay.x)?;
        let y = i32::from(root_y).checked_sub(self.menu_overlay.y)?;
        if x < 0
            || y < i32::try_from(self.config.menu.row_height).ok()?
            || u32::try_from(x).ok()? >= self.menu_overlay.width
            || u32::try_from(y).ok()? >= self.menu_overlay.height
        {
            return None;
        }
        let rows = definition.entries.len().min(
            usize::try_from(
                (self.menu_overlay.height / self.config.menu.row_height)
                    .saturating_sub(1)
                    .min(self.config.menu.max_rows),
            )
            .unwrap_or(usize::MAX),
        );
        let start = focus_cycle_visible_start(definition.entries.len(), session.selected, rows);
        let row = usize::try_from(u32::try_from(y).ok()? / self.config.menu.row_height - 1).ok()?;
        (row < rows).then_some(start + row)
    }

    fn hide_menu(&mut self, timestamp: u32) -> Result<(), X11Error> {
        let session = self.menu_session.take();
        if self.menu_overlay.mapped {
            self.connection.unmap_window(self.menu_overlay.window)?;
            self.menu_overlay.mapped = false;
        }
        self.connection
            .delete_property(self.menu_overlay.window, self.atoms._NOBOX_MENU)?;
        self.connection
            .delete_property(self.menu_overlay.window, self.atoms._NOBOX_MENU_SELECTION)?;
        if session
            .as_ref()
            .is_some_and(|session| session.pointer_grabbed)
        {
            self.connection.ungrab_pointer(timestamp)?;
        }
        if session
            .as_ref()
            .is_some_and(|session| session.keyboard_grabbed)
        {
            self.connection.ungrab_keyboard(timestamp)?;
        }
        Ok(())
    }

    fn cycle_focus(
        &mut self,
        direction: FocusCycleDirection,
        modifiers: u16,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        if self
            .focus_cycle
            .as_ref()
            .is_none_or(|cycle| cycle.modifiers != modifiers)
        {
            self.finish_focus_cycle(timestamp)?;
            let candidates = self.clients.focus_cycle_candidates();
            if candidates.is_empty() {
                return Ok(());
            }
            let index = self.clients.focused().and_then(|focused| {
                candidates
                    .iter()
                    .position(|candidate| *candidate == focused)
            });
            let status = self
                .connection
                .grab_keyboard(
                    false,
                    self.root,
                    timestamp,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )?
                .reply()?
                .status;
            if status != GrabStatus::SUCCESS {
                warn!(?status, "could not retain keyboard grab for focus cycle");
            }
            self.focus_cycle = Some(FocusCycle {
                candidates,
                index,
                original: self.clients.focused(),
                modifiers,
                keyboard_grabbed: status == GrabStatus::SUCCESS,
            });
        }

        let attempts = self
            .focus_cycle
            .as_ref()
            .map_or(0, |cycle| cycle.candidates.len());
        for _ in 0..attempts {
            let candidate = self.focus_cycle.as_mut().map(|cycle| {
                let length = cycle.candidates.len();
                let index = match (cycle.index, direction) {
                    (Some(index), FocusCycleDirection::Next) => (index + 1) % length,
                    (Some(index), FocusCycleDirection::Previous) => {
                        index.checked_sub(1).unwrap_or(length - 1)
                    }
                    (None, FocusCycleDirection::Next) => 0,
                    (None, FocusCycleDirection::Previous) => length - 1,
                };
                cycle.index = Some(index);
                cycle.candidates[index]
            });
            let Some(candidate) = candidate else {
                break;
            };
            debug!(
                candidate = candidate.raw(),
                ?direction,
                "advancing modifier-held focus cycle"
            );
            let focused = match self.focus(window_id(candidate), timestamp) {
                Ok(focused) => focused,
                Err(error) => {
                    if let Err(ungrab_error) = self.finish_focus_cycle(timestamp) {
                        warn!(%ungrab_error, "could not release failed focus-cycle grab");
                    }
                    return Err(error);
                }
            };
            if focused {
                self.update_focus_overlay()?;
                break;
            }
        }
        if self
            .focus_cycle
            .as_ref()
            .is_some_and(|cycle| !cycle.keyboard_grabbed)
        {
            self.finish_focus_cycle(timestamp)?;
        }
        Ok(())
    }

    fn close_client(&mut self, client: ClientId, timestamp: u32) -> Result<(), X11Error> {
        if self
            .clients
            .get(client)
            .is_some_and(|client| !client.policy.capabilities.closable)
        {
            return Ok(());
        }
        if let Some(ping) = self.pending_pings.get(&client) {
            if !ping.timed_out {
                debug!(
                    client = client.raw(),
                    "close request already awaiting a client pong"
                );
                return Ok(());
            }
            warn!(
                client = client.raw(),
                "force-disconnecting an unresponsive X11 client after repeated close"
            );
            self.runtime_timer.cancel_ping(client)?;
            self.pending_pings.remove(&client);
            self.connection.kill_client(window_id(client))?;
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
            if self.supports_protocol(window, self.atoms._NET_WM_PING)? {
                self.start_client_ping(client, timestamp)?;
            }
        } else {
            self.connection.kill_client(window)?;
        }
        Ok(())
    }

    fn start_client_ping(&mut self, client: ClientId, timestamp: u32) -> Result<(), X11Error> {
        self.ping_generation = self.ping_generation.wrapping_add(1);
        let generation = self.ping_generation;
        let window = window_id(client);
        let message = ClientMessageEvent::new(
            32,
            window,
            self.atoms.WM_PROTOCOLS,
            [self.atoms._NET_WM_PING, timestamp, window, 0, 0],
        );
        self.connection
            .send_event(false, window, EventMask::NO_EVENT, message)?;
        self.pending_pings.insert(
            client,
            PendingPing {
                timestamp,
                generation,
                timed_out: false,
            },
        );
        self.runtime_timer
            .arm_ping(client, generation, CLIENT_PING_TIMEOUT)
    }

    fn client_ping_timeout(&mut self, client: ClientId, generation: u32) -> Result<(), X11Error> {
        let Some(ping) = self.pending_pings.get_mut(&client) else {
            return Ok(());
        };
        if ping.generation != generation || ping.timed_out {
            return Ok(());
        }
        ping.timed_out = true;
        if !self.clients.contains(client) {
            self.pending_pings.remove(&client);
            return Ok(());
        }
        self.unresponsive_clients.insert(client);
        warn!(
            client = client.raw(),
            "X11 client did not answer _NET_WM_PING; repeat close to force disconnect"
        );
        self.sync_visible_title(client)?;
        self.refresh_frame_colors(client)?;
        self.draw_title(client)
    }

    fn client_pong(&mut self, event: &ClientMessageEvent) -> Result<(), X11Error> {
        if event.window != self.root || event.format != 32 {
            return Ok(());
        }
        let data = event.data.as_data32();
        if data[0] != self.atoms._NET_WM_PING || data[2] == NONE {
            return Ok(());
        }
        let client = client_id(data[2]);
        let Some(ping) = self.pending_pings.get(&client).copied() else {
            return Ok(());
        };
        if ping.timestamp != data[1] {
            return Ok(());
        }
        self.runtime_timer.cancel_ping(client)?;
        self.pending_pings.remove(&client);
        if self.unresponsive_clients.remove(&client) && self.clients.contains(client) {
            info!(
                client = client.raw(),
                "X11 client resumed responding to pings"
            );
            self.sync_visible_title(client)?;
            self.refresh_frame_colors(client)?;
            self.draw_title(client)?;
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
            positioned: hints.position.is_some(),
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

    fn read_client_presentation(&self, window: Window) -> Result<ClientPresentation, X11Error> {
        let states = self.read_atom_list(window, self.atoms._NET_WM_STATE)?;
        let wm_urgent = WmHints::get(&self.connection, window)?
            .reply()?
            .is_some_and(|hints| hints.urgent);
        Ok(ClientPresentation {
            skip_taskbar: states.contains(&self.atoms._NET_WM_STATE_SKIP_TASKBAR),
            skip_pager: states.contains(&self.atoms._NET_WM_STATE_SKIP_PAGER),
            urgent: wm_urgent || states.contains(&self.atoms._NET_WM_STATE_DEMANDS_ATTENTION),
        })
    }

    fn refresh_client_presentation(&mut self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        let presentation = self.read_client_presentation(window)?;
        if self.clients.set_presentation(id, presentation) {
            self.refresh_frame_colors(id)?;
            self.draw_title(id)?;
            debug!(
                window = format_args!("{window:#x}"),
                ?presentation,
                "updated client presentation hints"
            );
        }
        Ok(())
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
        let client_policy = self.read_client_policy(window, current.transient_for.is_some())?;
        let application = self.read_application_settings(window, client_policy.role)?;
        let policy = apply_size_capabilities(
            apply_application_decorations(client_policy, application.decorated),
            current.size_hints,
        );
        if current.maximize.is_some() && !policy.capabilities.maximizable {
            self.set_maximized(window, false, false)?;
        }
        if current.fullscreen.is_some() && !policy.capabilities.fullscreenable {
            self.set_fullscreen(window, false)?;
        }
        if current.shaded && !policy.decorations.titlebar {
            self.set_shaded(window, false)?;
        }
        if !self.clients.set_policy(id, policy) {
            return Ok(());
        }
        let Some(policy) = self.clients.get(id).map(|client| client.policy) else {
            return Ok(());
        };
        self.apply_frame_policy(id, policy)?;
        self.refresh_output_coverage(id);
        if self.clients.focused() == Some(id) && !policy.capabilities.focusable {
            self.clear_x_focus(self.last_timestamp)?;
        }
        self.publish_allowed_actions(id)?;
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
        self.root_geometry
    }

    fn refresh_output_coverage(&mut self, id: ClientId) -> bool {
        let coverage = self.clients.get(id).and_then(|client| {
            legacy_output_coverage(
                client.geometry,
                client.policy,
                client.maximize.is_some(),
                client.fullscreen.is_some(),
                &self.outputs,
                self.root_geometry,
            )
        });
        self.clients.set_output_coverage(id, coverage)
    }

    fn refresh_all_output_coverage(&mut self) -> bool {
        let clients = self.clients.stacking().collect::<Vec<_>>();
        let mut changed = false;
        for id in clients {
            changed |= self.refresh_output_coverage(id);
        }
        changed
    }

    fn enforce_output_coverage_layers(&mut self) -> Result<(), X11Error> {
        if self.clients.stacking().any(|id| {
            self.clients
                .get(id)
                .is_some_and(|client| client.output_coverage.is_some())
        }) {
            self.enforce_layers()?;
        }
        Ok(())
    }

    fn refresh_outputs(&mut self) -> Result<(), X11Error> {
        let geometry = self.connection.get_geometry(self.root)?.reply()?;
        let root_geometry = Geometry::new(
            i32::from(geometry.x),
            i32::from(geometry.y),
            u32::from(geometry.width),
            u32::from(geometry.height),
        );
        let outputs = discover_outputs(
            &self.connection,
            self.root,
            root_geometry,
            self.randr_version,
        )?;
        if root_geometry == self.root_geometry && outputs == self.outputs {
            return Ok(());
        }
        self.root_geometry = root_geometry;
        self.outputs = outputs;
        let invalid_fullscreen_monitors = self
            .fullscreen_monitors
            .iter()
            .filter_map(|(id, indices)| {
                fullscreen_monitor_geometry(&self.outputs, *indices)
                    .is_none()
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in invalid_fullscreen_monitors {
            self.fullscreen_monitors.remove(&id);
            self.connection
                .delete_property(window_id(id), self.atoms._NET_WM_FULLSCREEN_MONITORS)?;
        }
        self.refresh_work_area()?;
        self.rehome_clients_without_output()?;
        self.reflow_fullscreen_clients()?;
        self.refresh_all_output_coverage();
        self.enforce_layers()?;
        self.publish_workspaces()?;
        if self.focus_cycle.is_some() {
            self.update_focus_overlay()?;
        }
        if self.menu_session.is_some() {
            self.update_menu_overlay()?;
        }
        info!(
            outputs = self.outputs.outputs().len(),
            width = self.root_geometry.width,
            height = self.root_geometry.height,
            "updated X11 output topology"
        );
        Ok(())
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

    fn publish_allowed_actions(&self, id: ClientId) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let operations = client.operations();
        let mut actions = [NONE; 11];
        let mut count = 0;
        for (enabled, atom) in [
            (
                operations.workspace_movable,
                self.atoms._NET_WM_ACTION_CHANGE_DESKTOP,
            ),
            (operations.movable, self.atoms._NET_WM_ACTION_MOVE),
            (operations.resizable, self.atoms._NET_WM_ACTION_RESIZE),
            (operations.minimizable, self.atoms._NET_WM_ACTION_MINIMIZE),
            (operations.shadeable, self.atoms._NET_WM_ACTION_SHADE),
            (
                operations.maximizable,
                self.atoms._NET_WM_ACTION_MAXIMIZE_HORZ,
            ),
            (
                operations.maximizable,
                self.atoms._NET_WM_ACTION_MAXIMIZE_VERT,
            ),
            (
                operations.fullscreenable,
                self.atoms._NET_WM_ACTION_FULLSCREEN,
            ),
            (operations.closable, self.atoms._NET_WM_ACTION_CLOSE),
            (operations.above, self.atoms._NET_WM_ACTION_ABOVE),
            (operations.below, self.atoms._NET_WM_ACTION_BELOW),
        ] {
            if enabled {
                actions[count] = atom;
                count += 1;
            }
        }
        let actions = &actions[..count];
        if self.read_atom_list(window_id(id), self.atoms._NET_WM_ALLOWED_ACTIONS)? == actions {
            return Ok(());
        }
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            window_id(id),
            self.atoms._NET_WM_ALLOWED_ACTIONS,
            AtomEnum::ATOM,
            actions,
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
        let mut work_areas = Vec::new();
        let mut output_work_areas = BTreeMap::new();
        for index in 0..self.clients.workspace_count() {
            let workspace = WorkspaceId::new(index);
            let reservations = self
                .struts
                .iter()
                .filter_map(|(id, reservation)| {
                    self.clients
                        .get(*id)
                        .filter(|client| client.workspace.is_visible_on(workspace))
                        .map(|_| *reservation)
                })
                .collect::<Vec<_>>();
            work_areas.push(screen.work_area(reservations.iter().copied()));
            for output in self.outputs.outputs() {
                let local = reservations
                    .iter()
                    .copied()
                    .map(|reservation| output_reservations(reservation, output.geometry, screen));
                output_work_areas.insert((output.id, workspace), output.geometry.work_area(local));
            }
        }
        if work_areas == self.work_areas && output_work_areas == self.output_work_areas {
            return Ok(false);
        }
        let published_changed = work_areas != self.work_areas;
        self.work_areas = work_areas;
        self.output_work_areas = output_work_areas;
        if published_changed {
            self.publish_work_area()?;
        }
        self.reflow_maximized_clients()?;
        info!(
            workspaces = self.work_areas.len(),
            outputs = self.outputs.outputs().len(),
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

    fn reflow_fullscreen_clients(&mut self) -> Result<(), X11Error> {
        let fullscreen = self
            .clients
            .stacking()
            .filter(|id| {
                self.clients
                    .get(*id)
                    .is_some_and(|client| client.fullscreen.is_some())
            })
            .collect::<Vec<_>>();
        for id in fullscreen {
            self.set_fullscreen(window_id(id), true)?;
        }
        Ok(())
    }

    fn rehome_clients_without_output(&mut self) -> Result<(), X11Error> {
        let clients = self
            .clients
            .stacking()
            .filter(|id| {
                self.clients.get(*id).is_some_and(|client| {
                    client.maximize.is_none()
                        && client.fullscreen.is_none()
                        && self.outputs.overlapping_output(client.geometry).is_none()
                })
            })
            .collect::<Vec<_>>();
        for id in clients {
            let Some(client) = self.clients.get(id).copied() else {
                continue;
            };
            let geometry = client.geometry.clamp_position(self.available_geometry(id));
            if geometry != client.geometry {
                self.configure_decorated_client(id, geometry)?;
                self.clients.set_geometry(id, geometry);
            }
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
        let output = self.clients.get(id).map_or_else(
            || self.outputs.primary(),
            |client| self.outputs.output_for(client.geometry),
        );
        let work_area = self
            .output_work_areas
            .get(&(output.id, workspace))
            .copied()
            .unwrap_or(output.geometry);
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
        if self.refresh_output_coverage(id) {
            self.enforce_layers()?;
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
        if fullscreen && self.clients.get(id).is_some_and(|client| client.shaded) {
            self.set_shaded(window, false)?;
        }
        let output = self.clients.get(id).map_or_else(
            || self.outputs.primary(),
            |client| self.outputs.output_for(client.geometry),
        );
        let fullscreen_geometry = self
            .fullscreen_monitors
            .get(&id)
            .and_then(|indices| fullscreen_monitor_geometry(&self.outputs, *indices))
            .unwrap_or(output.geometry);
        let previous = self
            .clients
            .get(id)
            .is_some_and(|client| client.fullscreen.is_some());
        let geometry = self
            .clients
            .set_fullscreen(id, fullscreen, fullscreen_geometry);
        self.refresh_output_coverage(id);
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
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_FULLSCREEN, actual)?;
        self.publish_allowed_actions(id)
    }

    fn net_wm_fullscreen_monitors(&mut self, event: &ClientMessageEvent) -> Result<(), X11Error> {
        let id = client_id(event.window);
        let indices = FullscreenMonitorIndices::from_message(event.data.as_data32());
        if fullscreen_monitor_geometry(&self.outputs, indices).is_none() {
            debug!(
                window = format_args!("{:#x}", event.window),
                ?indices,
                outputs = self.outputs.outputs().len(),
                "ignored invalid fullscreen-monitor request"
            );
            return Ok(());
        }
        self.fullscreen_monitors.insert(id, indices);
        self.connection.change_property32(
            x11rb::protocol::xproto::PropMode::REPLACE,
            event.window,
            self.atoms._NET_WM_FULLSCREEN_MONITORS,
            AtomEnum::CARDINAL,
            &indices.property(),
        )?;
        if self
            .clients
            .get(id)
            .is_some_and(|client| client.fullscreen.is_some())
        {
            self.set_fullscreen(event.window, true)?;
        }
        Ok(())
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
        if states.contains(&atom) == enabled {
            return Ok(());
        }
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

    fn sync_wm_owned_states(&self, window: Window) -> Result<(), X11Error> {
        let id = client_id(window);
        let Some(client) = self.clients.get(id) else {
            return Ok(());
        };
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_HIDDEN, client.iconic)?;
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_SHADED, client.shaded)?;
        self.sync_boolean_state(
            window,
            self.atoms._NET_WM_STATE_FOCUSED,
            self.clients.focused() == Some(id),
        )
    }

    fn sync_focused_state(&mut self) -> Result<(), X11Error> {
        let focused = self.clients.focused();
        if self.published_focus == focused {
            return Ok(());
        }
        if let Some(previous) = self.published_focus
            && self.clients.contains(previous)
        {
            self.sync_boolean_state(window_id(previous), self.atoms._NET_WM_STATE_FOCUSED, false)?;
        }
        if let Some(focused) = focused {
            self.sync_boolean_state(window_id(focused), self.atoms._NET_WM_STATE_FOCUSED, true)?;
        }
        self.published_focus = focused;
        Ok(())
    }

    fn clear_demands_attention(&mut self, window: Window) -> Result<(), X11Error> {
        if !self
            .read_atom_list(window, self.atoms._NET_WM_STATE)?
            .contains(&self.atoms._NET_WM_STATE_DEMANDS_ATTENTION)
        {
            return Ok(());
        }
        self.sync_boolean_state(window, self.atoms._NET_WM_STATE_DEMANDS_ATTENTION, false)?;
        self.refresh_client_presentation(window)
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
        let presentation_atoms = [
            self.atoms._NET_WM_STATE_SKIP_TASKBAR,
            self.atoms._NET_WM_STATE_SKIP_PAGER,
            self.atoms._NET_WM_STATE_DEMANDS_ATTENTION,
        ];
        let mut states = self.read_atom_list(event.window, self.atoms._NET_WM_STATE)?;
        let mut presentation_changed = false;
        for (index, state) in requested.into_iter().enumerate() {
            if (index == 1 && state == requested[0]) || !presentation_atoms.contains(&state) {
                continue;
            }
            let current = states.contains(&state);
            let Some(enabled) = ewmh_state_action(current, data[0]) else {
                continue;
            };
            if enabled == current {
                continue;
            }
            states.retain(|candidate| *candidate != state);
            if enabled {
                states.push(state);
            }
            presentation_changed = true;
        }
        if presentation_changed {
            self.connection.change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                event.window,
                self.atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                &states,
            )?;
            self.refresh_client_presentation(event.window)?;
        }

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

        if requested.contains(&self.atoms._NET_WM_STATE_SHADED)
            && let Some(shaded) = ewmh_state_action(client.shaded, data[0])
            && shaded != client.shaded
        {
            self.set_shaded(event.window, shaded)?;
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
        let frame_height = if self.clients.get(id).is_some_and(|client| client.shaded) {
            titlebar_height
        } else {
            geometry.height.saturating_add(titlebar_height)
        };
        self.connection.configure_window(
            frame.window,
            &ConfigureWindowAux::new()
                .x(outer.x)
                .y(outer.y)
                .width(geometry.width)
                .height(frame_height),
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
        if self.bounding_shaped.contains(&id) {
            self.apply_frame_shape(id, SK::BOUNDING, true)?;
        }
        if self.input_shaped.contains(&id) {
            self.apply_frame_shape(id, SK::INPUT, true)?;
        }
        Ok(())
    }

    fn configure_request(&mut self, event: &ConfigureRequestEvent) -> Result<(), X11Error> {
        let id = client_id(event.window);
        let managed = self.clients.get(id).copied();
        if let Some(client) = managed {
            self.configure_managed_geometry(
                id,
                GeometryRequest {
                    x: event
                        .value_mask
                        .contains(ConfigWindow::X)
                        .then_some(i32::from(event.x)),
                    y: event
                        .value_mask
                        .contains(ConfigWindow::Y)
                        .then_some(i32::from(event.y)),
                    width: event
                        .value_mask
                        .contains(ConfigWindow::WIDTH)
                        .then_some(u32::from(event.width)),
                    height: event
                        .value_mask
                        .contains(ConfigWindow::HEIGHT)
                        .then_some(u32::from(event.height)),
                    gravity: client.gravity,
                },
            )?;

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

    fn configure_managed_geometry(
        &mut self,
        id: ClientId,
        request: GeometryRequest,
    ) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let requested = Size::new(
            request.width.unwrap_or(client.geometry.width),
            request.height.unwrap_or(client.geometry.height),
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
            } else {
                request
                    .width
                    .map_or(client.geometry.width, |_| constrained.width)
            },
            height: if client.fullscreen.is_some()
                || client.maximize.is_some_and(|state| state.vertical)
            {
                client.geometry.height
            } else {
                request
                    .height
                    .map_or(client.geometry.height, |_| constrained.height)
            },
        };
        let (gravity_x, gravity_y) = request.gravity.adjust_resize(
            client.geometry,
            final_size,
            request.x.is_some(),
            request.y.is_some(),
        );
        let final_x = if client.fullscreen.is_some()
            || client.maximize.is_some_and(|state| state.horizontal)
        {
            client.geometry.x
        } else {
            request.x.unwrap_or(gravity_x)
        };
        let final_y =
            if client.fullscreen.is_some() || client.maximize.is_some_and(|state| state.vertical) {
                client.geometry.y
            } else {
                request.y.unwrap_or(gravity_y)
            };
        let geometry = Geometry::new(final_x, final_y, final_size.width, final_size.height);
        self.configure_decorated_client(id, geometry)?;
        self.clients.set_geometry(id, geometry);
        if self.refresh_output_coverage(id) {
            self.enforce_layers()?;
        }
        Ok(())
    }

    fn net_moveresize_window(&mut self, event: &ClientMessageEvent) -> Result<(), X11Error> {
        let data = event.data.as_data32();
        let flags = data[0];
        let id = client_id(event.window);
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let gravity = match flags & 0xff {
            0 => client.gravity,
            value => match ewmh_gravity(value) {
                Some(gravity) => gravity,
                None => return Ok(()),
            },
        };
        self.configure_managed_geometry(
            id,
            GeometryRequest {
                x: (flags & (1 << 8) != 0).then_some(signed_cardinal(data[1])),
                y: (flags & (1 << 9) != 0).then_some(signed_cardinal(data[2])),
                width: (flags & (1 << 10) != 0).then_some(data[3]),
                height: (flags & (1 << 11) != 0).then_some(data[4]),
                gravity,
            },
        )
    }

    fn net_wm_moveresize(&mut self, event: &ClientMessageEvent) -> Result<(), X11Error> {
        let data = event.data.as_data32();
        let Some(request) = net_wm_moveresize_request(data[2]) else {
            return Ok(());
        };
        if request == NetWmMoveResizeRequest::Cancel {
            if self.drag.is_some_and(|drag| drag.window == event.window) {
                self.cancel_drag(CURRENT_TIME)?;
            }
            return Ok(());
        }
        let id = client_id(event.window);
        if !self.clients.contains(id) {
            return Ok(());
        }
        let keyboard = matches!(
            request,
            NetWmMoveResizeRequest::MoveKeyboard | NetWmMoveResizeRequest::ResizeKeyboard
        );
        let (pointer_x, pointer_y) = if keyboard {
            let pointer = self.connection.query_pointer(self.root)?.reply()?;
            (pointer.root_x, pointer.root_y)
        } else {
            (
                clamp_i16(signed_cardinal(data[0])),
                clamp_i16(signed_cardinal(data[1])),
            )
        };
        let button = if data[3] == 0 {
            None
        } else {
            let Ok(button) = u8::try_from(data[3]) else {
                return Ok(());
            };
            Some(button)
        };
        let kind = match request {
            NetWmMoveResizeRequest::Move | NetWmMoveResizeRequest::MoveKeyboard => DragKind::Move,
            NetWmMoveResizeRequest::Resize(edges) => DragKind::Resize(edges),
            NetWmMoveResizeRequest::ResizeKeyboard => DragKind::Resize(ResizeEdges::bottom_right()),
            NetWmMoveResizeRequest::Cancel => return Ok(()),
        };
        debug!(
            window = format_args!("{:#x}", event.window),
            direction = data[2],
            source = data[4],
            "starting client-requested interactive operation"
        );
        self.begin_drag(
            id,
            DragStart {
                kind,
                pointer_x,
                pointer_y,
                button,
                keyboard,
                grab_pointer: true,
                timestamp: CURRENT_TIME,
            },
        )
    }

    fn button_press(&mut self, event: &ButtonPressEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        if let Some(drag) = &mut self.drag {
            if drag.button.is_none() {
                drag.button = Some(event.detail);
            }
            return Ok(());
        }
        let target = self.mouse_target(event.event, event.child, event.root_x, event.root_y);
        let modifiers = mouse_modifier_mask(u16::from(event.state));
        let pointer = PointerInvocation {
            target,
            button: event.detail,
            root_x: event.root_x,
            root_y: event.root_y,
        };
        self.dispatch_mouse_binding(
            target,
            event.detail,
            modifiers,
            MouseTrigger::Press,
            pointer,
            event.time,
        )?;
        if self.menu_session.is_some() {
            return Ok(());
        }
        if self.has_mouse_binding(target.context, event.detail, modifiers) {
            self.mouse_gesture = Some(MouseGesture {
                target,
                button: event.detail,
                modifiers,
                root_x: event.root_x,
                root_y: event.root_y,
                dragged: false,
            });
        }
        Ok(())
    }

    fn button_motion(&mut self, event: &MotionNotifyEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        if let Some(drag) = self.drag {
            return if drag.keyboard {
                Ok(())
            } else {
                self.pointer_motion(event.root_x, event.root_y)
            };
        }
        let Some(gesture) = self.mouse_gesture else {
            return Ok(());
        };
        if gesture.dragged
            || (i32::from(event.root_x) - i32::from(gesture.root_x)).unsigned_abs()
                < self.config.mouse.drag_threshold
                && (i32::from(event.root_y) - i32::from(gesture.root_y)).unsigned_abs()
                    < self.config.mouse.drag_threshold
        {
            return Ok(());
        }
        if let Some(active) = &mut self.mouse_gesture {
            active.dragged = true;
        }
        let pointer = PointerInvocation {
            target: gesture.target,
            button: gesture.button,
            root_x: gesture.root_x,
            root_y: gesture.root_y,
        };
        self.dispatch_mouse_binding(
            gesture.target,
            gesture.button,
            gesture.modifiers,
            MouseTrigger::Drag,
            pointer,
            event.time,
        )?;
        self.pointer_motion(event.root_x, event.root_y)
    }

    fn button_release(&mut self, event: &ButtonReleaseEvent) -> Result<(), X11Error> {
        self.last_timestamp = event.time;
        if self
            .drag
            .is_some_and(|drag| drag.button.is_none_or(|button| button == event.detail))
        {
            self.mouse_gesture = None;
            return self.finish_drag(event.time);
        }
        let Some(gesture) = self.mouse_gesture.take() else {
            return Ok(());
        };
        if gesture.button != event.detail || gesture.dragged {
            return Ok(());
        }
        let pointer = PointerInvocation {
            target: gesture.target,
            button: gesture.button,
            root_x: gesture.root_x,
            root_y: gesture.root_y,
        };
        self.dispatch_mouse_binding(
            gesture.target,
            gesture.button,
            gesture.modifiers,
            MouseTrigger::Release,
            pointer,
            event.time,
        )?;
        if !self.release_over_target(event, gesture.target) {
            self.last_mouse_click = None;
            return Ok(());
        }
        self.dispatch_mouse_binding(
            gesture.target,
            gesture.button,
            gesture.modifiers,
            MouseTrigger::Click,
            pointer,
            event.time,
        )?;
        let current = MouseClick {
            target: gesture.target,
            button: gesture.button,
            modifiers: gesture.modifiers,
            root_x: event.root_x,
            root_y: event.root_y,
            timestamp: event.time,
        };
        let double_click = self.last_mouse_click.take().is_some_and(|previous| {
            previous.target == current.target
                && previous.button == current.button
                && previous.modifiers == current.modifiers
                && current.timestamp.wrapping_sub(previous.timestamp)
                    <= self.config.mouse.double_click_ms
                && (i32::from(current.root_x) - i32::from(previous.root_x)).unsigned_abs() < 8
                && (i32::from(current.root_y) - i32::from(previous.root_y)).unsigned_abs() < 8
        });
        if double_click {
            self.dispatch_mouse_binding(
                gesture.target,
                gesture.button,
                gesture.modifiers,
                MouseTrigger::DoubleClick,
                pointer,
                event.time,
            )?;
        } else {
            self.last_mouse_click = Some(current);
        }
        Ok(())
    }

    fn release_over_target(&self, event: &ButtonReleaseEvent, target: MouseTarget) -> bool {
        if event.event != target.window {
            let released = self.mouse_target(event.event, event.child, event.root_x, event.root_y);
            return released.client == target.client && released.context == target.context;
        }
        let (width, height, border) = match self.frame_parts.get(&target.window).copied() {
            Some(FramePart::Button(id, _)) => {
                let size = self.frames.get(&id).map_or(1, |frame| {
                    frame
                        .extents
                        .top
                        .saturating_sub(frame.extents.left)
                        .saturating_sub(8)
                        .max(1)
                });
                (size, size, 0)
            }
            Some(FramePart::Container(id)) => {
                let Some(client) = self.clients.get(id) else {
                    return false;
                };
                let Some(frame) = self.frames.get(&id) else {
                    return false;
                };
                if self.frame_context_at(id, event.root_x, event.root_y) != target.context {
                    return false;
                }
                (
                    client.geometry.width,
                    client
                        .geometry
                        .height
                        .saturating_add(frame.extents.top.saturating_sub(frame.extents.left)),
                    frame.extents.left,
                )
            }
            None if target.window == self.root => {
                (self.root_geometry.width, self.root_geometry.height, 0)
            }
            None => {
                let Some(client) = target.client.and_then(|id| self.clients.get(id)) else {
                    return false;
                };
                (client.geometry.width, client.geometry.height, 0)
            }
        };
        point_inside_window(event.event_x, event.event_y, width, height, border)
    }

    fn dispatch_mouse_binding(
        &mut self,
        target: MouseTarget,
        button: u8,
        modifiers: u16,
        trigger: MouseTrigger,
        pointer: PointerInvocation,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        let actions = mouse_context_chain(target.context)
            .iter()
            .find_map(|context| {
                self.mouse_bindings
                    .get(&MouseBindingKey {
                        context: *context,
                        button,
                        modifiers,
                        trigger,
                    })
                    .cloned()
            })
            .unwrap_or_default();
        for action in actions {
            self.run_action(action, target.client, modifiers, timestamp, Some(pointer))?;
        }
        Ok(())
    }

    fn has_mouse_binding(&self, context: MouseContext, button: u8, modifiers: u16) -> bool {
        mouse_context_chain(context).iter().any(|context| {
            [
                MouseTrigger::Press,
                MouseTrigger::Release,
                MouseTrigger::Click,
                MouseTrigger::DoubleClick,
                MouseTrigger::Drag,
            ]
            .into_iter()
            .any(|trigger| {
                self.mouse_bindings.contains_key(&MouseBindingKey {
                    context: *context,
                    button,
                    modifiers,
                    trigger,
                })
            })
        })
    }

    fn mouse_target(
        &self,
        event_window: Window,
        child: Window,
        root_x: i16,
        root_y: i16,
    ) -> MouseTarget {
        for candidate in [child, event_window] {
            match self.frame_parts.get(&candidate).copied() {
                Some(FramePart::Button(id, kind)) => {
                    return MouseTarget {
                        client: Some(id),
                        context: match kind {
                            FrameButtonKind::Minimize => MouseContext::Minimize,
                            FrameButtonKind::Maximize => MouseContext::Maximize,
                            FrameButtonKind::Close => MouseContext::Close,
                        },
                        window: candidate,
                    };
                }
                Some(FramePart::Container(id)) => {
                    return MouseTarget {
                        client: Some(id),
                        context: self.frame_context_at(id, root_x, root_y),
                        window: candidate,
                    };
                }
                None if self.clients.contains(client_id(candidate)) => {
                    let id = client_id(candidate);
                    let context = if self
                        .clients
                        .get(id)
                        .is_some_and(|client| client.policy.role == ClientRole::Desktop)
                    {
                        MouseContext::Desktop
                    } else {
                        MouseContext::Client
                    };
                    return MouseTarget {
                        client: Some(id),
                        context,
                        window: candidate,
                    };
                }
                _ => {}
            }
        }
        MouseTarget {
            client: None,
            context: MouseContext::Root,
            window: self.root,
        }
    }

    fn frame_context_at(&self, id: ClientId, root_x: i16, root_y: i16) -> MouseContext {
        let Some(client) = self.clients.get(id) else {
            return MouseContext::Frame;
        };
        let Some(frame) = self.frames.get(&id) else {
            return MouseContext::Frame;
        };
        let x = i32::from(root_x);
        let y = i32::from(root_y);
        let outer = frame.extents.outer_geometry(client.geometry);
        let content_right = geometry_end(client.geometry.x, client.geometry.width);
        let content_bottom = geometry_end(client.geometry.y, client.geometry.height);
        let outer_right = geometry_end(outer.x, outer.width);
        let outer_bottom = geometry_end(outer.y, outer.height);
        if x < outer.x || x >= outer_right || y < outer.y || y >= outer_bottom {
            return MouseContext::Frame;
        }
        let on_left = x < client.geometry.x;
        let on_right = x >= content_right;
        let titlebar_height = frame.extents.top.saturating_sub(frame.extents.left);
        let titlebar_top = client
            .geometry
            .y
            .saturating_sub(i32::try_from(titlebar_height).unwrap_or(i32::MAX));
        let on_top = y < titlebar_top;
        let on_bottom = y >= content_bottom;
        match (on_top, on_bottom, on_left, on_right) {
            (true, _, true, _) => MouseContext::TopLeft,
            (true, _, _, true) => MouseContext::TopRight,
            (_, true, true, _) => MouseContext::BottomLeft,
            (_, true, _, true) => MouseContext::BottomRight,
            (true, _, _, _) => MouseContext::Top,
            (_, true, _, _) => MouseContext::Bottom,
            (_, _, true, _) => MouseContext::Left,
            (_, _, _, true) => MouseContext::Right,
            _ if y < client.geometry.y => MouseContext::Titlebar,
            _ => MouseContext::Client,
        }
    }

    fn start_drag(
        &mut self,
        id: ClientId,
        kind: DragKind,
        pointer: PointerInvocation,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        self.begin_drag(
            id,
            DragStart {
                kind,
                pointer_x: pointer.root_x,
                pointer_y: pointer.root_y,
                button: Some(pointer.button),
                keyboard: false,
                grab_pointer: false,
                timestamp,
            },
        )
    }

    fn begin_drag(&mut self, id: ClientId, start: DragStart) -> Result<(), X11Error> {
        if self.drag.is_some() {
            return Ok(());
        }
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let permitted = match start.kind {
            DragKind::Move => client.policy.capabilities.movable,
            DragKind::Resize(_) => client.policy.capabilities.resizable,
        } && client.maximize.is_none()
            && client.fullscreen.is_none()
            && !client.iconic
            && self.clients.is_visible(id);
        if !permitted {
            return Ok(());
        }
        if start.grab_pointer {
            let pointer_status = self
                .connection
                .grab_pointer(
                    false,
                    self.root,
                    EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                    self.root,
                    NONE,
                    start.timestamp,
                )?
                .reply()?
                .status;
            if pointer_status != GrabStatus::SUCCESS {
                warn!(
                    status = u8::from(pointer_status),
                    "could not grab pointer for client-requested interactive operation"
                );
                return Ok(());
            }
        }
        let keyboard_status = self
            .connection
            .grab_keyboard(
                false,
                self.root,
                start.timestamp,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            )?
            .reply()?
            .status;
        if keyboard_status != GrabStatus::SUCCESS {
            if start.grab_pointer {
                self.connection.ungrab_pointer(start.timestamp)?;
            }
            warn!(
                status = u8::from(keyboard_status),
                "could not grab keyboard for cancellable interactive operation"
            );
            return Ok(());
        }
        let sync = if matches!(start.kind, DragKind::Resize(_)) {
            match self.begin_sync_resize(id) {
                Ok(sync) => sync,
                Err(error) => {
                    self.connection.ungrab_keyboard(start.timestamp)?;
                    if start.grab_pointer {
                        self.connection.ungrab_pointer(start.timestamp)?;
                    }
                    return Err(error);
                }
            }
        } else {
            None
        };
        self.mouse_gesture = None;
        self.last_mouse_click = None;
        self.drag = Some(Drag {
            window: window_id(id),
            kind: start.kind,
            button: start.button,
            pointer_x: start.pointer_x,
            pointer_y: start.pointer_y,
            initial: client.geometry,
            sync,
            keyboard: start.keyboard,
            keyboard_resize_edge: None,
            pointer_grabbed: start.grab_pointer,
        });
        Ok(())
    }

    fn begin_sync_resize(&mut self, id: ClientId) -> Result<Option<SyncResize>, X11Error> {
        let Some(counter) = self.sync_counters.get(&id).copied() else {
            return Ok(None);
        };
        if !sync_request_succeeded(
            self.connection
                .sync_set_counter(counter, sync_value(0))?
                .check(),
        )? {
            self.sync_counters.remove(&id);
            return Ok(None);
        }
        let alarm = self.connection.generate_id()?;
        let attributes = CreateAlarmAux::new()
            .counter(counter)
            .value_type(VALUETYPE::ABSOLUTE)
            .value(sync_value(1))
            .test_type(TESTTYPE::POSITIVE_TRANSITION)
            .delta(sync_value(1))
            .events(1_u32);
        if !sync_request_succeeded(
            self.connection
                .sync_create_alarm(alarm, &attributes)?
                .check(),
        )? {
            return Ok(None);
        }
        Ok(Some(SyncResize {
            alarm,
            sequence: 0,
            waiting: false,
            timeout_generation: 0,
            pending: None,
        }))
    }

    fn resize_edges(&self, id: ClientId, pointer: PointerInvocation) -> ResizeEdges {
        match pointer.target.context {
            MouseContext::TopLeft => ResizeEdges::new(true, false, true, false),
            MouseContext::TopRight => ResizeEdges::new(false, true, true, false),
            MouseContext::BottomLeft => ResizeEdges::new(true, false, false, true),
            MouseContext::BottomRight => ResizeEdges::new(false, true, false, true),
            MouseContext::Top => ResizeEdges::new(false, false, true, false),
            MouseContext::Bottom => ResizeEdges::new(false, false, false, true),
            MouseContext::Left => ResizeEdges::new(true, false, false, false),
            MouseContext::Right => ResizeEdges::new(false, true, false, false),
            MouseContext::Border => self
                .clients
                .get(id)
                .map_or_else(ResizeEdges::bottom_right, |client| {
                    ResizeEdges::nearest(client.geometry, pointer.root_x, pointer.root_y)
                }),
            MouseContext::Root
            | MouseContext::Desktop
            | MouseContext::Client
            | MouseContext::Frame
            | MouseContext::Titlebar
            | MouseContext::Minimize
            | MouseContext::Maximize
            | MouseContext::Close => ResizeEdges::bottom_right(),
        }
    }

    fn keyboard_drag_direction(&self, keycode: u8) -> Option<KeyboardDragDirection> {
        if self.menu_keycodes.left.contains(&keycode) {
            Some(KeyboardDragDirection::Left)
        } else if self.menu_keycodes.right.contains(&keycode) {
            Some(KeyboardDragDirection::Right)
        } else if self.menu_keycodes.up.contains(&keycode) {
            Some(KeyboardDragDirection::Up)
        } else if self.menu_keycodes.down.contains(&keycode) {
            Some(KeyboardDragDirection::Down)
        } else {
            None
        }
    }

    fn keyboard_drag(
        &mut self,
        direction: KeyboardDragDirection,
        state: u16,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        let Some(drag) = self.drag else {
            return Ok(());
        };
        if !drag.keyboard {
            return Ok(());
        }
        let id = client_id(drag.window);
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let bounds = self.available_geometry(id);
        match drag.kind {
            DragKind::Move => {
                let fine = state & u16::from(ModMask::CONTROL) != 0;
                let edge = state & u16::from(ModMask::SHIFT) != 0;
                let step = if fine { 1 } else { 8 };
                let geometry =
                    keyboard_move_geometry(client.geometry, bounds, direction, step, edge);
                self.apply_drag_geometry(id, geometry, timestamp)
            }
            DragKind::Resize(_) => {
                let selected = drag.keyboard_resize_edge;
                if selected.is_none_or(|selected| selected.axis() != direction.axis()) {
                    if let Some(drag) = &mut self.drag {
                        drag.keyboard_resize_edge = Some(direction);
                        drag.kind = DragKind::Resize(direction.resize_edges());
                    }
                    return Ok(());
                }
                let edges = selected
                    .expect("a matching keyboard-resize axis has a selected edge")
                    .resize_edges();
                let increment = client.size_hints.increment.map_or(1, |increment| {
                    if direction.axis() == KeyboardDragAxis::Horizontal {
                        increment.width
                    } else {
                        increment.height
                    }
                });
                let step = if increment > 1 {
                    increment
                } else if state & u16::from(ModMask::CONTROL) != 0 {
                    1
                } else {
                    8
                };
                let (dx, dy) = direction.delta(step);
                let geometry = self.resize_drag_geometry(
                    id,
                    ResizeDragRequest {
                        initial: client.geometry,
                        edges,
                        dx,
                        dy,
                        bounds,
                        resistance: 0,
                    },
                );
                self.apply_drag_geometry(id, geometry, timestamp)
            }
        }
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
            DragKind::Resize(edges) => self.resize_drag_geometry(
                id,
                ResizeDragRequest {
                    initial: drag.initial,
                    edges,
                    dx,
                    dy,
                    bounds,
                    resistance,
                },
            ),
        };
        self.apply_drag_geometry(id, geometry, self.last_timestamp)
    }

    fn resize_drag_geometry(&self, id: ClientId, request: ResizeDragRequest) -> Geometry {
        let resized = resize_from_edges(
            request.initial,
            request.edges,
            request.dx,
            request.dy,
            request.bounds,
            request.resistance,
        );
        let requested = Size::new(resized.width, resized.height);
        let constrained = self
            .clients
            .get(id)
            .map_or(requested, |client| client.size_hints.constrain(requested));
        let titlebar_height = self.frames.get(&id).map_or(0, |frame| {
            frame.extents.top.saturating_sub(frame.extents.left)
        });
        let constrained = x_content_size(constrained, titlebar_height);
        let initial_right = geometry_end(request.initial.x, request.initial.width);
        let initial_bottom = geometry_end(request.initial.y, request.initial.height);
        Geometry::new(
            if request.edges.left {
                initial_right.saturating_sub(i32::try_from(constrained.width).unwrap_or(i32::MAX))
            } else {
                resized.x
            },
            if request.edges.top {
                initial_bottom.saturating_sub(i32::try_from(constrained.height).unwrap_or(i32::MAX))
            } else {
                resized.y
            },
            constrained.width,
            constrained.height,
        )
    }

    fn apply_drag_geometry(
        &mut self,
        id: ClientId,
        geometry: Geometry,
        timestamp: u32,
    ) -> Result<(), X11Error> {
        if self
            .clients
            .get(id)
            .is_some_and(|client| client.geometry == geometry)
        {
            return Ok(());
        }
        let Some(sync) = self.drag.and_then(|drag| drag.sync) else {
            self.configure_decorated_client(id, geometry)?;
            self.clients.set_geometry(id, geometry);
            return Ok(());
        };
        if sync.waiting {
            if let Some(drag) = &mut self.drag
                && let Some(sync) = &mut drag.sync
            {
                sync.pending = Some(geometry);
            }
            return Ok(());
        }

        let sequence = if sync.sequence >= i64::MAX as u64 {
            1
        } else {
            sync.sequence + 1
        };
        self.sync_resize_generation = self.sync_resize_generation.wrapping_add(1);
        let timeout_generation = self.sync_resize_generation;
        let value = sync_value(sequence);
        let message = ClientMessageEvent::new(
            32,
            window_id(id),
            self.atoms.WM_PROTOCOLS,
            [
                self.atoms._NET_WM_SYNC_REQUEST,
                timestamp,
                value.lo,
                u32::try_from(value.hi).expect("synchronized resize values stay positive"),
                0,
            ],
        );
        self.connection
            .send_event(false, window_id(id), EventMask::NO_EVENT, message)?;
        self.configure_decorated_client(id, geometry)?;
        self.clients.set_geometry(id, geometry);
        if let Some(drag) = &mut self.drag
            && let Some(sync) = &mut drag.sync
        {
            sync.sequence = sequence;
            sync.waiting = true;
            sync.timeout_generation = timeout_generation;
            sync.pending = None;
        }
        self.runtime_timer
            .arm_sync_resize(id, timeout_generation, SYNC_RESIZE_TIMEOUT)
    }

    fn sync_alarm_notify(&mut self, event: &sync::AlarmNotifyEvent) -> Result<(), X11Error> {
        let Some(drag) = self.drag else {
            return Ok(());
        };
        let Some(sync) = drag.sync else {
            return Ok(());
        };
        if event.alarm != sync.alarm
            || !sync.waiting
            || sync_value_u64(event.counter_value).is_none_or(|value| value < sync.sequence)
        {
            return Ok(());
        }

        self.last_timestamp = event.timestamp;
        self.runtime_timer.cancel_sync_resize()?;
        let pending = if let Some(drag) = &mut self.drag
            && let Some(sync) = &mut drag.sync
        {
            sync.waiting = false;
            sync.pending.take()
        } else {
            None
        };
        if let Some(geometry) = pending {
            self.apply_drag_geometry(client_id(drag.window), geometry, event.timestamp)?;
        }
        Ok(())
    }

    fn sync_resize_timeout(&mut self, id: ClientId, generation: u32) -> Result<(), X11Error> {
        let Some(drag) = self.drag else {
            return Ok(());
        };
        let Some(sync) = drag.sync else {
            return Ok(());
        };
        if client_id(drag.window) != id || !sync.waiting || sync.timeout_generation != generation {
            return Ok(());
        }

        let sync = self
            .drag
            .as_mut()
            .and_then(|drag| drag.sync.take())
            .expect("validated synchronized resize remains active");
        let pending = sync.pending;
        self.end_sync_resize(sync)?;
        warn!(
            window = format_args!("{:#x}", window_id(id)),
            "client did not acknowledge synchronized resize; continuing without pacing"
        );
        if let Some(geometry) = pending {
            self.apply_drag_geometry(id, geometry, self.last_timestamp)?;
        }
        Ok(())
    }

    fn end_sync_resize(&self, sync: SyncResize) -> Result<(), X11Error> {
        self.runtime_timer.cancel_sync_resize()?;
        sync_request_succeeded(self.connection.sync_destroy_alarm(sync.alarm)?.check())?;
        Ok(())
    }

    fn finish_drag(&mut self, timestamp: u32) -> Result<(), X11Error> {
        self.mouse_gesture = None;
        let Some(drag) = self.drag.take() else {
            return Ok(());
        };
        let id = client_id(drag.window);
        if let Some(sync) = drag.sync {
            self.end_sync_resize(sync)?;
            if let Some(geometry) = sync.pending {
                self.configure_decorated_client(id, geometry)?;
                self.clients.set_geometry(id, geometry);
            }
        }
        let coverage_changed = self.refresh_output_coverage(id);
        self.connection.ungrab_keyboard(timestamp)?;
        if drag.pointer_grabbed {
            self.connection.ungrab_pointer(timestamp)?;
        }
        if coverage_changed {
            self.enforce_layers()?;
        }
        Ok(())
    }

    fn cancel_drag(&mut self, timestamp: u32) -> Result<(), X11Error> {
        self.mouse_gesture = None;
        let Some(drag) = self.drag.take() else {
            return Ok(());
        };
        let id = client_id(drag.window);
        if let Some(sync) = drag.sync {
            self.end_sync_resize(sync)?;
        }
        self.configure_decorated_client(id, drag.initial)?;
        self.clients.set_geometry(id, drag.initial);
        let coverage_changed = self.refresh_output_coverage(id);
        self.connection.ungrab_keyboard(timestamp)?;
        if drag.pointer_grabbed {
            self.connection.ungrab_pointer(timestamp)?;
        }
        if coverage_changed {
            self.enforce_layers()?;
        }
        Ok(())
    }

    fn restore_session_stacking(&mut self, id: ClientId) -> Result<(), X11Error> {
        let Some(index) = self.session_stacking.get(&id).copied() else {
            return Ok(());
        };
        let lower = self
            .session_stacking
            .iter()
            .filter(|(candidate, candidate_index)| **candidate != id && **candidate_index < index)
            .max_by_key(|(_, candidate_index)| **candidate_index)
            .map(|(candidate, _)| *candidate);
        let higher = self
            .session_stacking
            .iter()
            .filter(|(candidate, candidate_index)| **candidate != id && **candidate_index > index)
            .min_by_key(|(_, candidate_index)| **candidate_index)
            .map(|(candidate, _)| *candidate);
        let values = if let Some(lower) = lower {
            ConfigureWindowAux::new()
                .sibling(self.frame_window(lower))
                .stack_mode(StackMode::ABOVE)
        } else if let Some(higher) = higher {
            ConfigureWindowAux::new()
                .sibling(self.frame_window(higher))
                .stack_mode(StackMode::BELOW)
        } else {
            return Ok(());
        };
        self.connection
            .configure_window(self.frame_window(id), &values)?;
        self.sync_stacking_from_server()?;
        self.enforce_layers()
    }

    fn session_snapshot(&self) -> SessionSnapshot {
        let clients = self
            .clients
            .stacking()
            .enumerate()
            .filter_map(|(stacking_index, id)| {
                let identity = self.session_identities.get(&id)?.clone();
                let client = self.clients.get(id).copied()?;
                let geometry = client.unmanaged_geometry();
                Some(session::SessionClient {
                    identity,
                    x: geometry.x,
                    y: geometry.y,
                    width: geometry.width,
                    height: geometry.height,
                    workspace: match client.workspace {
                        WorkspaceAssignment::Workspace(workspace) => Some(workspace.index()),
                        WorkspaceAssignment::All => None,
                    },
                    iconic: client.iconic,
                    shaded: client.shaded,
                    skip_taskbar: client.presentation.skip_taskbar,
                    skip_pager: client.presentation.skip_pager,
                    fullscreen: client.fullscreen.is_some(),
                    maximized_horizontal: client
                        .maximize
                        .is_some_and(|maximize| maximize.horizontal),
                    maximized_vertical: client.maximize.is_some_and(|maximize| maximize.vertical),
                    layer: session_client_layer(client.layer),
                    decoration_override: session_client_decoration_override(
                        client.decoration_override,
                    ),
                    focused: self.clients.focused() == Some(id),
                    stacking_index: u32::try_from(stacking_index).unwrap_or(u32::MAX),
                })
            })
            .collect();
        SessionSnapshot::new(self.clients.current_workspace().index(), clients)
    }

    fn release_client_for_shutdown(&mut self, id: ClientId) -> Result<(), X11Error> {
        let Some(client) = self.clients.get(id).copied() else {
            return Ok(());
        };
        let Some(frame) = self.frames.remove(&id) else {
            return Ok(());
        };
        let window = window_id(id);
        let geometry = client.unmanaged_geometry();
        let _ = window_request_succeeded(
            self.connection
                .change_window_attributes(
                    window,
                    &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
                )?
                .check(),
        )?;
        let _ = window_request_succeeded(self.connection.unmap_window(frame.window)?.check())?;
        let exists = window_request_succeeded(
            self.connection
                .reparent_window(
                    window,
                    self.root,
                    clamp_i16(geometry.x),
                    clamp_i16(geometry.y),
                )?
                .check(),
        )?;
        if exists {
            let _ = window_request_succeeded(
                self.connection
                    .change_save_set(SetMode::DELETE, window)?
                    .check(),
            )?;
            let _ = window_request_succeeded(
                self.connection
                    .configure_window(
                        window,
                        &ConfigureWindowAux::new()
                            .x(geometry.x)
                            .y(geometry.y)
                            .width(geometry.width)
                            .height(geometry.height)
                            .border_width(u32::from(frame.original_border_width)),
                    )?
                    .check(),
            )?;
            for property in [
                self.atoms._NET_FRAME_EXTENTS,
                self.atoms._NET_WM_ALLOWED_ACTIONS,
                self.atoms._NET_WM_VISIBLE_NAME,
            ] {
                let _ = window_request_succeeded(
                    self.connection.delete_property(window, property)?.check(),
                )?;
            }
            let _ = window_request_succeeded(self.connection.map_window(window)?.check())?;
        }
        self.connection.destroy_window(frame.window)?;
        Ok(())
    }
}

impl Drop for WindowManager {
    fn drop(&mut self) {
        let _ = self.finish_drag(self.last_timestamp);
        let clients: Vec<ClientId> = self.clients.management_order().collect();
        for id in clients {
            let _ = self.release_client_for_shutdown(id);
        }
        self.runtime_timer.stop();
        let _ = self
            .connection
            .ungrab_key(Grab::ANY, self.root, ModMask::ANY);
        let _ = self
            .connection
            .ungrab_button(ButtonIndex::ANY, self.root, ModMask::ANY);
        let _ = self.connection.ungrab_keyboard(CURRENT_TIME);
        let _ = self.connection.ungrab_pointer(CURRENT_TIME);
        let _ = self.connection.unmap_window(self.focus_overlay.window);
        let _ = self.connection.destroy_window(self.focus_overlay.window);
        let _ = self.connection.unmap_window(self.menu_overlay.window);
        let _ = self.connection.destroy_window(self.menu_overlay.window);
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
            .delete_property(self.root, self.atoms._NET_SHOWING_DESKTOP);
        let _ = self
            .connection
            .delete_property(self.root, self.atoms._NET_WORKAREA);
        let _ = self.connection.free_gc(self.title_gc);
        let _ = self.connection.close_font(self.title_font);
        let _ = self.connection.change_window_attributes(
            self.root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
        );
        let _ = self.connection.destroy_window(self.support_window);
        let _ = self.connection.flush();
    }
}

#[derive(Clone, Copy)]
struct DecorationPixels {
    active_border: u32,
    inactive_border: u32,
    urgent_border: u32,
    active_titlebar: u32,
    inactive_titlebar: u32,
    urgent_titlebar: u32,
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
            theme.urgent_border,
            theme.active_titlebar,
            theme.inactive_titlebar,
            theme.urgent_titlebar,
            theme.title_text,
            theme.minimize_button,
            theme.maximize_button,
            theme.close_button,
        ];
        let mut pixels = [0; 10];
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
            urgent_border: pixels[2],
            active_titlebar: pixels[3],
            inactive_titlebar: pixels[4],
            urgent_titlebar: pixels[5],
            title_text: pixels[6],
            minimize_button: pixels[7],
            maximize_button: pixels[8],
            close_button: pixels[9],
        })
    }

    const fn as_array(self) -> [u32; 10] {
        [
            self.active_border,
            self.inactive_border,
            self.urgent_border,
            self.active_titlebar,
            self.inactive_titlebar,
            self.urgent_titlebar,
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
    positioned: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GeometryRequest {
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
    gravity: Gravity,
}

#[derive(Clone, Copy, Debug)]
struct Relationships {
    transient_for: Option<TransientTarget>,
    group: Option<ClientId>,
    modal: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ClientIcon {
    width: u32,
    height: u32,
    argb: Vec<u32>,
}

#[derive(Debug, Eq, PartialEq)]
struct X11ApplicationIdentity {
    name: String,
    class: String,
    role: String,
    title: String,
    kind: ApplicationKind,
}

impl X11ApplicationIdentity {
    fn as_application_identity(&self) -> ApplicationIdentity<'_> {
        ApplicationIdentity {
            name: &self.name,
            class: &self.class,
            role: &self.role,
            title: &self.title,
            kind: self.kind,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct MotifHints {
    flags: u32,
    functions: u32,
    decorations: u32,
}

struct FocusCycle {
    candidates: Vec<ClientId>,
    index: Option<usize>,
    original: Option<ClientId>,
    modifiers: u16,
    keyboard_grabbed: bool,
}

#[derive(Clone, Copy)]
struct FocusOverlay {
    window: Window,
    width: u32,
    height: u32,
    mapped: bool,
}

#[derive(Clone, Copy)]
struct MenuOverlay {
    window: Window,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    mapped: bool,
}

struct MenuSession {
    menu: RuntimeMenu,
    parents: Vec<(RuntimeMenu, usize)>,
    selected: usize,
    target: Option<ClientId>,
    anchor_x: i32,
    anchor_y: i32,
    centered: bool,
    opening_button: Option<u8>,
    pending_key: Option<(u8, RuntimeMenuAction, Option<ClientId>)>,
    keyboard_grabbed: bool,
    pointer_grabbed: bool,
}

struct RuntimeMenu {
    id: String,
    title: String,
    entries: Vec<RuntimeMenuEntry>,
}

#[derive(Clone)]
enum RuntimeMenuEntry {
    Item {
        label: String,
        accelerator: Option<char>,
        action: RuntimeMenuAction,
        target: Option<ClientId>,
    },
    Submenu {
        label: String,
        accelerator: Option<char>,
        menu: String,
    },
    Separator {
        label: Option<String>,
    },
}

#[derive(Clone)]
enum RuntimeMenuAction {
    Configured(Vec<Action>),
    ActivateClient(ClientId),
}

#[derive(Default)]
struct MenuKeycodes {
    up: Vec<u8>,
    down: Vec<u8>,
    left: Vec<u8>,
    right: Vec<u8>,
    home: Vec<u8>,
    end: Vec<u8>,
    enter: Vec<u8>,
    characters: BTreeMap<u8, char>,
}

#[derive(Clone, Copy, Debug)]
enum FocusCycleDirection {
    Next,
    Previous,
}

#[derive(Clone, Copy)]
struct Drag {
    window: Window,
    kind: DragKind,
    button: Option<u8>,
    pointer_x: i16,
    pointer_y: i16,
    initial: Geometry,
    sync: Option<SyncResize>,
    keyboard: bool,
    keyboard_resize_edge: Option<KeyboardDragDirection>,
    pointer_grabbed: bool,
}

#[derive(Clone, Copy)]
struct DragStart {
    kind: DragKind,
    pointer_x: i16,
    pointer_y: i16,
    button: Option<u8>,
    keyboard: bool,
    grab_pointer: bool,
    timestamp: u32,
}

#[derive(Clone, Copy)]
struct ResizeDragRequest {
    initial: Geometry,
    edges: ResizeEdges,
    dx: i32,
    dy: i32,
    bounds: Geometry,
    resistance: u32,
}

#[derive(Clone, Copy)]
struct SyncResize {
    alarm: Alarm,
    sequence: u64,
    waiting: bool,
    timeout_generation: u32,
    pending: Option<Geometry>,
}

#[derive(Clone, Copy)]
enum DragKind {
    Move,
    Resize(ResizeEdges),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NetWmMoveResizeRequest {
    Resize(ResizeEdges),
    Move,
    ResizeKeyboard,
    MoveKeyboard,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyboardDragAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyboardDragDirection {
    Left,
    Right,
    Up,
    Down,
}

impl KeyboardDragDirection {
    const fn axis(self) -> KeyboardDragAxis {
        match self {
            Self::Left | Self::Right => KeyboardDragAxis::Horizontal,
            Self::Up | Self::Down => KeyboardDragAxis::Vertical,
        }
    }

    const fn resize_edges(self) -> ResizeEdges {
        match self {
            Self::Left => ResizeEdges::new(true, false, false, false),
            Self::Right => ResizeEdges::new(false, true, false, false),
            Self::Up => ResizeEdges::new(false, false, true, false),
            Self::Down => ResizeEdges::new(false, false, false, true),
        }
    }

    fn delta(self, step: u32) -> (i32, i32) {
        let step = i32::try_from(step).unwrap_or(i32::MAX);
        match self {
            Self::Left => (-step, 0),
            Self::Right => (step, 0),
            Self::Up => (0, -step),
            Self::Down => (0, step),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MouseBindingKey {
    context: MouseContext,
    button: u8,
    modifiers: u16,
    trigger: MouseTrigger,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MouseTarget {
    client: Option<ClientId>,
    context: MouseContext,
    window: Window,
}

#[derive(Clone, Copy, Debug)]
struct MouseGesture {
    target: MouseTarget,
    button: u8,
    modifiers: u16,
    root_x: i16,
    root_y: i16,
    dragged: bool,
}

#[derive(Clone, Copy, Debug)]
struct MouseClick {
    target: MouseTarget,
    button: u8,
    modifiers: u16,
    root_x: i16,
    root_y: i16,
    timestamp: u32,
}

#[derive(Clone, Copy, Debug)]
struct PointerInvocation {
    target: MouseTarget,
    button: u8,
    root_x: i16,
    root_y: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResizeEdges {
    left: bool,
    right: bool,
    top: bool,
    bottom: bool,
}

impl ResizeEdges {
    const fn new(left: bool, right: bool, top: bool, bottom: bool) -> Self {
        Self {
            left,
            right,
            top,
            bottom,
        }
    }

    const fn bottom_right() -> Self {
        Self::new(false, true, false, true)
    }

    fn nearest(geometry: Geometry, root_x: i16, root_y: i16) -> Self {
        let horizontal = i32::from(root_x)
            < geometry
                .x
                .saturating_add(i32::try_from(geometry.width / 2).unwrap_or(i32::MAX));
        let vertical = i32::from(root_y)
            < geometry
                .y
                .saturating_add(i32::try_from(geometry.height / 2).unwrap_or(i32::MAX));
        Self::new(horizontal, !horizontal, vertical, !vertical)
    }
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

fn x11_text(value: &[u8]) -> String {
    value.iter().copied().map(char::from).collect()
}

fn parse_wm_class(value: &[u8]) -> (String, String) {
    let mut fields = value.split(|byte| *byte == 0);
    let name = fields.next().map_or_else(String::new, x11_text);
    let class = fields.next().map_or_else(String::new, x11_text);
    (name, class)
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

fn output_reservations(
    reservations: EdgeReservations,
    output: Geometry,
    root: Geometry,
) -> EdgeReservations {
    let start_depth = |depth: u32, root_start: i32, output_start: i32| {
        if depth == 0 {
            return 0;
        }
        let boundary = i64::from(root_start).saturating_add(i64::from(depth));
        positive_u32(boundary.saturating_sub(i64::from(output_start)))
    };
    let end_depth = |depth: u32, root_start: i32, root_size: u32, output_end: i64| {
        if depth == 0 {
            return 0;
        }
        let root_end = i64::from(root_start).saturating_add(i64::from(root_size));
        let boundary = root_end.saturating_sub(i64::from(depth));
        positive_u32(output_end.saturating_sub(boundary))
    };
    EdgeReservations {
        left: EdgeReservation {
            depth: start_depth(reservations.left.depth, root.x, output.x),
            ..reservations.left
        },
        right: EdgeReservation {
            depth: end_depth(
                reservations.right.depth,
                root.x,
                root.width,
                i64::from(output.x).saturating_add(i64::from(output.width)),
            ),
            ..reservations.right
        },
        top: EdgeReservation {
            depth: start_depth(reservations.top.depth, root.y, output.y),
            ..reservations.top
        },
        bottom: EdgeReservation {
            depth: end_depth(
                reservations.bottom.depth,
                root.y,
                root.height,
                i64::from(output.y).saturating_add(i64::from(output.height)),
            ),
            ..reservations.bottom
        },
    }
}

fn positive_u32(value: i64) -> u32 {
    if value <= 0 {
        0
    } else {
        u32::try_from(value).unwrap_or(u32::MAX)
    }
}

fn runtime_request(request: u32, value: u32, extra: u32) -> Option<RuntimeRequest> {
    match request {
        CONTROL_RELOAD => Some(RuntimeRequest::Reload),
        CONTROL_SHUTDOWN => Some(RuntimeRequest::Shutdown),
        CONTROL_KEY_CHAIN_TIMEOUT => Some(RuntimeRequest::KeyChainTimeout(value)),
        CONTROL_PING_TIMEOUT => Some(RuntimeRequest::PingTimeout {
            client: client_id(value),
            generation: extra,
        }),
        CONTROL_SYNC_RESIZE_TIMEOUT => Some(RuntimeRequest::SyncResizeTimeout {
            client: client_id(value),
            generation: extra,
        }),
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

fn apply_size_capabilities(mut policy: ClientPolicy, hints: SizeHints) -> ClientPolicy {
    let resizable = match (hints.minimum, hints.maximum) {
        (Some(minimum), Some(maximum)) => {
            minimum.width < maximum.width.max(minimum.width)
                || minimum.height < maximum.height.max(minimum.height)
        }
        _ => true,
    };
    if !resizable {
        policy.capabilities.resizable = false;
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

const fn showing_desktop_request(value: u32) -> Option<bool> {
    match value {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

fn parse_client_icon(values: &[u32], preferred_size: u32) -> Option<ClientIcon> {
    let mut cursor = 0_usize;
    let mut best_dimensions = None;
    let mut best_pixels = None;
    let mut best_score = None;
    while cursor.checked_add(2).is_some_and(|end| end <= values.len()) {
        let width = values[cursor];
        let height = values[cursor + 1];
        cursor += 2;
        let pixel_count = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|count| usize::try_from(count).ok());
        let Some(pixel_count) = pixel_count else {
            break;
        };
        let Some(end) = cursor.checked_add(pixel_count) else {
            break;
        };
        if end > values.len() {
            break;
        }
        if width > 0
            && height > 0
            && width <= MAX_CLIENT_ICON_DIMENSION
            && height <= MAX_CLIENT_ICON_DIMENSION
        {
            let size = width.max(height);
            let score = (
                size.abs_diff(preferred_size),
                size < preferred_size,
                u32::MAX - size,
            );
            if best_score.is_none_or(|current| score < current) {
                best_dimensions = Some((width, height));
                best_pixels = Some(cursor..end);
                best_score = Some(score);
            }
        }
        cursor = end;
    }
    let ((width, height), pixels) = best_dimensions.zip(best_pixels)?;
    Some(ClientIcon {
        width,
        height,
        argb: values[pixels].to_vec(),
    })
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

const fn role_occupies_placement_space(role: ClientRole) -> bool {
    matches!(
        role,
        ClientRole::Normal
            | ClientRole::Dialog
            | ClientRole::Utility
            | ClientRole::Toolbar
            | ClientRole::Menu
            | ClientRole::Splash
    )
}

const fn application_kind(role: ClientRole) -> ApplicationKind {
    match role {
        ClientRole::Normal => ApplicationKind::Normal,
        ClientRole::Dialog => ApplicationKind::Dialog,
        ClientRole::Utility => ApplicationKind::Utility,
        ClientRole::Toolbar => ApplicationKind::Toolbar,
        ClientRole::Menu => ApplicationKind::Menu,
        ClientRole::Splash => ApplicationKind::Splash,
        ClientRole::Desktop => ApplicationKind::Desktop,
        ClientRole::Dock => ApplicationKind::Dock,
        ClientRole::DropdownMenu => ApplicationKind::DropdownMenu,
        ClientRole::PopupMenu => ApplicationKind::PopupMenu,
        ClientRole::Tooltip => ApplicationKind::Tooltip,
        ClientRole::Notification => ApplicationKind::Notification,
        ClientRole::Combo => ApplicationKind::Combo,
        ClientRole::DragAndDrop => ApplicationKind::DragAndDrop,
    }
}

const fn application_kind_name(kind: ApplicationKind) -> &'static str {
    match kind {
        ApplicationKind::Normal => "normal",
        ApplicationKind::Dialog => "dialog",
        ApplicationKind::Utility => "utility",
        ApplicationKind::Toolbar => "toolbar",
        ApplicationKind::Menu => "menu",
        ApplicationKind::Splash => "splash",
        ApplicationKind::Desktop => "desktop",
        ApplicationKind::Dock => "dock",
        ApplicationKind::DropdownMenu => "dropdown_menu",
        ApplicationKind::PopupMenu => "popup_menu",
        ApplicationKind::Tooltip => "tooltip",
        ApplicationKind::Notification => "notification",
        ApplicationKind::Combo => "combo",
        ApplicationKind::DragAndDrop => "drag_and_drop",
    }
}

const fn application_layer(layer: ApplicationLayer) -> ClientLayer {
    match layer {
        ApplicationLayer::Below => ClientLayer::Below,
        ApplicationLayer::Normal => ClientLayer::Normal,
        ApplicationLayer::Above => ClientLayer::Above,
    }
}

const fn session_layer(layer: session::SessionLayer) -> ClientLayer {
    match layer {
        session::SessionLayer::Below => ClientLayer::Below,
        session::SessionLayer::Normal => ClientLayer::Normal,
        session::SessionLayer::Above => ClientLayer::Above,
    }
}

const fn session_client_layer(layer: ClientLayer) -> session::SessionLayer {
    match layer {
        ClientLayer::Below => session::SessionLayer::Below,
        ClientLayer::Normal => session::SessionLayer::Normal,
        ClientLayer::Above => session::SessionLayer::Above,
    }
}

const fn session_decoration_override(
    preference: session::SessionDecorationOverride,
) -> DecorationOverride {
    match preference {
        session::SessionDecorationOverride::Default => DecorationOverride::Default,
        session::SessionDecorationOverride::Decorated => DecorationOverride::Decorated,
        session::SessionDecorationOverride::Undecorated => DecorationOverride::Undecorated,
    }
}

const fn session_client_decoration_override(
    preference: DecorationOverride,
) -> session::SessionDecorationOverride {
    match preference {
        DecorationOverride::Default => session::SessionDecorationOverride::Default,
        DecorationOverride::Decorated => session::SessionDecorationOverride::Decorated,
        DecorationOverride::Undecorated => session::SessionDecorationOverride::Undecorated,
    }
}

fn apply_application_decorations(
    mut policy: ClientPolicy,
    decorated: Option<bool>,
) -> ClientPolicy {
    match decorated {
        Some(true) => {
            policy.decorations = ClientPolicy::for_role(policy.role).decorations;
        }
        Some(false) => {
            policy.decorations = ClientDecorations {
                border: false,
                titlebar: false,
                minimize: false,
                maximize: false,
                close: false,
            };
        }
        None => {}
    }
    policy
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

fn colormap_request_succeeded(result: Result<(), ReplyError>) -> Result<bool, X11Error> {
    match result {
        Ok(()) => Ok(true),
        Err(ReplyError::X11Error(error)) if error.error_kind == ErrorKind::Colormap => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn sync_request_succeeded(result: Result<(), ReplyError>) -> Result<bool, X11Error> {
    match result {
        Ok(()) => Ok(true),
        Err(ReplyError::X11Error(error))
            if error.error_kind == ErrorKind::SyncCounter
                || error.error_kind == ErrorKind::SyncAlarm =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

fn sync_value(value: u64) -> SyncInt64 {
    SyncInt64 {
        hi: i32::try_from(value >> 32).expect("synchronized resize values stay positive"),
        lo: u32::try_from(value & u64::from(u32::MAX)).expect("masked low word"),
    }
}

fn sync_value_u64(value: SyncInt64) -> Option<u64> {
    u32::try_from(value.hi)
        .ok()
        .map(|hi| (u64::from(hi) << 32) | u64::from(value.lo))
}

fn prioritized_colormap_windows(top_level: Window, listed: &[Window]) -> Vec<Window> {
    let mut windows = Vec::with_capacity(listed.len().min(MAX_CLIENT_COLORMAP_WINDOWS));
    let mut seen = BTreeSet::new();
    for window in listed.iter().copied().filter(|window| *window != NONE) {
        if seen.insert(window) {
            windows.push(window);
            if windows.len() == MAX_CLIENT_COLORMAP_WINDOWS {
                break;
            }
        }
    }
    if !seen.contains(&top_level) {
        windows.insert(0, top_level);
        windows.truncate(MAX_CLIENT_COLORMAP_WINDOWS);
    }
    windows
}

const fn net_wm_moveresize_request(value: u32) -> Option<NetWmMoveResizeRequest> {
    match value {
        0 => Some(NetWmMoveResizeRequest::Resize(ResizeEdges::new(
            true, false, true, false,
        ))),
        1 => Some(NetWmMoveResizeRequest::Resize(ResizeEdges::new(
            false, false, true, false,
        ))),
        2 => Some(NetWmMoveResizeRequest::Resize(ResizeEdges::new(
            false, true, true, false,
        ))),
        3 => Some(NetWmMoveResizeRequest::Resize(ResizeEdges::new(
            false, true, false, false,
        ))),
        4 => Some(NetWmMoveResizeRequest::Resize(ResizeEdges::new(
            false, true, false, true,
        ))),
        5 => Some(NetWmMoveResizeRequest::Resize(ResizeEdges::new(
            false, false, false, true,
        ))),
        6 => Some(NetWmMoveResizeRequest::Resize(ResizeEdges::new(
            true, false, false, true,
        ))),
        7 => Some(NetWmMoveResizeRequest::Resize(ResizeEdges::new(
            true, false, false, false,
        ))),
        8 => Some(NetWmMoveResizeRequest::Move),
        9 => Some(NetWmMoveResizeRequest::ResizeKeyboard),
        10 => Some(NetWmMoveResizeRequest::MoveKeyboard),
        11 => Some(NetWmMoveResizeRequest::Cancel),
        _ => None,
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

fn keyboard_move_geometry(
    initial: Geometry,
    bounds: Geometry,
    direction: KeyboardDragDirection,
    step: u32,
    edge: bool,
) -> Geometry {
    let step = i32::try_from(step).unwrap_or(i32::MAX);
    let right = geometry_end(bounds.x, bounds.width)
        .saturating_sub(i32::try_from(initial.width).unwrap_or(i32::MAX));
    let bottom = geometry_end(bounds.y, bounds.height)
        .saturating_sub(i32::try_from(initial.height).unwrap_or(i32::MAX));
    let (x, y) = match (direction, edge) {
        (KeyboardDragDirection::Left, true) => (bounds.x, initial.y),
        (KeyboardDragDirection::Right, true) => (right, initial.y),
        (KeyboardDragDirection::Up, true) => (initial.x, bounds.y),
        (KeyboardDragDirection::Down, true) => (initial.x, bottom),
        (KeyboardDragDirection::Left, false) => (initial.x.saturating_sub(step), initial.y),
        (KeyboardDragDirection::Right, false) => (initial.x.saturating_add(step), initial.y),
        (KeyboardDragDirection::Up, false) => (initial.x, initial.y.saturating_sub(step)),
        (KeyboardDragDirection::Down, false) => (initial.x, initial.y.saturating_add(step)),
    };
    Geometry::new(x, y, initial.width, initial.height).clamp_position(bounds)
}

fn resize_from_edges(
    initial: Geometry,
    edges: ResizeEdges,
    dx: i32,
    dy: i32,
    bounds: Geometry,
    resistance: u32,
) -> Geometry {
    let mut left = i64::from(initial.x);
    let mut right = i64::from(geometry_end(initial.x, initial.width));
    let mut top = i64::from(initial.y);
    let mut bottom = i64::from(geometry_end(initial.y, initial.height));
    if edges.left {
        left = left.saturating_add(i64::from(dx));
    }
    if edges.right {
        right = right.saturating_add(i64::from(dx));
    }
    if edges.top {
        top = top.saturating_add(i64::from(dy));
    }
    if edges.bottom {
        bottom = bottom.saturating_add(i64::from(dy));
    }
    let resistance = i64::from(resistance);
    let bounds_left = i64::from(bounds.x);
    let bounds_right = i64::from(geometry_end(bounds.x, bounds.width));
    let bounds_top = i64::from(bounds.y);
    let bounds_bottom = i64::from(geometry_end(bounds.y, bounds.height));
    if edges.left && left.abs_diff(bounds_left) <= u64::try_from(resistance).unwrap_or(u64::MAX) {
        left = bounds_left;
    }
    if edges.right && right.abs_diff(bounds_right) <= u64::try_from(resistance).unwrap_or(u64::MAX)
    {
        right = bounds_right;
    }
    if edges.top && top.abs_diff(bounds_top) <= u64::try_from(resistance).unwrap_or(u64::MAX) {
        top = bounds_top;
    }
    if edges.bottom
        && bottom.abs_diff(bounds_bottom) <= u64::try_from(resistance).unwrap_or(u64::MAX)
    {
        bottom = bounds_bottom;
    }
    let width = u32::try_from(right.saturating_sub(left).max(1)).unwrap_or(u32::MAX);
    let height = u32::try_from(bottom.saturating_sub(top).max(1)).unwrap_or(u32::MAX);
    Geometry::new(
        i32::try_from(left).unwrap_or(if left.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        }),
        i32::try_from(top).unwrap_or(if top.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        }),
        width,
        height,
    )
}

fn geometry_end(start: i32, extent: u32) -> i32 {
    start.saturating_add(i32::try_from(extent).unwrap_or(i32::MAX))
}

fn centered_axis(origin: i32, extent: u32, child: u32) -> i32 {
    origin.saturating_add(i32::try_from(extent.saturating_sub(child) / 2).unwrap_or(i32::MAX))
}

fn place_popup_axis(preferred: i32, origin: i32, extent: u32, child: u32) -> i32 {
    let end = geometry_end(origin, extent);
    let child = child.min(extent);
    let child_i32 = i32::try_from(child).unwrap_or(i32::MAX);
    if preferred.saturating_add(child_i32) <= end {
        return preferred.max(origin);
    }
    if preferred.saturating_sub(child_i32) >= origin {
        return preferred.saturating_sub(child_i32);
    }
    let last = end.saturating_sub(child_i32);
    preferred.clamp(origin, last.max(origin))
}

fn runtime_configured_entry(entry: &MenuEntry) -> RuntimeMenuEntry {
    match entry {
        MenuEntry::Item { label, actions } => {
            let (label, accelerator) = menu_label(label);
            RuntimeMenuEntry::Item {
                label,
                accelerator,
                action: RuntimeMenuAction::Configured(actions.clone()),
                target: None,
            }
        }
        MenuEntry::Submenu { label, menu } => {
            let (label, accelerator) = menu_label(label);
            RuntimeMenuEntry::Submenu {
                label,
                accelerator,
                menu: menu.clone(),
            }
        }
        MenuEntry::Separator { label } => RuntimeMenuEntry::Separator {
            label: label.clone(),
        },
    }
}

fn runtime_action(label: &str, action: Action, target: ClientId) -> RuntimeMenuEntry {
    let (label, accelerator) = menu_label(label);
    RuntimeMenuEntry::Item {
        label,
        accelerator,
        action: RuntimeMenuAction::Configured(vec![action]),
        target: Some(target),
    }
}

fn runtime_submenu(label: &str, menu: &str) -> RuntimeMenuEntry {
    let (label, accelerator) = menu_label(label);
    RuntimeMenuEntry::Submenu {
        label,
        accelerator,
        menu: menu.to_owned(),
    }
}

fn runtime_client_activation(label: String, target: ClientId) -> RuntimeMenuEntry {
    RuntimeMenuEntry::Item {
        label,
        accelerator: None,
        action: RuntimeMenuAction::ActivateClient(target),
        target: Some(target),
    }
}

fn workspace_menu_label(workspace: u32, name: &str) -> String {
    if workspace <= 9 {
        format!("_{workspace}  {name}")
    } else {
        format!("{workspace}  {name}")
    }
}

fn menu_label(label: &str) -> (String, Option<char>) {
    let mut rendered = String::with_capacity(label.len());
    let mut accelerator = None;
    let mut characters = label.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '_' {
            rendered.push(character);
            continue;
        }
        let Some(next) = characters.next() else {
            rendered.push('_');
            break;
        };
        if next == '_' {
            rendered.push('_');
        } else {
            if accelerator.is_none() {
                accelerator = Some(lowercase_character(next));
            } else {
                rendered.push('_');
            }
            rendered.push(next);
        }
    }
    (rendered, accelerator)
}

fn lowercase_character(character: char) -> char {
    character.to_lowercase().next().unwrap_or(character)
}

fn menu_entry_is_selectable(entry: &RuntimeMenuEntry) -> bool {
    matches!(
        entry,
        RuntimeMenuEntry::Item { .. } | RuntimeMenuEntry::Submenu { .. }
    )
}

fn first_selectable_menu_entry(entries: &[RuntimeMenuEntry]) -> Option<usize> {
    entries.iter().position(menu_entry_is_selectable)
}

fn last_selectable_menu_entry(entries: &[RuntimeMenuEntry]) -> Option<usize> {
    entries.iter().rposition(menu_entry_is_selectable)
}

fn next_selectable_menu_entry(
    entries: &[RuntimeMenuEntry],
    current: usize,
    forward: bool,
) -> Option<usize> {
    if entries.is_empty() {
        return None;
    }
    for offset in 1..=entries.len() {
        let index = if forward {
            current.wrapping_add(offset) % entries.len()
        } else {
            current
                .wrapping_add(entries.len())
                .wrapping_sub(offset % entries.len())
                % entries.len()
        };
        if menu_entry_is_selectable(&entries[index]) {
            return Some(index);
        }
    }
    None
}

fn accelerator_menu_entry(
    entries: &[RuntimeMenuEntry],
    current: usize,
    accelerator: char,
) -> Option<(usize, usize)> {
    if entries.is_empty() {
        return None;
    }
    let mut selected = None;
    let mut matches = 0;
    for offset in 1..=entries.len() {
        let index = current.wrapping_add(offset) % entries.len();
        let candidate = match &entries[index] {
            RuntimeMenuEntry::Item { accelerator, .. }
            | RuntimeMenuEntry::Submenu { accelerator, .. } => *accelerator,
            RuntimeMenuEntry::Separator { .. } => None,
        };
        if candidate == Some(accelerator) {
            selected.get_or_insert(index);
            matches += 1;
        }
    }
    selected.map(|selected| (selected, matches))
}

fn focus_cycle_visible_start(total: usize, selected: usize, rows: usize) -> usize {
    if total <= rows || rows == 0 {
        return 0;
    }
    selected.saturating_sub(rows / 2).min(total - rows)
}

fn point_inside_window(x: i16, y: i16, width: u32, height: u32, border: u32) -> bool {
    let x = i32::from(x);
    let y = i32::from(y);
    let border = i32::try_from(border).unwrap_or(i32::MAX);
    let width = i32::try_from(width).unwrap_or(i32::MAX);
    let height = i32::try_from(height).unwrap_or(i32::MAX);
    x >= -border
        && y >= -border
        && x < width.saturating_add(border)
        && y < height.saturating_add(border)
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

fn ewmh_gravity(value: u32) -> Option<Gravity> {
    match value {
        1 => Some(Gravity::NorthWest),
        2 => Some(Gravity::North),
        3 => Some(Gravity::NorthEast),
        4 => Some(Gravity::West),
        5 => Some(Gravity::Center),
        6 => Some(Gravity::East),
        7 => Some(Gravity::SouthWest),
        8 => Some(Gravity::South),
        9 => Some(Gravity::SouthEast),
        10 => Some(Gravity::Static),
        _ => None,
    }
}

fn signed_cardinal(value: u32) -> i32 {
    i32::from_ne_bytes(value.to_ne_bytes())
}

fn legacy_output_coverage(
    geometry: Geometry,
    policy: ClientPolicy,
    maximized: bool,
    fullscreen: bool,
    outputs: &OutputSet,
    root: Geometry,
) -> Option<OutputCoverage> {
    if maximized
        || fullscreen
        || matches!(policy.role, ClientRole::Desktop | ClientRole::Dock)
        || policy.decorations.border
        || policy.decorations.titlebar
        || (geometry != root
            && !outputs
                .outputs()
                .iter()
                .any(|output| output.geometry == geometry))
    {
        return None;
    }
    Some(OutputCoverage::new(outputs.output_for(geometry).id))
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

fn focus_mode_changes_ownership(mode: NotifyMode) -> bool {
    mode != NotifyMode::GRAB && mode != NotifyMode::UNGRAB
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

fn mouse_modifier_mask(state: u16) -> u16 {
    let supported = u16::from(ModMask::CONTROL)
        | u16::from(ModMask::M1)
        | u16::from(ModMask::SHIFT)
        | u16::from(ModMask::M4);
    state & supported
}

fn mouse_context_chain(context: MouseContext) -> &'static [MouseContext] {
    match context {
        MouseContext::Root => &[MouseContext::Root, MouseContext::Desktop],
        MouseContext::Desktop => &[MouseContext::Desktop],
        MouseContext::Client => &[MouseContext::Client, MouseContext::Frame],
        MouseContext::Frame => &[MouseContext::Frame],
        MouseContext::Titlebar => &[MouseContext::Titlebar, MouseContext::Frame],
        MouseContext::Border => &[MouseContext::Border, MouseContext::Frame],
        MouseContext::Top => &[MouseContext::Top, MouseContext::Border, MouseContext::Frame],
        MouseContext::Bottom => &[
            MouseContext::Bottom,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::Left => &[
            MouseContext::Left,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::Right => &[
            MouseContext::Right,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::TopLeft => &[
            MouseContext::TopLeft,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::TopRight => &[
            MouseContext::TopRight,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::BottomLeft => &[
            MouseContext::BottomLeft,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::BottomRight => &[
            MouseContext::BottomRight,
            MouseContext::Border,
            MouseContext::Frame,
        ],
        MouseContext::Minimize => &[
            MouseContext::Minimize,
            MouseContext::Titlebar,
            MouseContext::Frame,
        ],
        MouseContext::Maximize => &[
            MouseContext::Maximize,
            MouseContext::Titlebar,
            MouseContext::Frame,
        ],
        MouseContext::Close => &[
            MouseContext::Close,
            MouseContext::Titlebar,
            MouseContext::Frame,
        ],
    }
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

fn insert_key_binding_variants(
    node: &mut KeyBindingNode,
    sequence: &[Vec<KeyInput>],
    actions: &[Action],
) -> Result<(), X11Error> {
    let Some((inputs, remaining)) = sequence.split_first() else {
        if !node.actions.is_empty() || !node.children.is_empty() {
            return Err(X11Error::ConflictingKeyGrab);
        }
        node.actions.extend_from_slice(actions);
        return Ok(());
    };
    if !node.actions.is_empty() {
        return Err(X11Error::ConflictingKeyGrab);
    }
    for input in inputs {
        insert_key_binding_variants(node.children.entry(*input).or_default(), remaining, actions)?;
    }
    Ok(())
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

fn query_randr_version(connection: &RustConnection) -> Result<Option<(u32, u32)>, X11Error> {
    if connection
        .extension_information(x11rb::protocol::randr::X11_EXTENSION_NAME)?
        .is_none()
    {
        return Ok(None);
    }
    let version = connection.randr_query_version(1, 5)?.reply()?;
    Ok(Some((version.major_version, version.minor_version)))
}

fn fullscreen_monitor_geometry(
    outputs: &OutputSet,
    indices: FullscreenMonitorIndices,
) -> Option<Geometry> {
    let output = |index: u32| outputs.outputs().get(usize::try_from(index).ok()?).copied();
    let top = output(indices.top)?.geometry;
    let bottom = output(indices.bottom)?.geometry;
    let left = output(indices.left)?.geometry;
    let right = output(indices.right)?.geometry;
    let x = i64::from(left.x);
    let y = i64::from(top.y);
    let right_edge = i64::from(right.x).checked_add(i64::from(right.width))?;
    let bottom_edge = i64::from(bottom.y).checked_add(i64::from(bottom.height))?;
    let width = u32::try_from(right_edge.checked_sub(x)?).ok()?;
    let height = u32::try_from(bottom_edge.checked_sub(y)?).ok()?;
    (width > 0 && height > 0).then(|| Geometry::new(left.x, top.y, width, height))
}

fn query_shape_version(connection: &RustConnection) -> Result<Option<(u16, u16)>, X11Error> {
    if connection
        .extension_information(x11rb::protocol::shape::X11_EXTENSION_NAME)?
        .is_none()
    {
        return Ok(None);
    }
    let version = connection.shape_query_version()?.reply()?;
    Ok(Some((version.major_version, version.minor_version)))
}

fn query_sync_version(connection: &RustConnection) -> Result<Option<(u8, u8)>, X11Error> {
    if connection
        .extension_information(sync::X11_EXTENSION_NAME)?
        .is_none()
    {
        return Ok(None);
    }
    let version = connection.sync_initialize(3, 1)?.reply()?;
    Ok(Some((version.major_version, version.minor_version)))
}

fn discover_outputs(
    connection: &RustConnection,
    root: Window,
    fallback: Geometry,
    randr_version: Option<(u32, u32)>,
) -> Result<OutputSet, X11Error> {
    let Some(version) = randr_version else {
        return Ok(root_output(root, fallback));
    };
    if !version_at_least(version, (1, 2)) {
        return Ok(root_output(root, fallback));
    }
    if version_at_least(version, (1, 5)) {
        let monitors = connection.randr_get_monitors(root, true)?.reply()?;
        let outputs = monitors
            .monitors
            .into_iter()
            .filter(|monitor| monitor.width > 0 && monitor.height > 0)
            .map(|monitor| Output {
                id: OutputId::new(u64::from(monitor.name)),
                geometry: Geometry::new(
                    i32::from(monitor.x),
                    i32::from(monitor.y),
                    u32::from(monitor.width),
                    u32::from(monitor.height),
                ),
                primary: monitor.primary,
            })
            .collect::<Vec<_>>();
        if !outputs.is_empty() {
            return Ok(OutputSet::new(outputs));
        }
    }

    let primary = if version_at_least(version, (1, 3)) {
        connection.randr_get_output_primary(root)?.reply()?.output
    } else {
        NONE
    };
    let resources = connection.randr_get_screen_resources(root)?.reply()?;
    let mut outputs = Vec::new();
    for crtc in resources.crtcs {
        let info = connection
            .randr_get_crtc_info(crtc, resources.config_timestamp)?
            .reply()?;
        if info.width == 0 || info.height == 0 {
            continue;
        }
        outputs.push(Output {
            id: OutputId::new(u64::from(crtc)),
            geometry: Geometry::new(
                i32::from(info.x),
                i32::from(info.y),
                u32::from(info.width),
                u32::from(info.height),
            ),
            primary: primary != NONE && info.outputs.contains(&primary),
        });
    }
    if outputs.is_empty() {
        Ok(root_output(root, fallback))
    } else {
        Ok(OutputSet::new(outputs))
    }
}

fn root_output(root: Window, geometry: Geometry) -> OutputSet {
    OutputSet::new([Output {
        id: OutputId::new(u64::from(root)),
        geometry,
        primary: true,
    }])
}

fn version_at_least(actual: (u32, u32), required: (u32, u32)) -> bool {
    actual.0 > required.0 || (actual.0 == required.0 && actual.1 >= required.1)
}

fn shape_version_at_least(actual: (u16, u16), required: (u16, u16)) -> bool {
    actual.0 > required.0 || (actual.0 == required.0 && actual.1 >= required.1)
}

fn x11_time_after(candidate: u32, reference: u32) -> bool {
    candidate != reference && candidate.wrapping_sub(reference) < (1_u32 << 31)
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
    /// Distinct configured symbols collapsed into conflicting X11 keycode paths.
    #[error("configured keyboard sequences resolve to conflicting X11 keycode paths")]
    ConflictingKeyGrab,
    /// The keyboard-chain timeout worker could not be started.
    #[error("could not start keyboard-chain timer thread")]
    TimerThread(#[source] std::io::Error),
    /// The keyboard-chain timeout worker stopped unexpectedly.
    #[error("keyboard-chain timer is unavailable")]
    TimerChannel,
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
    fn colormap_windows_implicitly_prioritize_the_top_level() {
        assert_eq!(prioritized_colormap_windows(10, &[]), vec![10]);
        assert_eq!(
            prioritized_colormap_windows(10, &[20, 30]),
            vec![10, 20, 30]
        );
        assert_eq!(
            prioritized_colormap_windows(10, &[20, 10, 30]),
            vec![20, 10, 30]
        );
    }

    #[test]
    fn colormap_windows_are_deduplicated_and_bounded() {
        let mut listed = vec![NONE, 20, 20];
        listed.extend(30..400);
        let windows = prioritized_colormap_windows(10, &listed);

        assert_eq!(windows.len(), MAX_CLIENT_COLORMAP_WINDOWS);
        assert_eq!(&windows[..3], &[10, 20, 30]);
        assert_eq!(windows.iter().filter(|window| **window == 20).count(), 1);
        assert!(!windows.contains(&NONE));
    }

    #[test]
    fn x11_root_struts_translate_to_each_output_edge() {
        let root = Geometry::new(0, 0, 1600, 600);
        let reservations = EdgeReservations {
            left: EdgeReservation {
                depth: 40,
                start: 0,
                end: 599,
            },
            right: EdgeReservation {
                depth: 50,
                start: 0,
                end: 599,
            },
            top: EdgeReservation {
                depth: 30,
                start: 0,
                end: 799,
            },
            ..EdgeReservations::default()
        };
        let left = Geometry::new(0, 0, 800, 600);
        let right = Geometry::new(800, 0, 800, 600);

        assert_eq!(
            left.work_area([output_reservations(reservations, left, root)]),
            Geometry::new(40, 30, 760, 570)
        );
        assert_eq!(
            right.work_area([output_reservations(reservations, right, root)]),
            Geometry::new(800, 0, 750, 600)
        );
    }

    #[test]
    fn focus_switcher_geometry_centers_and_scrolls_selection() {
        assert_eq!(centered_axis(100, 800, 420), 290);
        assert_eq!(centered_axis(-800, 800, 420), -610);
        assert_eq!(focus_cycle_visible_start(3, 2, 8), 0);
        assert_eq!(focus_cycle_visible_start(10, 0, 4), 0);
        assert_eq!(focus_cycle_visible_start(10, 5, 4), 3);
        assert_eq!(focus_cycle_visible_start(10, 9, 4), 6);
        assert_eq!(focus_cycle_visible_start(10, 9, 0), 0);
    }

    #[test]
    fn menu_navigation_skips_separators_and_wraps() {
        let configured = [
            MenuEntry::Separator { label: None },
            MenuEntry::Item {
                label: "_one".to_owned(),
                actions: vec![Action::Exit],
            },
            MenuEntry::Separator {
                label: Some("group".to_owned()),
            },
            MenuEntry::Submenu {
                label: "two".to_owned(),
                menu: "other".to_owned(),
            },
        ];
        let entries = configured
            .iter()
            .map(runtime_configured_entry)
            .collect::<Vec<_>>();
        assert_eq!(first_selectable_menu_entry(&entries), Some(1));
        assert_eq!(last_selectable_menu_entry(&entries), Some(3));
        assert_eq!(next_selectable_menu_entry(&entries, 1, true), Some(3));
        assert_eq!(next_selectable_menu_entry(&entries, 3, true), Some(1));
        assert_eq!(next_selectable_menu_entry(&entries, 1, false), Some(3));
        assert_eq!(
            menu_label("_Open __ terminal"),
            ("Open _ terminal".to_owned(), Some('o'))
        );
        assert_eq!(accelerator_menu_entry(&entries, 3, 'o'), Some((1, 1)));
        assert_eq!(place_popup_axis(790, 0, 800, 260), 530);
        assert_eq!(place_popup_axis(-900, -800, 800, 260), -800);
    }

    #[test]
    fn randr_versions_compare_lexicographically() {
        assert!(version_at_least((1, 5), (1, 5)));
        assert!(version_at_least((1, 6), (1, 5)));
        assert!(!version_at_least((1, 4), (1, 5)));
        assert!(!version_at_least((0, 9), (1, 0)));
        assert!(shape_version_at_least((1, 1), (1, 1)));
        assert!(!shape_version_at_least((1, 0), (1, 1)));
        assert!(x11_time_after(101, 100));
        assert!(!x11_time_after(100, 100));
        assert!(x11_time_after(5, u32::MAX - 5));
        assert!(!x11_time_after(u32::MAX - 5, 5));
    }

    #[test]
    fn fullscreen_monitor_edges_form_a_valid_spanning_rectangle() {
        let outputs = OutputSet::new([
            Output {
                id: OutputId::new(1),
                geometry: Geometry::new(-1280, 0, 1280, 1024),
                primary: false,
            },
            Output {
                id: OutputId::new(2),
                geometry: Geometry::new(0, -200, 1920, 1080),
                primary: true,
            },
            Output {
                id: OutputId::new(3),
                geometry: Geometry::new(1920, 100, 1600, 900),
                primary: false,
            },
        ]);
        assert_eq!(
            fullscreen_monitor_geometry(
                &outputs,
                FullscreenMonitorIndices {
                    top: 1,
                    bottom: 0,
                    left: 0,
                    right: 2,
                }
            ),
            Some(Geometry::new(-1280, -200, 4800, 1224))
        );
        assert!(
            fullscreen_monitor_geometry(
                &outputs,
                FullscreenMonitorIndices {
                    top: 3,
                    bottom: 0,
                    left: 0,
                    right: 2,
                }
            )
            .is_none()
        );
        assert!(
            fullscreen_monitor_geometry(
                &outputs,
                FullscreenMonitorIndices {
                    top: 0,
                    bottom: 1,
                    left: 2,
                    right: 0,
                }
            )
            .is_none()
        );
    }

    #[test]
    fn wm_class_fields_are_bounded_and_tolerate_missing_data() {
        assert_eq!(
            parse_wm_class(b"terminal\0XTerm\0ignored"),
            ("terminal".to_owned(), "XTerm".to_owned())
        );
        assert_eq!(
            parse_wm_class(b"terminal"),
            ("terminal".to_owned(), String::new())
        );
        assert_eq!(parse_wm_class(&[]), (String::new(), String::new()));
    }

    #[test]
    fn application_rules_translate_without_x11_in_the_config_model() {
        assert_eq!(
            application_kind(ClientRole::DropdownMenu),
            ApplicationKind::DropdownMenu
        );
        assert_eq!(
            application_layer(ApplicationLayer::Above),
            ClientLayer::Above
        );

        let motif_limited = apply_motif_hints(
            ClientPolicy::for_role(ClientRole::Normal),
            Some(MotifHints {
                flags: MOTIF_FLAG_DECORATIONS,
                functions: 0,
                decorations: 0,
            }),
        );
        assert!(!motif_limited.decorations.titlebar);
        let forced = apply_application_decorations(motif_limited, Some(true));
        assert!(forced.decorations.titlebar);
        assert!(forced.decorations.close);
        let hidden = apply_application_decorations(forced, Some(false));
        assert_eq!(
            hidden.decorations.extents(2, 24),
            DecorationExtents::default()
        );
    }

    #[test]
    fn edge_resizing_preserves_opposite_anchor_and_snaps() {
        let initial = Geometry::new(100, 100, 200, 120);
        let bounds = Geometry::new(0, 0, 800, 600);
        assert_eq!(
            resize_from_edges(
                initial,
                ResizeEdges::new(true, false, true, false),
                -97,
                -98,
                bounds,
                5,
            ),
            Geometry::new(0, 0, 300, 220)
        );
        assert_eq!(
            resize_from_edges(initial, ResizeEdges::bottom_right(), -500, -500, bounds, 0,),
            Geometry::new(100, 100, 1, 1)
        );
        assert_eq!(
            mouse_context_chain(MouseContext::BottomRight),
            [
                MouseContext::BottomRight,
                MouseContext::Border,
                MouseContext::Frame,
            ]
        );
    }

    #[test]
    fn fixed_size_hints_remove_resize_and_maximize_capabilities() {
        let fixed = Size::new(640, 480);
        let policy = apply_size_capabilities(
            ClientPolicy::for_role(ClientRole::Normal),
            SizeHints {
                minimum: Some(fixed),
                maximum: Some(fixed),
                ..SizeHints::default()
            },
        );
        assert!(!policy.capabilities.resizable);
        assert!(!policy.capabilities.maximizable);
        assert!(!policy.decorations.maximize);

        let vertically_resizable = apply_size_capabilities(
            ClientPolicy::for_role(ClientRole::Normal),
            SizeHints {
                minimum: Some(fixed),
                maximum: Some(Size::new(640, 600)),
                ..SizeHints::default()
            },
        );
        assert!(vertically_resizable.capabilities.resizable);
        assert!(vertically_resizable.capabilities.maximizable);
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
    fn temporary_x11_grabs_do_not_change_policy_focus() {
        assert!(focus_mode_changes_ownership(NotifyMode::NORMAL));
        assert!(focus_mode_changes_ownership(NotifyMode::WHILE_GRABBED));
        assert!(!focus_mode_changes_ownership(NotifyMode::GRAB));
        assert!(!focus_mode_changes_ownership(NotifyMode::UNGRAB));
    }

    #[test]
    fn legacy_output_coverage_requires_exact_undecorated_unmanaged_geometry() {
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
        let root = Geometry::new(0, 0, 1600, 600);
        let undecorated =
            apply_application_decorations(ClientPolicy::for_role(ClientRole::Normal), Some(false));
        assert_eq!(
            legacy_output_coverage(
                Geometry::new(800, 0, 800, 600),
                undecorated,
                false,
                false,
                &outputs,
                root,
            )
            .map(OutputCoverage::output),
            Some(OutputId::new(20))
        );
        assert_eq!(
            legacy_output_coverage(root, undecorated, false, false, &outputs, root)
                .map(OutputCoverage::output),
            Some(OutputId::new(10))
        );
        assert!(
            legacy_output_coverage(
                Geometry::new(0, 0, 799, 600),
                undecorated,
                false,
                false,
                &outputs,
                root,
            )
            .is_none()
        );
        assert!(
            legacy_output_coverage(
                Geometry::new(0, 0, 800, 600),
                ClientPolicy::for_role(ClientRole::Normal),
                false,
                false,
                &outputs,
                root,
            )
            .is_none()
        );
        assert!(
            legacy_output_coverage(
                Geometry::new(0, 0, 800, 600),
                undecorated,
                true,
                false,
                &outputs,
                root,
            )
            .is_none()
        );
        assert!(
            legacy_output_coverage(
                Geometry::new(0, 0, 800, 600),
                undecorated,
                false,
                true,
                &outputs,
                root,
            )
            .is_none()
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
    fn showing_desktop_requests_accept_only_ewmh_booleans() {
        assert_eq!(showing_desktop_request(0), Some(false));
        assert_eq!(showing_desktop_request(1), Some(true));
        assert_eq!(showing_desktop_request(2), None);
        assert_eq!(showing_desktop_request(u32::MAX), None);
    }

    #[test]
    fn client_icons_are_bounded_and_selected_near_the_preferred_size() {
        let mut values = vec![16, 16];
        values.extend(std::iter::repeat_n(0xff11_2233, 16 * 16));
        values.extend([32, 24]);
        values.extend(std::iter::repeat_n(0xff44_5566, 32 * 24));
        values.extend([48, 48]);
        values.extend(std::iter::repeat_n(0xff77_8899, 48 * 48));

        let icon = parse_client_icon(&values, 32).expect("valid closest icon");
        assert_eq!((icon.width, icon.height), (32, 24));
        assert_eq!(icon.argb.len(), 32 * 24);
        assert!(icon.argb.iter().all(|pixel| *pixel == 0xff44_5566));
    }

    #[test]
    fn client_icons_reject_zero_oversized_overflowing_and_truncated_entries() {
        assert!(parse_client_icon(&[0, 32], 32).is_none());
        assert!(parse_client_icon(&[257, 1], 32).is_none());
        assert!(parse_client_icon(&[u32::MAX, u32::MAX], 32).is_none());
        assert!(parse_client_icon(&[32, 32, 0xff00_0000], 32).is_none());

        let mut oversized_then_valid = vec![257, 1];
        oversized_then_valid.extend(std::iter::repeat_n(0, 257));
        oversized_then_valid.extend([1, 1, 0xffab_cdef]);
        let icon = parse_client_icon(&oversized_then_valid, 32).expect("later bounded icon");
        assert_eq!((icon.width, icon.height), (1, 1));
        assert_eq!(icon.argb, [0xffab_cdef]);
    }

    #[test]
    fn ewmh_moveresize_gravity_and_signed_coordinates_are_validated() {
        assert_eq!(ewmh_gravity(1), Some(Gravity::NorthWest));
        assert_eq!(ewmh_gravity(9), Some(Gravity::SouthEast));
        assert_eq!(ewmh_gravity(10), Some(Gravity::Static));
        assert_eq!(ewmh_gravity(0), None);
        assert_eq!(ewmh_gravity(11), None);
        assert_eq!(signed_cardinal(u32::MAX), -1);
        assert_eq!(signed_cardinal(0x8000_0000), i32::MIN);
    }

    #[test]
    fn ewmh_interactive_moveresize_directions_are_strict_and_typed() {
        assert_eq!(
            net_wm_moveresize_request(0),
            Some(NetWmMoveResizeRequest::Resize(ResizeEdges::new(
                true, false, true, false
            )))
        );
        assert_eq!(
            net_wm_moveresize_request(4),
            Some(NetWmMoveResizeRequest::Resize(ResizeEdges::bottom_right()))
        );
        assert_eq!(
            net_wm_moveresize_request(8),
            Some(NetWmMoveResizeRequest::Move)
        );
        assert_eq!(
            net_wm_moveresize_request(9),
            Some(NetWmMoveResizeRequest::ResizeKeyboard)
        );
        assert_eq!(
            net_wm_moveresize_request(10),
            Some(NetWmMoveResizeRequest::MoveKeyboard)
        );
        assert_eq!(
            net_wm_moveresize_request(11),
            Some(NetWmMoveResizeRequest::Cancel)
        );
        assert_eq!(net_wm_moveresize_request(12), None);
        assert_eq!(net_wm_moveresize_request(u32::MAX), None);
    }

    #[test]
    fn keyboard_move_steps_and_jumps_stay_inside_work_area() {
        let bounds = Geometry::new(10, 20, 300, 200);
        let initial = Geometry::new(50, 60, 100, 80);
        assert_eq!(
            keyboard_move_geometry(initial, bounds, KeyboardDragDirection::Right, 8, false),
            Geometry::new(58, 60, 100, 80)
        );
        assert_eq!(
            keyboard_move_geometry(initial, bounds, KeyboardDragDirection::Down, 1, false),
            Geometry::new(50, 61, 100, 80)
        );
        assert_eq!(
            keyboard_move_geometry(initial, bounds, KeyboardDragDirection::Right, 8, true),
            Geometry::new(210, 60, 100, 80)
        );
        assert_eq!(
            keyboard_move_geometry(initial, bounds, KeyboardDragDirection::Up, 8, true),
            Geometry::new(50, 20, 100, 80)
        );
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
            runtime_request(CONTROL_RELOAD, 0, 0),
            Some(RuntimeRequest::Reload)
        );
        assert_eq!(
            runtime_request(CONTROL_SHUTDOWN, 0, 0),
            Some(RuntimeRequest::Shutdown)
        );
        assert_eq!(
            runtime_request(CONTROL_KEY_CHAIN_TIMEOUT, 42, 0),
            Some(RuntimeRequest::KeyChainTimeout(42))
        );
        assert_eq!(
            runtime_request(CONTROL_PING_TIMEOUT, 0x1234, 7),
            Some(RuntimeRequest::PingTimeout {
                client: client_id(0x1234),
                generation: 7,
            })
        );
        assert_eq!(
            runtime_request(CONTROL_SYNC_RESIZE_TIMEOUT, 0x5678, 9),
            Some(RuntimeRequest::SyncResizeTimeout {
                client: client_id(0x5678),
                generation: 9,
            })
        );
        assert_eq!(runtime_request(0, 0, 0), None);
        assert_eq!(runtime_request(u32::MAX, 0, 0), None);
    }

    #[test]
    fn synchronized_resize_values_round_trip_positive_sequences() {
        for value in [0, 1, u64::from(u32::MAX), 1_u64 << 32, i64::MAX as u64] {
            assert_eq!(sync_value_u64(sync_value(value)), Some(value));
        }
        assert_eq!(sync_value_u64(SyncInt64 { hi: -1, lo: 0 }), None);
    }

    #[test]
    fn key_binding_tree_expands_keycodes_and_rejects_prefix_collisions() {
        let mut tree = KeyBindingNode::default();
        insert_key_binding_variants(
            &mut tree,
            &[vec![(10, 1), (11, 1)], vec![(20, 0)]],
            &[Action::Close, Action::NextWorkspace],
        )
        .expect("valid keycode variants");
        assert_eq!(tree.children.len(), 2);
        for prefix in [(10, 1), (11, 1)] {
            assert_eq!(
                tree.children[&prefix].children[&(20, 0)].actions,
                [Action::Close, Action::NextWorkspace]
            );
        }

        assert!(matches!(
            insert_key_binding_variants(&mut tree, &[vec![(10, 1)]], &[Action::Exit]),
            Err(X11Error::ConflictingKeyGrab)
        ));
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
